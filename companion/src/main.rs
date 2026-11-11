//! MapCast companion: reads the addon's strip from the WoW window and serves the live map page
//! to phones and tablets on the same network.

mod blp;
mod capture;
mod casc;
mod discovery;
mod hub;
mod icons;
mod imaging;
mod luadata;
mod maps;
mod openmap;
mod places;
mod protocol;
mod qr;
mod shade;
mod sim;
mod util;
mod web;

#[cfg(test)]
mod tests;

use capture::{Source, DEFAULT_REGION};
use hub::Hub;
use serde_json::json;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

const STALE_AFTER: Duration = Duration::from_secs(2);

const HELP: &str = "MapCast companion: shows your WoW map live on a phone or tablet.

Usage: mapcast [options]

  --wow PATH       WoW install folder (the one with .build.info); found by itself
  --openmap PATH   OpenMap addon folder, if not in the game's AddOns
  --addons PATH    the game's AddOns folder, for map addons' pin icons (found by itself)
  --port N         web server port (default 8765)
  --fps N          screen captures per second (default 30)
  --sim            simulate the game, no WoW needed
  --no-browser     do not open the QR page on this PC
  -v, --verbose    detailed log
";

#[derive(Default)]
struct Args {
    wow: Option<String>,
    openmap: Option<String>,
    addons: Option<String>,
    port: u16,
    fps: u32,
    sim: bool,
    no_browser: bool,
    verbose: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args { port: 8765, fps: 30, ..Default::default() };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut value = |name: &str| it.next().ok_or(format!("{name} needs a value"));
        match a.as_str() {
            "--wow" => args.wow = Some(value("--wow")?),
            "--openmap" => args.openmap = Some(value("--openmap")?),
            "--addons" => args.addons = Some(value("--addons")?),
            "--port" => args.port = value("--port")?.parse().map_err(|_| "--port needs a number")?,
            "--fps" => args.fps = value("--fps")?.parse().map_err(|_| "--fps needs a number")?,
            "--sim" => args.sim = true,
            "--no-browser" => args.no_browser = true,
            "-v" | "--verbose" => args.verbose = true,
            "-h" | "--help" => {
                print!("{HELP}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown option {other}")),
        }
    }
    args.fps = args.fps.clamp(1, 120);
    Ok(args)
}

/// Grabs frames, decodes pages and hands events to the hub.
fn run_pipeline(mut source: Box<dyn Source>, hub: Arc<Hub>, fps: u32) {
    let period = Duration::from_secs_f64(1.0 / fps as f64);
    let mut decoder = protocol::Decoder::new();
    let mut next_status = Instant::now();
    let mut pages_at_status = 0;
    loop {
        let started = Instant::now();
        if let Some(frame) = source.grab() {
            for (name, data) in decoder.feed(&frame) {
                hub.publish(&name, data);
            }
            match decoder.geometry {
                Some(g) => {
                    let (w, h) = g.size_px();
                    source.resize(w, h);
                }
                None => source.resize(DEFAULT_REGION.0, DEFAULT_REGION.1),
            }
        }
        let now = Instant::now();
        if now >= next_status {
            let streaming = decoder.last_page_time.is_some_and(|t| now.duration_since(t) < STALE_AFTER);
            let problem = if !source.window_found() {
                "WoW window not found".to_string()
            } else if !streaming {
                if decoder.problem.is_empty() { "no new data from the addon".to_string() } else { decoder.problem.clone() }
            } else {
                String::new()
            };
            hub.publish("status", json!({"window": source.window_found(), "streaming": streaming, "problem": problem,
                                         "pagesPerSec": decoder.pages - pages_at_status, "badPages": decoder.bad_pages}));
            next_status = now + Duration::from_secs(1);
            pages_at_status = decoder.pages;
        }
        std::thread::sleep(period.saturating_sub(started.elapsed()));
    }
}

fn open_casc(root: Option<&PathBuf>) -> Option<Arc<casc::LocalCasc>> {
    let Some(root) = root else {
        log::warn!("WoW folder not found: maps will be grid placeholders (use --wow PATH)");
        return None;
    };
    let product = casc::flavor_product(&root.join(util::FLAVOR_DIR));
    match casc::LocalCasc::open(root, product.as_deref()) {
        Ok(c) => {
            log::info!("game data: {} {}", c.product, c.version);
            Some(Arc::new(c))
        }
        Err(e) => {
            log::warn!("cannot read game data in {}: {e}", root.display());
            None
        }
    }
}

fn open_openmap(root: Option<&PathBuf>, explicit: Option<&str>, cache: &std::path::Path) -> Option<Arc<openmap::OpenMapArt>> {
    let Some(path) = openmap::find_openmap(root.map(|p| p.as_path()), explicit) else {
        log::info!("OpenMap not found: the phone shows one zone at a time (use --openmap PATH)");
        return None;
    };
    match openmap::OpenMapArt::open(&path, cache) {
        Ok(art) => {
            log::info!("OpenMap {}: seamless world map from {}", if art.version.is_empty() { "?" } else { &art.version }, path.display());
            Some(Arc::new(art))
        }
        Err(e) => {
            log::warn!("cannot read OpenMap in {}: {e}", path.display());
            None
        }
    }
}

fn print_banner(url: &str, token: &str) {
    let link = format!("{url}/?t={token}");
    println!("\n  MapCast: open {link}\n  token: {token}\n");
    print!("{}", qr::terminal(&link));
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}\n\n{HELP}");
            std::process::exit(2);
        }
    };
    util::init_logging(args.verbose);

    let token = util::load_token();
    let url = format!("http://{}:{}", util::lan_ip(), args.port);
    let cache_dir = util::data_dir("cache");
    let maps = Arc::new(maps::MapImages::new(&cache_dir));
    let world = Arc::new(openmap::WorldImages::new(&cache_dir, maps.clone()));
    let icons = Arc::new(icons::PinIcons::new(&cache_dir, maps.clone()));
    let shades = Arc::new(shade::ZoneShades::new(&cache_dir));
    let hub = Arc::new(Hub::new(maps.clone(), world.clone(), icons.clone(), shades));

    let source: Box<dyn Source> = if args.sim {
        match sim::SimSource::new() {
            Ok(s) => Box::new(s),
            Err(e) => {
                eprintln!("simulator: {e}");
                std::process::exit(1);
            }
        }
    } else {
        Box::new(capture::ScreenSource::new())
    };
    let pipeline_hub = hub.clone();
    let fps = args.fps;
    std::thread::Builder::new().name("capture".into()).spawn(move || run_pipeline(source, pipeline_hub, fps)).ok();

    // game data and OpenMap load in the background; the page works meanwhile
    let (sim, wow, om_path, addons) = (args.sim, args.wow.clone(), args.openmap.clone(), args.addons.clone());
    let loader_hub = hub.clone();
    std::thread::Builder::new().name("game-data".into()).spawn(move || {
        let root = if sim { None } else { util::find_wow_root(wow.as_deref()) };
        if !sim {
            *maps.casc.write().unwrap() = open_casc(root.as_ref()).map(|c| c as Arc<dyn maps::GameFiles>);
        }
        let om = open_openmap(root.as_ref(), om_path.as_deref(), &cache_dir);
        *world.openmap.write().unwrap() = om.clone();
        *icons.addons_dir.write().unwrap() = addons.map(PathBuf::from)
            .or_else(|| root.as_ref().map(|r| r.join(util::FLAVOR_DIR).join("Interface").join("AddOns")));
        loader_hub.refresh();
        if let Some(om) = om {
            let started = Instant::now();
            om.warm(); // builds the zone tiles once; later runs read them from the cache
            log::debug!("zone tiles ready in {:.1}s", started.elapsed().as_secs_f64());
        }
    }).ok();

    discovery::serve(args.port, url.clone()); // the phone app finds this PC by itself
    let app = web::App { hub, token: token.clone(), public_url: url.clone() };
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("runtime");
    runtime.block_on(async move {
        let addr = SocketAddr::from(([0, 0, 0, 0], args.port));
        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                eprintln!("cannot listen on port {}: {e} (is another MapCast running? try --port)", args.port);
                std::process::exit(1);
            }
        };
        print_banner(&url, &token);
        if !args.no_browser {
            let _ = std::process::Command::new("cmd")
                .args(["/C", "start", "", &format!("http://127.0.0.1:{}/qr", args.port)])
                .spawn();
        }
        let service = web::router(app).into_make_service_with_connect_info::<SocketAddr>();
        if let Err(e) = axum::serve(listener, service).await {
            eprintln!("web server: {e}");
        }
    });
}
