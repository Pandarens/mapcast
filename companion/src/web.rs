//! The web server phones connect to: the page, its images and the live data over a WebSocket.

use crate::hub::Hub;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Path as UrlPath, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

const COOKIE: &str = "mapcast";
const INDEX_HTML: &str = include_str!("../web/index.html");
const APP_JS: &str = include_str!("../web/app.js");
const STYLE_CSS: &str = include_str!("../web/style.css");

#[derive(Clone)]
pub struct App {
    pub hub: Arc<Hub>,
    pub token: String,
    pub public_url: String,
}

const LOGIN_PAGE: &str = r#"<!doctype html><html lang="ru"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1"><title>MapCast</title>
<style>body{font:16px system-ui;background:#16130f;color:#eee;display:grid;place-items:center;height:100vh;margin:0}
form{display:grid;gap:12px;padding:16px}input,button{font:inherit;padding:12px;border-radius:8px;border:1px solid #555}
button{background:#c9a24a;border:0;color:#111}</style></head><body>
<form method="get" action="/"><b>MapCast</b><label>Токен из окна компаньона на ПК</label>
<input name="t" autocomplete="off" autofocus><button>Открыть карту</button></form></body></html>"#;

fn query_param(query: Option<&str>, key: &str) -> Option<String> {
    query?.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        (k == key).then(|| percent_decode(v))
    })
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get_all(header::COOKIE).iter().filter_map(|v| v.to_str().ok()).flat_map(|v| v.split(';')).find_map(|c| {
        let (k, v) = c.trim().split_once('=')?;
        (k == name).then(|| v.to_string())
    })
}

fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn auth(State(app): State<App>, ConnectInfo(addr): ConnectInfo<SocketAddr>, req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    if path == "/qr" {
        if !addr.ip().is_loopback() {
            return StatusCode::FORBIDDEN.into_response();
        }
        return next.run(req).await;
    }
    if path.starts_with("/static/") || path == "/favicon.ico" {
        return next.run(req).await;
    }
    let from_query = query_param(req.uri().query(), "t");
    let given = from_query.clone().or_else(|| cookie(req.headers(), COOKIE)).unwrap_or_default();
    if !same(&given, &app.token) {
        if path == "/" {
            return Html(LOGIN_PAGE).into_response();
        }
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut response = next.run(req).await;
    if from_query.is_some() {
        let value = format!("{COOKIE}={}; Max-Age=31536000; Path=/; SameSite=Lax", app.token);
        if let Ok(v) = HeaderValue::from_str(&value) {
            response.headers_mut().append(header::SET_COOKIE, v);
        }
    }
    response
}

fn content_type(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "png" => "image/png",
        "webp" => "image/webp",
        "json" => "application/json",
        _ => "application/octet-stream",
    }
}

/// Serve a cached image, building it off the async threads. URLs change with their content.
async fn cached_file<F>(build: F) -> Response
where
    F: FnOnce() -> Option<PathBuf> + Send + 'static,
{
    let read = tokio::task::spawn_blocking(move || build().and_then(|p| std::fs::read(&p).ok().map(|b| (p, b)))).await;
    match read {
        Ok(Some((path, bytes))) => (
            [(header::CONTENT_TYPE, content_type(&path)), (header::CACHE_CONTROL, "max-age=31536000, immutable")],
            bytes,
        ).into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

fn valid_key(key: &str) -> bool {
    key.len() == 16 && key.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn strip_png(key: &str) -> Option<String> {
    key.strip_suffix(".png").filter(|k| valid_key(k)).map(str::to_string)
}

async fn index() -> Response {
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")], INDEX_HTML).into_response()
}

async fn static_file(UrlPath(file): UrlPath<String>) -> Response {
    let (body, kind) = match file.as_str() {
        "app.js" => (APP_JS, "text/javascript; charset=utf-8"),
        "style.css" => (STYLE_CSS, "text/css; charset=utf-8"),
        "index.html" => (INDEX_HTML, "text/html; charset=utf-8"),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    ([(header::CONTENT_TYPE, kind), (header::CACHE_CONTROL, "no-cache")], body).into_response()
}

async fn qr(State(app): State<App>) -> Response {
    let link = format!("{}/?t={}", app.public_url, app.token);
    let png = crate::qr::png(&link);
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(png);
    Html(format!(
        r#"<!doctype html><html lang="ru"><head><meta charset="utf-8"><title>MapCast</title>
<style>body{{font:18px system-ui;background:#16130f;color:#eee;text-align:center;padding:24px}}
img{{background:#fff;padding:12px;border-radius:12px;image-rendering:pixelated}}code{{font-size:22px;color:#c9a24a}}</style></head><body>
<h2>Отсканируй телефоном или планшетом</h2><img src="data:image/png;base64,{b64}" alt="QR" width="330" height="330">
<p>или открой <code>{url}</code></p><p>токен: <code>{token}</code></p>
<p>Телефон должен быть в той же Wi-Fi сети, что и этот ПК.</p></body></html>"#,
        url = app.public_url, token = app.token
    )).into_response()
}

async fn ws_handler(State(app): State<App>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| serve_socket(app, socket))
}

async fn serve_socket(app: App, mut socket: WebSocket) {
    let mut rx = app.hub.tx.subscribe();
    let snapshot = serde_json::json!({"type": "snapshot", "data": app.hub.snapshot()}).to_string();
    if socket.send(Message::Text(snapshot.into())).await.is_err() {
        return;
    }
    let mut ping = tokio::time::interval(Duration::from_secs(20));
    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Ok(text) => if socket.send(Message::Text(text.into())).await.is_err() { break },
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    // fell behind: start again from the whole state
                    let snapshot = serde_json::json!({"type": "snapshot", "data": app.hub.snapshot()}).to_string();
                    if socket.send(Message::Text(snapshot.into())).await.is_err() { break }
                }
                Err(_) => break,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            },
            _ = ping.tick() => if socket.send(Message::Ping(Vec::new().into())).await.is_err() { break },
        }
    }
}

pub fn router(app: App) -> Router {
    let hub = app.hub.clone();
    let (h1, h2, h3, h4, h5, h6, h7, h8) =
        (hub.clone(), hub.clone(), hub.clone(), hub.clone(), hub.clone(), hub.clone(), hub.clone(), hub);
    Router::new()
        .route("/", get(index))
        .route("/qr", get(qr))
        .route("/ws", get(ws_handler))
        .route("/favicon.ico", get(|| async { StatusCode::NO_CONTENT }))
        .route("/static/{file}", get(static_file))
        .route("/map/{key}", get(move |UrlPath(key): UrlPath<String>| {
            let hub = h1.clone();
            async move {
                let Some(key) = strip_png(&key) else { return StatusCode::NOT_FOUND.into_response() };
                cached_file(move || hub.maps.image_path(&key)).await
            }
        }))
        .route("/world/art/{key}", get(move |UrlPath(key): UrlPath<String>| {
            let hub = h2.clone();
            async move {
                let Some(key) = strip_png(&key) else { return StatusCode::NOT_FOUND.into_response() };
                cached_file(move || hub.world.art_path(&key)).await
            }
        }))
        .route("/world/sea.png", get(move || {
            let hub = h3.clone();
            async move { cached_file(move || hub.world.sea_path()).await }
        }))
        .route("/world/places.json", get(move || {
            let hub = h4.clone();
            async move {
                match hub.world.places_json() {
                    Some(blob) => ([(header::CONTENT_TYPE, "application/json"),
                                    (header::CACHE_CONTROL, "max-age=31536000, immutable")], blob).into_response(),
                    None => StatusCode::NOT_FOUND.into_response(),
                }
            }
        }))
        .route("/world/zone/{level}/{tile}", get(move |UrlPath((level, tile)): UrlPath<(String, String)>| {
            let hub = h5.clone();
            async move {
                let parsed = (|| {
                    let level: usize = level.parse().ok()?;
                    let (i, j) = tile.split_once('_')?;
                    Some((level, i.parse::<u32>().ok()?, j.parse::<u32>().ok()?))
                })();
                let (Some((level, i, j)), Some(om)) = (parsed, hub.world.openmap()) else {
                    return StatusCode::NOT_FOUND.into_response();
                };
                cached_file(move || om.zone_tile(level, i, j)).await
            }
        }))
        .route("/world/mini/{file}", get(move |UrlPath(file): UrlPath<String>| {
            let hub = h8.clone();
            async move {
                let Ok(file) = file.parse::<u32>() else { return StatusCode::NOT_FOUND.into_response() };
                cached_file(move || hub.world.minimap_path(file)).await
            }
        }))
        .route("/icon/{key}", get(move |UrlPath(key): UrlPath<String>| {
            let hub = h6.clone();
            async move {
                let Some(key) = strip_png(&key) else { return StatusCode::NOT_FOUND.into_response() };
                cached_file(move || hub.icons.icon_path(&key)).await
            }
        }))
        .route("/world/shade/{key}", get(move |UrlPath(key): UrlPath<String>| {
            let hub = h7.clone();
            async move {
                let Some(key) = strip_png(&key) else { return StatusCode::NOT_FOUND.into_response() };
                cached_file(move || hub.shades.shade_path(&key)).await
            }
        }))
        .layer(middleware::from_fn_with_state(app.clone(), auth))
        .with_state(app)
}
