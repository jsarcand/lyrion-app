//! Local same-origin shell for release builds.
//!
//! The packaged app is `tauri://localhost`. An iframe to `http://<lms>:9000`
//! is cross-origin, and WKWebView then reports a 0×0 frame: Material stays blank
//! (the webview's default white shows through). This server is `http://localhost`,
//! which ATS allows and which Tauri can allowlist for IPC (a raw IP has no
//! domain, so `127.0.0.1` cannot). The window navigates here, and `/material`
//! is proxied to LMS so the iframe is same-origin with the shell.

use std::convert::Infallible;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use hyper::body::HttpBody;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Client, Request, Response, Server, StatusCode, Uri};
use hyper::client::HttpConnector;

use crate::CURRENT_LMS;

pub const SHELL_PORT: u16 = 14321;

static LISTENERS: std::sync::Mutex<Vec<TcpListener>> = std::sync::Mutex::new(Vec::new());
static LOGGED_MATERIAL: AtomicBool = AtomicBool::new(false);

/// Bind before any window loads so the first navigation can connect.
/// Both loopback families: WKWebView may open `localhost` as ::1 or 127.0.0.1.
pub fn reserve() {
    let mut listeners = Vec::new();
    for addr in [
        SocketAddr::from((Ipv4Addr::LOCALHOST, SHELL_PORT)),
        SocketAddr::from((Ipv6Addr::LOCALHOST, SHELL_PORT)),
    ] {
        match TcpListener::bind(addr) {
            Ok(listener) => listeners.push(listener),
            Err(err) => eprintln!("Lyrion shell proxy did not bind {addr}: {err}"),
        }
    }
    if let Ok(mut slot) = LISTENERS.lock() {
        *slot = listeners;
    }
}

pub fn start(app: tauri::AppHandle) {
    let listeners = match LISTENERS.lock() {
        Ok(mut slot) => std::mem::take(&mut *slot),
        Err(_) => Vec::new(),
    };
    if listeners.is_empty() {
        eprintln!("Lyrion shell proxy has no listener");
        return;
    }
    eprintln!("Lyrion shell proxy {}", shell_origin());
    for listener in listeners {
        serve_listener(app.clone(), listener);
    }
}

fn serve_listener(app: tauri::AppHandle, listener: TcpListener) {
    // setup() is not inside the Tokio context. hyper's from_tcp calls
    // TcpListener::from_std, which panics with "no reactor running" and
    // aborted the process before the window appeared.
    let built = {
        let _guard = tauri::async_runtime::handle().inner().enter();
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Server::from_tcp(listener)))
    };
    let server = match built {
        Ok(Ok(server)) => server,
        Ok(Err(err)) => {
            eprintln!("Lyrion shell proxy: {err}");
            return;
        }
        Err(_) => {
            eprintln!("Lyrion shell proxy failed to attach to the runtime");
            return;
        }
    };
    let make_svc = make_service_fn(move |_| {
        let app = app.clone();
        async move {
            Ok::<_, Infallible>(service_fn(move |req| {
                let app = app.clone();
                async move { Ok::<_, Infallible>(dispatch(app, req).await) }
            }))
        }
    });
    tauri::async_runtime::spawn(async move {
        if let Err(err) = server.serve(make_svc).await {
            eprintln!("Lyrion shell proxy stopped: {err}");
        }
    });
}

fn is_lms_path(path: &str) -> bool {
    path == "/jsonrpc.js"
        || path.starts_with("/material")
        || path.starts_with("/html")
        || path.starts_with("/plugins")
        || path.starts_with("/music")
        || path.starts_with("/imageproxy")
        || path.starts_with("/cometd")
        || path.starts_with("/status")
        || path.starts_with("/repo")
        || path.starts_with("/update")
        || path.starts_with("/slim")
}

fn pct_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query_param(query: &str, key: &str) -> Option<String> {
    for pair in query.split('&') {
        let mut parts = pair.splitn(2, '=');
        let k = parts.next().unwrap_or("");
        if k == key {
            return Some(pct_decode(parts.next().unwrap_or("")));
        }
    }
    None
}

fn lms_from_query(query: &str) -> Option<(String, String)> {
    let host = query_param(query, "lmsHost").or_else(|| query_param(query, "lms"))?;
    let host = host.trim().to_string();
    if host.is_empty() {
        return None;
    }
    let port = query_param(query, "lmsPort")
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| "9000".to_string());
    Some((host, port))
}

fn lms_from_referer(req: &Request<Body>) -> Option<(String, String)> {
    let referer = req.headers().get(hyper::header::REFERER)?.to_str().ok()?;
    let q = referer.split_once('?')?.1;
    let q = q.split('#').next().unwrap_or(q);
    lms_from_query(q)
}

fn lms_from_disk(app: &tauri::AppHandle) -> Option<(String, String)> {
    let dir = app.path_resolver().app_config_dir()?;
    let text = std::fs::read_to_string(dir.join("config.json")).ok()?;
    let cfg: serde_json::Value = serde_json::from_str(&text).ok()?;
    let server = cfg.get("server")?;
    let host = server.get("host")?.as_str()?.trim().to_string();
    if host.is_empty() {
        return None;
    }
    let port = server
        .get("port")
        .and_then(|p| p.as_u64().map(|n| n.to_string()).or_else(|| p.as_str().map(|s| s.to_string())))
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| "9000".to_string());
    Some((host, port))
}

fn lms_target(app: &tauri::AppHandle, req: &Request<Body>) -> Option<(String, String)> {
    if let Some(q) = req.uri().query() {
        if let Some(pair) = lms_from_query(q) {
            return Some(pair);
        }
    }
    if let Some(pair) = lms_from_referer(req) {
        return Some(pair);
    }
    if let Some(pair) = CURRENT_LMS.lock().ok().and_then(|g| g.clone()) {
        if !pair.0.is_empty() {
            return Some(pair);
        }
    }
    lms_from_disk(app)
}

fn shell_origin() -> String {
    format!("http://localhost:{SHELL_PORT}")
}

fn rewrite_location(loc: &str, host: &str, port: &str) -> String {
    let absolute = format!("http://{host}:{port}");
    if let Some(rest) = loc.strip_prefix(&absolute) {
        return format!("{}{}", shell_origin(), rest);
    }
    if loc.starts_with('/') {
        return format!("{}{}", shell_origin(), loc);
    }
    loc.to_string()
}

fn rewrite_cookie(cookie: &str) -> String {
    cookie
        .split(';')
        .filter(|part| {
            let t = part.trim().to_ascii_lowercase();
            !t.starts_with("domain=") && t != "secure"
        })
        .collect::<Vec<_>>()
        .join(";")
}

fn text_response(status: StatusCode, body: &str) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("content-type", "text/plain; charset=utf-8")
        .body(Body::from(body.to_string()))
        .unwrap_or_else(|_| Response::new(Body::from(body.to_string())))
}

async fn proxy_lms(app: tauri::AppHandle, req: Request<Body>) -> Response<Body> {
    let Some((host, port)) = lms_target(&app, &req) else {
        return text_response(StatusCode::BAD_GATEWAY, "LMS proxy: no server configured");
    };
    let path_and_query = req
        .uri()
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/");
    let url = format!("http://{host}:{port}{path_and_query}");
    let uri: Uri = match url.parse() {
        Ok(u) => u,
        Err(err) => return text_response(StatusCode::BAD_GATEWAY, &format!("LMS proxy: {err}")),
    };
    let method = req.method().clone();
    let mut outbound = Request::builder().method(method).uri(uri);
    for (name, value) in req.headers().iter() {
        let key = name.as_str();
        if matches!(
            key,
            "host" | "connection" | "keep-alive" | "proxy-connection" | "transfer-encoding" | "content-length" | "accept-encoding"
        ) {
            continue;
        }
        outbound = outbound.header(name, value);
    }
    outbound = outbound.header("host", format!("{host}:{port}"));
    outbound = outbound.header("connection", "close");
    let body = match req.into_body().collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(err) => return text_response(StatusCode::BAD_GATEWAY, &format!("LMS proxy body: {err}")),
    };
    let client: Client<HttpConnector> = Client::builder()
        .pool_max_idle_per_host(4)
        .build(HttpConnector::new());
    let request = match outbound.body(Body::from(body)) {
        Ok(r) => r,
        Err(err) => return text_response(StatusCode::BAD_GATEWAY, &format!("LMS proxy: {err}")),
    };
    let response = match tokio::time::timeout(Duration::from_secs(120), client.request(request)).await {
        Ok(Ok(r)) => r,
        Ok(Err(err)) => return text_response(StatusCode::BAD_GATEWAY, &format!("LMS proxy {host}:{port} {err}")),
        Err(_) => return text_response(StatusCode::GATEWAY_TIMEOUT, &format!("LMS proxy timeout {host}:{port}")),
    };
    let status = response.status();
    let mut builder = Response::builder().status(status);
    for (name, value) in response.headers().iter() {
        let key = name.as_str();
        if matches!(
            key,
            "connection" | "keep-alive" | "transfer-encoding" | "content-length" | "content-encoding"
        ) {
            continue;
        }
        if key.eq_ignore_ascii_case("location") {
            if let Ok(loc) = value.to_str() {
                builder = builder.header(name, rewrite_location(loc, &host, &port));
            }
            continue;
        }
        if key.eq_ignore_ascii_case("set-cookie") {
            if let Ok(cookie) = value.to_str() {
                builder = builder.header(name, rewrite_cookie(cookie));
            }
            continue;
        }
        builder = builder.header(name, value);
    }
    let bytes = match response.into_body().collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(err) => return text_response(StatusCode::BAD_GATEWAY, &format!("LMS proxy read: {err}")),
    };
    builder
        .body(Body::from(bytes))
        .unwrap_or_else(|_| text_response(StatusCode::BAD_GATEWAY, "LMS proxy response"))
}

fn serve_asset(app: &tauri::AppHandle, path: &str) -> Response<Body> {
    let key = if path.is_empty() || path == "/" {
        "/".to_string()
    } else if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    match app.asset_resolver().get(key) {
        Some(asset) => {
            let mut builder = Response::builder()
                .status(StatusCode::OK)
                .header("content-type", asset.mime_type);
            if let Some(csp) = asset.csp_header {
                builder = builder.header("content-security-policy", csp);
            }
            builder
                .body(Body::from(asset.bytes))
                .unwrap_or_else(|_| text_response(StatusCode::INTERNAL_SERVER_ERROR, "asset"))
        }
        None => text_response(StatusCode::NOT_FOUND, "not found"),
    }
}

async fn dispatch(app: tauri::AppHandle, req: Request<Body>) -> Response<Body> {
    let path = req.uri().path().to_string();
    if path == "/__lyrion_shell" {
        return text_response(StatusCode::NO_CONTENT, "");
    }
    if path.starts_with("/material") && !LOGGED_MATERIAL.swap(true, Ordering::Relaxed) {
        eprintln!("Lyrion shell: material requested");
    }
    if is_lms_path(&path) {
        return proxy_lms(app, req).await;
    }
    serve_asset(&app, &path)
}
