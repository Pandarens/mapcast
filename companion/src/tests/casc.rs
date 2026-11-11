//! CASC reader against a tiny synthetic local storage, plus map composition.

use crate::casc::{blte_decode, LocalCasc};
use crate::maps::{GameFiles, MapImages};
use crate::util::random_bytes;
use flate2::write::ZlibEncoder;
use md5::{Digest, Md5};
use serde_json::json;
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

fn md5(d: &[u8]) -> [u8; 16] {
    Md5::digest(d).into()
}

fn blte_single(data: &[u8]) -> Vec<u8> {
    let mut out = b"BLTE".to_vec();
    out.extend(0u32.to_be_bytes());
    out.push(b'N');
    out.extend(data);
    out
}

fn blte_chunked(data: &[u8]) -> Vec<u8> {
    let (a, b) = data.split_at(data.len() / 2);
    let mut z = ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(a).unwrap();
    let mut c1 = vec![b'Z'];
    c1.extend(z.finish().unwrap());
    let mut c2 = vec![b'N'];
    c2.extend(b);
    let header_size = (12 + 24 * 2) as u32;
    let mut out = b"BLTE".to_vec();
    out.extend(header_size.to_be_bytes());
    out.extend([0x0F, 0, 0, 2]);
    for (c, p) in [(&c1, a), (&c2, b)] {
        out.extend((c.len() as u32).to_be_bytes());
        out.extend((p.len() as u32).to_be_bytes());
        out.extend(md5(c));
    }
    out.extend(&c1);
    out.extend(&c2);
    out
}

#[test]
fn blte_modes() {
    let payload = random_bytes(5000);
    assert_eq!(blte_decode(&blte_single(&payload)).unwrap(), payload);
    assert_eq!(blte_decode(&blte_chunked(&payload)).unwrap(), payload);
}

fn bucket(ekey: &[u8]) -> u8 {
    let h = ekey.iter().take(9).fold(0u8, |h, b| h ^ b);
    (h & 0xF) ^ (h >> 4)
}

/// Write .build.info, config, encoding, root, one data archive and index files.
fn build_storage(root: &Path, files: &BTreeMap<u32, Vec<u8>>) {
    let data_dir = root.join("Data").join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let mut archive: Vec<u8> = Vec::new();
    let mut index: HashMap<u8, Vec<(Vec<u8>, usize, usize)>> = HashMap::new();
    let mut store = |content: &[u8], chunked: bool, archive: &mut Vec<u8>| -> Vec<u8> {
        let ekey = random_bytes(16);
        let blte = if chunked { blte_chunked(content) } else { blte_single(content) };
        let mut entry: Vec<u8> = ekey.iter().rev().copied().collect();
        entry.extend(((30 + blte.len()) as u32).to_le_bytes());
        entry.extend([0u8; 10]);
        entry.extend(&blte);
        index.entry(bucket(&ekey)).or_default().push((ekey[..9].to_vec(), archive.len(), entry.len()));
        archive.extend(entry);
        ekey
    };
    let mut ckey_to_ekey: BTreeMap<[u8; 16], Vec<u8>> = BTreeMap::new();
    for (id, content) in files {
        ckey_to_ekey.insert(md5(content), store(content, id % 2 == 0, &mut archive));
    }
    // root: MFST v1, one block, enUS
    let ids: Vec<u32> = files.keys().copied().collect();
    let mut root_file = b"TSFM".to_vec();
    root_file.extend((ids.len() as u32).to_le_bytes());
    root_file.extend((ids.len() as u32).to_le_bytes());
    root_file.extend((ids.len() as u32).to_le_bytes());
    root_file.extend(0u32.to_le_bytes());
    root_file.extend(2u32.to_le_bytes());
    let mut prev: i64 = -1;
    for id in &ids {
        root_file.extend(((*id as i64 - prev - 1) as i32).to_le_bytes());
        prev = *id as i64;
    }
    for id in &ids {
        root_file.extend(md5(&files[id]));
    }
    root_file.extend(vec![0u8; 8 * ids.len()]);
    let root_ckey = md5(&root_file);
    let root_ekey = store(&root_file, false, &mut archive);
    ckey_to_ekey.insert(root_ckey, root_ekey);
    // encoding: one 4 KB CE page
    let mut page = Vec::new();
    for (ckey, ekey) in &ckey_to_ekey {
        page.push(1);
        page.extend(&123u64.to_be_bytes()[3..]);
        page.extend(ckey);
        page.extend(ekey);
    }
    page.resize(4096, 0);
    let first_key = *ckey_to_ekey.keys().next().unwrap();
    let mut encoding = b"EN".to_vec();
    encoding.extend([1, 16, 16]);
    encoding.extend(4u16.to_be_bytes());
    encoding.extend(4u16.to_be_bytes());
    encoding.extend(1u32.to_be_bytes());
    encoding.extend(0u32.to_be_bytes());
    encoding.push(0);
    encoding.extend(0u32.to_be_bytes());
    encoding.extend(first_key);
    encoding.extend(md5(&page));
    encoding.extend(&page);
    let encoding_ekey = store(&encoding, false, &mut archive);

    std::fs::write(data_dir.join("data.000"), &archive).unwrap();
    for (b, entries) in index {
        let mut body = Vec::new();
        for (k, off, size) in entries {
            body.extend(k);
            body.extend(&(off as u64).to_be_bytes()[3..]);
            body.extend((size as u32).to_le_bytes());
        }
        let mut header = 0x10u32.to_le_bytes().to_vec();
        header.extend(0u32.to_le_bytes());
        header.extend(7u16.to_le_bytes());
        header.extend([b, 0, 4, 5, 9, 30]);
        header.extend((1u64 << 30).to_le_bytes());
        header.resize(32, 0);
        header.extend((body.len() as u32).to_le_bytes());
        header.extend(0u32.to_le_bytes());
        header.extend(body);
        std::fs::write(data_dir.join(format!("{b:02x}00000001.idx")), header).unwrap();
    }
    let build_key = "ab".repeat(16);
    let config_dir = root.join("Data").join("config").join(&build_key[..2]).join(&build_key[2..4]);
    std::fs::create_dir_all(&config_dir).unwrap();
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    std::fs::write(config_dir.join(&build_key), format!("# Build Configuration\nroot = {}\nencoding = {} {}\n",
                                                        hex(&root_ckey), "00".repeat(16), hex(&encoding_ekey))).unwrap();
    std::fs::write(root.join(".build.info"), format!(
        "Branch!STRING:0|Active!DEC:1|Build Key!HEX:16|Product!STRING:0|Version!STRING:0\neu|1|{}|wow|12.0.5.1\neu|1|{build_key}|wow_forever|1.15.8.1\n",
        "cd".repeat(16))).unwrap();
}

pub fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("mapcast-test-{name}-{}", hex8()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn hex8() -> String {
    random_bytes(4).iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn local_casc_reads_files_by_file_data_id() {
    let dir = temp_dir("casc");
    let files = BTreeMap::from([(100, b"hello".to_vec()), (101, random_bytes(3000)), (5000, vec![b'x'; 10000])]);
    build_storage(&dir, &files);
    let casc = LocalCasc::open(&dir, None).unwrap();
    assert_eq!(casc.product, "wow_forever");
    for (id, content) in &files {
        assert_eq!(&casc.read_file(*id).unwrap(), content);
    }
    assert!(casc.read_file(4242).is_err());
}

struct FakeFiles(HashMap<u32, Vec<u8>>);

impl GameFiles for FakeFiles {
    fn read_file(&self, id: u32) -> Result<Vec<u8>, String> {
        self.0.get(&id).cloned().ok_or("missing".into())
    }
}

fn png(color: [u8; 4], w: u32, h: u32) -> Vec<u8> {
    crate::imaging::png_bytes(&image::RgbaImage::from_pixel(w, h, image::Rgba(color)))
}

#[test]
fn map_composition_tiles_and_explored_overlays() {
    let dir = temp_dir("maps");
    let mut files: HashMap<u32, Vec<u8>> = (1..=12).map(|i| (i, png([(i * 20) as u8, 0, 0, 255], 256, 256))).collect();
    files.insert(50, png([0, 255, 0, 255], 256, 256));
    files.insert(51, png([0, 0, 255, 255], 64, 256)); // last column: 300 - 256 = 44 px used of a 64 px file
    let maps = MapImages::new(&dir);
    *maps.casc.write().unwrap() = Some(Arc::new(FakeFiles(files)));
    let info = json!({"mapID": 1, "mapType": 3, "parentMapID": 0, "name": "Test",
                      "layers": [{"w": 1002, "h": 668, "tileW": 256, "tileH": 256, "tiles": (1..=12).collect::<Vec<_>>()}]});
    let explored = json!([{"w": 300, "h": 200, "x": 400, "y": 300, "flags": 0, "files": [50, 51]}]);
    let desc = maps.register(&json!({"info": info, "explored": explored}));
    assert!(desc["width"] == 1002 && desc["placeholder"] == false);
    let url = desc["url"].as_str().unwrap();
    let image = crate::imaging::load_image(&maps.image_path(&url[5..url.len() - 4]).unwrap()).unwrap();
    assert_eq!(image.dimensions(), (1002, 668));
    let rgb = |x, y| { let p = image.get_pixel(x, y); [p[0], p[1], p[2]] };
    assert_eq!(rgb(10, 10), [20, 0, 0]);
    assert_eq!(rgb(1000, 600), [240, 0, 0]);
    assert_eq!(rgb(500, 350), [0, 255, 0]);
    assert_eq!(rgb(680, 350), [0, 0, 255]);
    assert_ne!(rgb(710, 350), [0, 0, 255]);
}

#[test]
fn placeholder_without_game_data() {
    let maps = MapImages::new(&temp_dir("placeholder"));
    let info = json!({"mapID": 1, "mapType": 3, "parentMapID": 0, "name": "Test",
                      "layers": [{"w": 1002, "h": 668, "tileW": 256, "tileH": 256, "tiles": [1, 2]}]});
    let desc = maps.register(&json!({"info": info, "explored": []}));
    assert_eq!(desc["placeholder"], true);
    let url = desc["url"].as_str().unwrap();
    let img = crate::imaging::load_image(&maps.image_path(&url[5..url.len() - 4]).unwrap()).unwrap();
    assert_eq!(img.dimensions(), (1002, 668));
}
