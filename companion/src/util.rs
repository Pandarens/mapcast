//! Small helpers: logging, data folders, the access token, the LAN address, finding WoW, content keys.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const FLAVOR_DIR: &str = "_forever_";

struct Logger {
    verbose: bool,
}

impl log::Log for Logger {
    fn enabled(&self, meta: &log::Metadata) -> bool {
        meta.level() <= if self.verbose { log::Level::Debug } else { log::Level::Info }
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let (h, m, s) = local_time();
        eprintln!("{h:02}:{m:02}:{s:02} {} {}", record.level(), record.args());
    }

    fn flush(&self) {}
}

#[cfg(windows)]
fn local_time() -> (u16, u16, u16) {
    let mut t = unsafe { std::mem::zeroed::<windows_sys::Win32::Foundation::SYSTEMTIME>() };
    unsafe { windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut t) };
    (t.wHour, t.wMinute, t.wSecond)
}

#[cfg(not(windows))]
fn local_time() -> (u16, u16, u16) {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    (((secs / 3600) % 24) as u16, ((secs / 60) % 60) as u16, (secs % 60) as u16)
}

pub fn init_logging(verbose: bool) {
    let logger = Box::leak(Box::new(Logger { verbose }));
    let _ = log::set_logger(logger);
    log::set_max_level(if verbose { log::LevelFilter::Debug } else { log::LevelFilter::Info });
}

/// %LOCALAPPDATA%\MapCast\cache or %APPDATA%\MapCast\config.
pub fn data_dir(kind: &str) -> PathBuf {
    let var = if kind == "cache" { "LOCALAPPDATA" } else { "APPDATA" };
    let base = std::env::var_os(var).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    let path = base.join("MapCast").join(kind);
    let _ = std::fs::create_dir_all(&path);
    path
}

pub fn random_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    if getrandom::fill(&mut buf).is_err() {
        let t = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(1);
        for (i, b) in buf.iter_mut().enumerate() {
            *b = (t >> ((i % 16) * 8)) as u8 ^ (i as u8).wrapping_mul(151);
        }
    }
    buf
}

/// The token phones need, kept in config.json (the same file the Python companion used).
pub fn load_token() -> String {
    use base64::Engine;
    let path = data_dir("config").join("config.json");
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            if let Some(t) = v.get("token").and_then(|t| t.as_str()).filter(|t| !t.is_empty()) {
                return t.to_string();
            }
        }
    }
    let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random_bytes(6));
    let _ = std::fs::write(&path, serde_json::json!({ "token": token }).to_string());
    token
}

/// The addresses phones on the same network can reach this PC at: those of its network cards
/// that are up and have a gateway (wired or Wi-Fi), best first. VPN and virtual adapters (WSL,
/// Hyper-V, Cloudflare WARP and such) are left out: a phone can't reach them.
pub fn lan_ips() -> Vec<std::net::Ipv4Addr> {
    let mut ips = adapter_ips();
    if ips.is_empty() {
        // the address of the route out (no packets are sent)
        if let Ok(std::net::IpAddr::V4(ip)) = std::net::UdpSocket::bind("0.0.0.0:0")
            .and_then(|s| {
                s.connect("10.255.255.255:1")?;
                s.local_addr()
            })
            .map(|a| a.ip())
        {
            ips.push(ip);
        }
    }
    // home networks first: 192.168.x.x, then 10.x.x.x, then 172.16-31.x.x, then the rest
    ips.sort_by_key(|ip| match ip.octets() {
        [192, 168, ..] => 0,
        [10, ..] => 1,
        [172, b, ..] if (16..32).contains(&b) => 2,
        _ => 3,
    });
    ips.dedup();
    ips
}

pub fn lan_ip() -> String {
    lan_ips().first().map(|ip| ip.to_string()).unwrap_or_else(|| "127.0.0.1".into())
}

#[cfg(windows)]
fn adapter_ips() -> Vec<std::net::Ipv4Addr> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetAdaptersAddresses, GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER,
        GAA_FLAG_SKIP_MULTICAST, IP_ADAPTER_ADDRESSES_LH,
    };
    use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;
    use windows_sys::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN};
    const IF_TYPE_ETHERNET: u32 = 6;
    const IF_TYPE_WIFI: u32 = 71;
    const ERROR_BUFFER_OVERFLOW: u32 = 111;

    let flags = GAA_FLAG_INCLUDE_GATEWAYS | GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
    let mut size: u32 = 32 * 1024;
    let mut buf: Vec<u64> = Vec::new();
    for _ in 0..3 {
        buf = vec![0u64; size as usize / 8 + 1];
        let ret = unsafe {
            GetAdaptersAddresses(AF_INET as u32, flags, std::ptr::null(), buf.as_mut_ptr().cast(), &mut size)
        };
        if ret == 0 {
            break;
        }
        if ret != ERROR_BUFFER_OVERFLOW {
            return vec![];
        }
    }
    let mut out = Vec::new();
    let mut adapter = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
    unsafe {
        while !adapter.is_null() {
            let a = &*adapter;
            let physical = a.IfType == IF_TYPE_ETHERNET || a.IfType == IF_TYPE_WIFI;
            if a.OperStatus == IfOperStatusUp && physical && !a.FirstGatewayAddress.is_null() {
                let mut unicast = a.FirstUnicastAddress;
                while !unicast.is_null() {
                    let sockaddr = (*unicast).Address.lpSockaddr;
                    if !sockaddr.is_null() && (*sockaddr).sa_family == AF_INET {
                        let sin = &*(sockaddr as *const SOCKADDR_IN);
                        out.push(std::net::Ipv4Addr::from(u32::from_be(sin.sin_addr.S_un.S_addr)));
                    }
                    unicast = (*unicast).Next;
                }
            }
            adapter = a.Next;
        }
    }
    out
}

#[cfg(not(windows))]
fn adapter_ips() -> Vec<std::net::Ipv4Addr> {
    vec![]
}

pub fn find_wow_root(explicit: Option<&str>) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(p) = explicit {
        candidates.push(PathBuf::from(p));
    }
    #[cfg(windows)]
    {
        use winreg::enums::HKEY_LOCAL_MACHINE;
        let hklm = winreg::RegKey::predef(HKEY_LOCAL_MACHINE);
        for key in [r"SOFTWARE\WOW6432Node\Blizzard Entertainment\World of Warcraft",
                    r"SOFTWARE\Blizzard Entertainment\World of Warcraft"] {
            if let Ok(k) = hklm.open_subkey(key) {
                if let Ok(path) = k.get_value::<String, _>("InstallPath") {
                    candidates.push(PathBuf::from(path));
                }
            }
        }
    }
    for drive in "CDEFGH".chars() {
        for sub in [r"Program Files (x86)\World of Warcraft", r"Program Files\World of Warcraft",
                    r"World of Warcraft", r"Games\World of Warcraft"] {
            candidates.push(PathBuf::from(format!("{drive}:\\{sub}")));
        }
    }
    for path in candidates {
        let mut path = std::path::absolute(&path).unwrap_or(path);
        for _ in 0..3 {
            // InstallPath may point at a flavor folder such as _retail_
            if path.join(".build.info").exists() {
                return Some(path);
            }
            match path.parent() {
                Some(p) => path = p.to_path_buf(),
                None => break,
            }
        }
    }
    None
}

/// A short key for a description of something to build: its content, so a changed spec is a new URL.
pub fn content_key(spec: &Value) -> String {
    let mut h = sha1_smol::Sha1::new();
    h.update(serde_json::to_string(spec).unwrap_or_default().as_bytes());
    h.digest().to_string()[..16].to_string()
}

pub fn hex_key(text: &str, len: usize) -> String {
    let mut h = sha1_smol::Sha1::new();
    h.update(text.as_bytes());
    h.digest().to_string()[..len].to_string()
}

/// Writes a file through a temporary name, so a reader never sees half of it.
pub fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path).or_else(|_| {
        let _ = std::fs::remove_file(path);
        std::fs::rename(&tmp, path)
    })
}

pub fn round_to(v: f64, digits: i32) -> f64 {
    let k = 10f64.powi(digits);
    (v * k).round() / k
}

/// A child of `folder` by name, ignoring case (the game's paths are case-insensitive).
pub fn child_ci(folder: &Path, name: &str, want_dir: bool) -> Option<PathBuf> {
    let exact = folder.join(name);
    if if want_dir { exact.is_dir() } else { exact.is_file() } {
        return Some(exact);
    }
    let lower = name.to_lowercase();
    for entry in std::fs::read_dir(folder).ok()?.flatten() {
        let path = entry.path();
        if entry.file_name().to_string_lossy().to_lowercase() == lower
            && (if want_dir { path.is_dir() } else { path.is_file() })
        {
            return Some(path);
        }
    }
    None
}
