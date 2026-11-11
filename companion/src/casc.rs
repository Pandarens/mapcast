//! Minimal read-only reader for the local CASC storage of an installed WoW client.
//!
//! Only what MapCast needs: read a file by FileDataID (map tiles, icons).
//! Formats: https://wowdev.wiki/CASC and https://wowdev.wiki/TACT

use md5::{Digest, Md5};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const LOCALE_ENUS: u32 = 0x2;
const LOCALE_ENGB: u32 = 0x200;
const LOCALE_RURU: u32 = 0x2000;
const CONTENT_LOW_VIOLENCE: u32 = 0x80;
const CONTENT_NO_NAME_HASH: u32 = 0x1000_0000;
const DATA_HEADER_SIZE: usize = 30;

pub type Result<T> = std::result::Result<T, String>;

/// Parse a Blizzard 'Name!TYPE:n|...' table such as .build.info.
pub fn parse_table(text: &str) -> Vec<HashMap<String, String>> {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#')).collect();
    let Some(first) = lines.first() else { return vec![] };
    let header: Vec<String> = first.split('|').map(|c| c.split('!').next().unwrap_or("").to_string()).collect();
    lines[1..]
        .iter()
        .map(|line| header.iter().cloned().zip(line.split('|').map(str::to_string)).collect())
        .collect()
}

pub fn parse_config(text: &str) -> HashMap<String, Vec<String>> {
    let mut config = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            config.insert(k.trim().to_string(), v.split_whitespace().map(str::to_string).collect());
        }
    }
    config
}

fn be32(d: &[u8], at: usize) -> Result<u32> {
    d.get(at..at + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]])).ok_or("truncated".into())
}

fn le32(d: &[u8], at: usize) -> Result<u32> {
    d.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).ok_or("truncated".into())
}

pub fn blte_decode(data: &[u8]) -> Result<Vec<u8>> {
    if data.get(..4) != Some(b"BLTE") {
        return Err("not a BLTE stream".into());
    }
    let header_size = be32(data, 4)? as usize;
    if header_size == 0 {
        return decode_chunk(&data[8..]);
    }
    let count = u32::from_be_bytes([0, *data.get(9).ok_or("truncated")?, data[10], data[11]]) as usize;
    let mut out = Vec::new();
    let mut pos = header_size;
    for i in 0..count {
        let compressed = be32(data, 12 + 24 * i)? as usize;
        let chunk = data.get(pos..pos + compressed).ok_or("truncated BLTE chunk")?;
        out.extend(decode_chunk(chunk)?);
        pos += compressed;
    }
    Ok(out)
}

fn decode_chunk(chunk: &[u8]) -> Result<Vec<u8>> {
    match chunk.first() {
        Some(b'N') => Ok(chunk[1..].to_vec()),
        Some(b'Z') => {
            let mut out = Vec::new();
            flate2::read::ZlibDecoder::new(&chunk[1..]).read_to_end(&mut out).map_err(|e| e.to_string())?;
            Ok(out)
        }
        Some(b'F') => blte_decode(&chunk[1..]),
        Some(b'E') => Err("file is encrypted".into()),
        other => Err(format!("unsupported BLTE mode {other:?}")),
    }
}

/// One local .idx bucket file: EKey prefix -> (archive, offset, size), sorted for binary search.
struct Index {
    key_len: usize,
    entries: Vec<(Vec<u8>, u32, u64, u32)>,
}

impl Index {
    fn load(path: &Path) -> Result<Index> {
        let data = std::fs::read(path).map_err(|e| e.to_string())?;
        let header_size = le32(&data, 0)? as usize;
        let size_len = *data.get(12).ok_or("truncated")? as usize;
        let offset_len = data[13] as usize;
        let key_len = data[14] as usize;
        let offset_bits = data[15] as u32;
        let mut pos = (8 + header_size + 15) & !15;
        let entries_size = le32(&data, pos)? as usize;
        pos += 8;
        let entry_len = key_len + offset_len + size_len;
        let n = entries_size / entry_len.max(1);
        let mut entries = Vec::with_capacity(n);
        for i in 0..n {
            let e = data.get(pos + i * entry_len..pos + (i + 1) * entry_len).ok_or("truncated index")?;
            let mut location: u64 = 0;
            for b in &e[key_len..key_len + offset_len] {
                location = (location << 8) | *b as u64;
            }
            let mut size: u32 = 0;
            for (k, b) in e[key_len + offset_len..].iter().enumerate() {
                size |= (*b as u32) << (8 * k);
            }
            let archive = (location >> offset_bits) as u32;
            let offset = location & ((1u64 << offset_bits) - 1);
            entries.push((e[..key_len].to_vec(), archive, offset, size));
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0)); // stable: the first of equal keys stays first
        Ok(Index { key_len, entries })
    }

    fn find(&self, ekey: &[u8]) -> Option<(u32, u64, u32)> {
        let key = &ekey[..self.key_len.min(ekey.len())];
        let i = self.entries.partition_point(|e| e.0.as_slice() < key);
        self.entries.get(i).filter(|e| e.0 == key).map(|e| (e.1, e.2, e.3))
    }
}

/// CKey -> EKey lookup over the paged encoding file.
struct Encoding {
    ckey_len: usize,
    ekey_len: usize,
    first_keys: Vec<Vec<u8>>,
    pages_start: usize,
    page_size: usize,
    data: Vec<u8>,
}

impl Encoding {
    fn new(data: Vec<u8>) -> Result<Encoding> {
        if data.get(..2) != Some(b"EN") {
            return Err("bad encoding file".into());
        }
        let (ckey_len, ekey_len) = (data[3] as usize, data[4] as usize);
        let page_kb = u16::from_be_bytes([data[5], data[6]]) as usize;
        let page_count = be32(&data, 9)? as usize;
        let espec_size = be32(&data, 18)? as usize;
        let pos = 22 + espec_size;
        let entry = ckey_len + 16;
        let first_keys = (0..page_count)
            .map(|i| data.get(pos + i * entry..pos + i * entry + ckey_len).map(<[u8]>::to_vec).ok_or("truncated"))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(Encoding { ckey_len, ekey_len, first_keys, pages_start: pos + page_count * entry,
                      page_size: page_kb * 1024, data })
    }

    fn ekey(&self, ckey: &[u8]) -> Option<Vec<u8>> {
        let page = self.first_keys.partition_point(|k| k.as_slice() <= ckey).checked_sub(1)?;
        let (ck, ek, d) = (self.ckey_len, self.ekey_len, &self.data);
        let mut pos = self.pages_start + page * self.page_size;
        let end = pos + self.page_size;
        while pos + 6 + ck <= end.min(d.len()) {
            let count = d[pos] as usize;
            if count == 0 {
                break;
            }
            if &d[pos + 6..pos + 6 + ck] == ckey {
                return d.get(pos + 6 + ck..pos + 6 + ck + ek).map(<[u8]>::to_vec);
            }
            pos += 6 + ck + count * ek;
        }
        None
    }
}

/// FileDataID -> CKey from the TSFM ('MFST') root file.
struct Root {
    ids: Vec<i64>,
    keys: Vec<[u8; 16]>,
}

impl Root {
    fn new(data: &[u8], locale_mask: u32) -> Result<Root> {
        let (mut version, mut pos, mut allow_nameless) = (0u32, 0usize, false);
        if data.get(..4) == Some(b"TSFM") {
            let (first, second) = (le32(data, 4)?, le32(data, 8)?);
            let (total, named);
            if first == 0x18 {
                version = second;
                total = le32(data, 12)?;
                named = le32(data, 16)?;
                pos = first as usize;
            } else {
                total = first;
                named = second;
                pos = 12;
            }
            allow_nameless = total != named;
        }
        let mut pairs: Vec<(i64, usize, [u8; 16])> = Vec::new();
        let mut order = 0usize;
        while pos + 12 <= data.len() {
            let (count, content, locale);
            if version >= 2 {
                count = le32(data, pos)? as usize;
                locale = le32(data, pos + 4)?;
                let (f1, f2) = (le32(data, pos + 8)?, le32(data, pos + 12)?);
                content = f1 | f2 | ((*data.get(pos + 16).ok_or("truncated")? as u32) << 17);
                pos += 17;
            } else {
                count = le32(data, pos)? as usize;
                content = le32(data, pos + 4)?;
                locale = le32(data, pos + 8)?;
                pos += 12;
            }
            let deltas_at = pos;
            pos += 4 * count;
            let keys_at = pos;
            pos += 16 * count;
            if !(allow_nameless && content & CONTENT_NO_NAME_HASH != 0) {
                pos += 8 * count;
            }
            if pos > data.len() {
                break;
            }
            if content & CONTENT_LOW_VIOLENCE != 0 || locale & locale_mask == 0 {
                continue;
            }
            let mut id: i64 = -1;
            for i in 0..count {
                let delta = le32(data, deltas_at + 4 * i)? as i32 as i64;
                id += delta + 1;
                let mut key = [0u8; 16];
                key.copy_from_slice(&data[keys_at + 16 * i..keys_at + 16 * (i + 1)]);
                pairs.push((id, order, key));
                order += 1;
            }
        }
        if pairs.is_empty() {
            return Err("root file has no usable entries".into());
        }
        pairs.sort_by_key(|p| (p.0, p.1)); // earlier blocks win on duplicates
        pairs.dedup_by_key(|p| p.0);
        Ok(Root { ids: pairs.iter().map(|p| p.0).collect(), keys: pairs.iter().map(|p| p.2).collect() })
    }

    fn ckey(&self, file_data_id: i64) -> Option<[u8; 16]> {
        self.ids.binary_search(&file_data_id).ok().map(|i| self.keys[i])
    }
}

/// Read files by FileDataID from an installed client.
pub struct LocalCasc {
    pub product: String,
    pub version: String,
    data_dir: PathBuf,
    indices: HashMap<u8, Index>,
    encoding: Encoding,
    root: Root,
}

impl LocalCasc {
    pub fn open(game_root: &Path, product: Option<&str>) -> Result<LocalCasc> {
        let data_dir = game_root.join("Data");
        let rows = parse_table(&read_text(&game_root.join(".build.info"))?);
        let row = pick_build(&rows, product).ok_or(format!("no build for product {product:?} in .build.info"))?;
        let build_key = row.get("Build Key").ok_or("no Build Key")?;
        if build_key.len() < 4 {
            return Err("bad Build Key".into());
        }
        let config = parse_config(&read_text(
            &data_dir.join("config").join(&build_key[..2]).join(&build_key[2..4]).join(build_key),
        )?);
        let indices = load_indices(&data_dir.join("data"))?;
        let mut casc = LocalCasc {
            product: row.get("Product").cloned().unwrap_or_default(),
            version: row.get("Version").cloned().unwrap_or_default(),
            data_dir,
            indices,
            encoding: Encoding { ckey_len: 16, ekey_len: 16, first_keys: vec![], pages_start: 0, page_size: 0, data: vec![] },
            root: Root { ids: vec![], keys: vec![] },
        };
        let enc_key = config.get("encoding").and_then(|v| v.get(1)).ok_or("no encoding key")?;
        casc.encoding = Encoding::new(casc.read_ekey(&hex(enc_key)?)?)?;
        let root_ckey = hex(config.get("root").and_then(|v| v.first()).ok_or("no root key")?)?;
        let root_ekey = casc.encoding.ekey(&root_ckey).ok_or("root file not found in encoding")?;
        casc.root = Root::new(&casc.read_ekey(&root_ekey)?, LOCALE_ENUS | LOCALE_ENGB | LOCALE_RURU)?;
        Ok(casc)
    }

    fn read_ekey(&self, ekey: &[u8]) -> Result<Vec<u8>> {
        let (archive, offset, size) = self
            .indices
            .get(&bucket(ekey))
            .and_then(|i| i.find(ekey))
            .ok_or_else(|| format!("ekey {} not in local storage", to_hex(ekey)))?;
        let mut f = File::open(self.data_dir.join("data").join(format!("data.{archive:03}"))).map_err(|e| e.to_string())?;
        f.seek(SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
        let mut blob = vec![0u8; size as usize];
        f.read_exact(&mut blob).map_err(|e| e.to_string())?;
        blte_decode(blob.get(DATA_HEADER_SIZE..).ok_or("short data entry")?)
    }

    pub fn read_file(&self, file_data_id: u32) -> Result<Vec<u8>> {
        let ckey = self.root.ckey(file_data_id as i64).ok_or(format!("FileDataID {file_data_id} not in root"))?;
        let ekey = self.encoding.ekey(&ckey).ok_or(format!("FileDataID {file_data_id} not in encoding"))?;
        let data = self.read_ekey(&ekey)?;
        if Md5::digest(&data).as_slice() != ckey {
            return Err(format!("FileDataID {file_data_id} failed its checksum"));
        }
        Ok(data)
    }
}

fn read_text(path: &Path) -> Result<String> {
    std::fs::read(path).map(|b| String::from_utf8_lossy(&b).into_owned()).map_err(|e| format!("{}: {e}", path.display()))
}

fn hex(s: &str) -> Result<Vec<u8>> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).map_err(|e| e.to_string())).collect()
}

fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn pick_build<'a>(rows: &'a [HashMap<String, String>], product: Option<&str>) -> Option<&'a HashMap<String, String>> {
    let active: Vec<&HashMap<String, String>> = rows.iter().filter(|r| r.get("Active").map_or(true, |a| a == "1")).collect();
    let active = if active.is_empty() { rows.iter().collect() } else { active };
    if let Some(p) = product {
        return active.into_iter().find(|r| r.get("Product").map(String::as_str) == Some(p));
    }
    active.iter().find(|r| r.get("Product").is_some_and(|p| p.contains("forever"))).copied().or(active.first().copied())
}

fn bucket(ekey: &[u8]) -> u8 {
    let h = ekey.iter().take(9).fold(0u8, |h, b| h ^ b);
    (h & 0xF) ^ (h >> 4)
}

fn load_indices(dir: &Path) -> Result<HashMap<u8, Index>> {
    let mut latest: HashMap<u8, (u32, PathBuf)> = HashMap::new();
    for entry in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(stem) = name.strip_suffix(".idx") else { continue };
        if stem.len() != 10 {
            continue;
        }
        let (Ok(b), Ok(v)) = (u8::from_str_radix(&stem[..2], 16), u32::from_str_radix(&stem[2..], 16)) else { continue };
        if latest.get(&b).is_none_or(|(old, _)| v > *old) {
            latest.insert(b, (v, entry.path()));
        }
    }
    if latest.is_empty() {
        return Err(format!("no CASC index files in {}", dir.display()));
    }
    latest.into_iter().map(|(b, (_, p))| Index::load(&p).map(|i| (b, i))).collect()
}

/// Product code from a flavor folder such as _forever_/.flavor.info.
pub fn flavor_product(flavor_dir: &Path) -> Option<String> {
    let text = read_text(&flavor_dir.join(".flavor.info")).ok()?;
    let rows = parse_table(&text);
    let first = rows.first()?;
    // the table's only column
    let header = text.lines().find(|l| !l.trim().is_empty() && !l.starts_with('#'))?;
    let name = header.split('|').next()?.split('!').next()?;
    first.get(name).cloned()
}
