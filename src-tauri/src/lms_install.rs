//! Local Lyrion Music Server install orchestration.
//!
//! Official packages (see https://lyrion.org/getting-started/):
//! - macOS:  .dmg → silent attach (`hdiutil -nobrowse`) + copy .app to Applications
//! - Windows: .exe → launch installer
//! - Linux:  .deb → pkexec apt install
//!
//! Download URLs are resolved from the **live** lyrion.org/downloads page first
//! (GitHub downloads.yaml is often stale and 404s for nightlies).

use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use tauri::{AppHandle, Manager};

/// Pinned latest release when the downloads catalog is unavailable.
pub const LMS_FALLBACK_VERSION: &str = "9.2.0";
const DOWNLOADS_BASE: &str = "https://downloads.lms-community.org";
/// Live page — always current (preferred over YAML).
const DOWNLOADS_PAGE_URL: &str = "https://lyrion.org/downloads/";
const DOWNLOADS_REPO_PAGE_URL: &str = "https://lyrion.org/lms-server-repository/";
/// Secondary catalog (may lag; always HEAD-checked before use).
const DOWNLOADS_YAML_URL: &str =
    "https://raw.githubusercontent.com/lms-community/lms-community.github.io/main/docs/downloads/downloads.yaml";

/// Install channel IDs (match downloads.yaml keys).
pub const CHANNEL_LATEST: &str = "latest";
pub const CHANNEL_STABLE: &str = "stable";
pub const CHANNEL_DEV: &str = "dev";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LmsInstallStatus {
    pub platform: String,
    pub app_installed: bool,
    pub app_path: Option<String>,
    pub server_running: bool,
    pub host: String,
    pub port: u16,
    pub version_hint: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LmsChannelInfo {
    pub id: String,
    pub label: String,
    pub description: String,
    pub recommended: bool,
    /// Filled when catalog is available
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LmsPackageInfo {
    pub channel: String,
    pub channel_label: String,
    pub version: String,
    pub url: String,
    pub filename: String,
    pub platform: String,
    pub size_hint_mb: Option<u32>,
    pub notes: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LmsInstallProgress {
    pub stage: String,
    pub percent: Option<u8>,
    pub message: String,
}

fn platform_id() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "unknown"
    }
}

pub(crate) fn emit_progress(app: &AppHandle, stage: &str, percent: Option<u8>, message: impl Into<String>) {
    let payload = LmsInstallProgress {
        stage: stage.to_string(),
        percent,
        message: message.into(),
    };
    let _ = app.emit_all("lms-install-progress", payload);
}

/// Probe HTTP web UI on host:port (Material / classic skin root).
async fn http_server_alive(host: &str, port: u16) -> bool {
    lms_probe_server(Some(host.to_string()), Some(port))
        .await
        .map(|r| r.ok)
        .unwrap_or(false)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LmsProbeResult {
    pub ok: bool,
    pub host: String,
    pub port: u16,
    pub url: Option<String>,
    pub status: Option<u16>,
    pub message: String,
}

/// Reliable reachability check (Rust/reqwest — not Tauri http allowlist).
/// Tries LMS root, Material, and jsonrpc.
#[tauri::command]
pub async fn lms_probe_server(host: Option<String>, port: Option<u16>) -> Result<LmsProbeResult, String> {
    let host = host
        .unwrap_or_else(|| "127.0.0.1".into())
        .trim()
        .to_string();
    let port = port.unwrap_or(9000);
    if host.is_empty() {
        return Ok(LmsProbeResult {
            ok: false,
            host,
            port,
            url: None,
            status: None,
            message: "No host configured".into(),
        });
    }

    // pCP / busy LMS often stalls HTTP/1.1 keep-alive (5–12s, 0 bytes) while
    // Connection: close returns in <1s. Don't reuse pooled sockets; give LAN time.
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .connect_timeout(Duration::from_secs(3))
        .pool_max_idle_per_host(0)
        .tcp_nodelay(true)
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent("Lyrion/0.3 (probe; +https://lyrion.org)")
        .build()
        .map_err(|e| e.to_string())?;

    // Material first (what the iframe loads); root as fallback.
    let candidates = [
        format!("http://{}:{}/material/", host, port),
        format!("http://{}:{}/", host, port),
    ];

    let mut last_err = String::new();
    for url in &candidates {
        match client
            .get(url)
            .header("Connection", "close")
            .send()
            .await
        {
            Ok(resp) => {
                let code = resp.status().as_u16();
                // LMS often 200; some setups 301/302 before redirect follow; 401 rare
                if (200..400).contains(&code) || code == 401 {
                    return Ok(LmsProbeResult {
                        ok: true,
                        host,
                        port,
                        url: Some(url.clone()),
                        status: Some(code),
                        message: format!("Reachable ({})", code),
                    });
                }
                last_err = format!("{} → HTTP {}", url, code);
            }
            Err(e) => {
                last_err = format!("{} → {}", url, e);
            }
        }
    }

    Ok(LmsProbeResult {
        ok: false,
        host,
        port,
        url: None,
        status: None,
        message: if last_err.is_empty() {
            "Server not reachable".into()
        } else {
            format!("Not reachable: {}", last_err)
        },
    })
}

pub(crate) fn macos_app_paths() -> Vec<PathBuf> {
    vec![
        PathBuf::from("/Applications/Lyrion Music Server.app"),
        PathBuf::from("/Applications/Logitech Media Server.app"),
        dirs_next_home().map(|h| h.join("Applications/Lyrion Music Server.app")).unwrap_or_default(),
    ]
    .into_iter()
    .filter(|p| !p.as_os_str().is_empty())
    .collect()
}

fn dirs_next_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn windows_install_hints() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(pf) = std::env::var("ProgramFiles") {
        out.push(PathBuf::from(&pf).join("Lyrion Music Server"));
        out.push(PathBuf::from(&pf).join("Squeezebox"));
    }
    if let Ok(pf86) = std::env::var("ProgramFiles(x86)") {
        out.push(PathBuf::from(&pf86).join("Lyrion Music Server"));
        out.push(PathBuf::from(&pf86).join("Squeezebox"));
    }
    out
}

fn path_exists_any(paths: &[PathBuf]) -> Option<PathBuf> {
    paths.iter().find(|p| p.exists()).cloned()
}

/// Detect local LMS app presence + whether something answers on 127.0.0.1:9000.
#[tauri::command]
pub async fn lms_install_status() -> Result<LmsInstallStatus, String> {
    let platform = platform_id().to_string();
    let host = "127.0.0.1".to_string();
    let port = 9000u16;
    let server_running = http_server_alive(&host, port).await;

    let (app_installed, app_path, message) = if cfg!(target_os = "macos") {
        let p = path_exists_any(&macos_app_paths());
        let installed = p.is_some();
        let msg = if server_running {
            "Lyrion Music Server is running on this Mac.".to_string()
        } else if installed {
            "Lyrion Music Server is installed but not running. Use the menu bar item → Start Service, or Install/Start below.".to_string()
        } else {
            "No local Lyrion Music Server app found. You can install the official package.".to_string()
        };
        (installed, p.map(|x| x.display().to_string()), msg)
    } else if cfg!(target_os = "windows") {
        let p = path_exists_any(&windows_install_hints());
        // Service may run without a simple folder; treat HTTP as installed+running
        let installed = p.is_some() || server_running;
        let msg = if server_running {
            "Lyrion Music Server is running on this PC.".to_string()
        } else if installed {
            "LMS files found but the service does not answer on port 9000 yet.".to_string()
        } else {
            "No local Lyrion Music Server detected. You can install the official Windows package.".to_string()
        };
        (installed, p.map(|x| x.display().to_string()), msg)
    } else if cfg!(target_os = "linux") {
        let deb_installed = Command::new("dpkg")
            .args(["-s", "lyrionmusicserver"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
            || Command::new("dpkg")
                .args(["-s", "logitechmediaserver"])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
        let installed = deb_installed || server_running;
        let msg = if server_running {
            "Lyrion Music Server is running on this machine.".to_string()
        } else if installed {
            "Package installed; service may need start (systemctl start lyrionmusicserver).".to_string()
        } else {
            "No local LMS package detected. You can download and install the official .deb.".to_string()
        };
        (installed, None, msg)
    } else {
        (server_running, None, "Unsupported platform for guided install.".to_string())
    };

    Ok(LmsInstallStatus {
        platform,
        app_installed,
        app_path,
        server_running,
        host,
        port,
        version_hint: Some(LMS_FALLBACK_VERSION.to_string()),
        message,
    })
}

fn normalize_channel(channel: Option<&str>) -> String {
    match channel.map(|c| c.trim().to_lowercase()).as_deref() {
        Some("stable") | Some("nightly") | Some("stable-nightly") => CHANNEL_STABLE.to_string(),
        Some("dev") | Some("development") | Some("devel") => CHANNEL_DEV.to_string(),
        Some("latest") | Some("release") | Some("stable-release") | None | Some("") => {
            CHANNEL_LATEST.to_string()
        }
        Some(other) => other.to_string(),
    }
}

fn channel_meta(id: &str) -> (&'static str, &'static str, bool) {
    match id {
        CHANNEL_STABLE => (
            "Stable nightly",
            "Latest release + recent bug fixes (stable branch builds).",
            false,
        ),
        CHANNEL_DEV => (
            "Development",
            "Newest features — may be unstable. Only if you accept occasional breakage.",
            false,
        ),
        _ => (
            "Latest release",
            "Official stable release — recommended for most users.",
            true,
        ),
    }
}

fn platform_package_key() -> Result<&'static str, String> {
    if cfg!(target_os = "macos") {
        Ok("macos")
    } else if cfg!(target_os = "windows") {
        Ok("win64")
    } else if cfg!(target_os = "linux") {
        match std::env::consts::ARCH {
            "aarch64" | "arm" => Ok("debarm"),
            _ => Ok("debamd64"),
        }
    } else {
        Err("Guided install is not available on this platform.".into())
    }
}

fn platform_install_notes() -> String {
    if cfg!(target_os = "macos") {
        "Downloads the official DMG, attaches it silently (no Finder), copies Lyrion Music Server.app to Applications, then launches it. Use the menu bar item → Start Service if the server is not running yet.".into()
    } else if cfg!(target_os = "windows") {
        "Launches the official Windows installer (64-bit). Complete the wizard; the service usually starts automatically.".into()
    } else if cfg!(target_os = "linux") {
        "Downloads the official .deb and runs: pkexec apt-get install -y <package> (admin password required).".into()
    } else {
        String::new()
    }
}

fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("Lyrion/0.3 (desktop install; +https://lyrion.org)")
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|e| e.to_string())
}

async fn url_ok(client: &reqwest::Client, url: &str) -> bool {
    match client.head(url).send().await {
        Ok(r) if r.status().is_success() => true,
        Ok(r) if r.status().as_u16() == 405 || r.status().as_u16() == 403 => {
            // Some CDNs dislike HEAD — try a ranged GET
            match client
                .get(url)
                .header("Range", "bytes=0-0")
                .send()
                .await
            {
                Ok(g) => g.status().is_success() || g.status().as_u16() == 206,
                Err(_) => false,
            }
        }
        _ => false,
    }
}

/// Platform file suffix / path fragment to pick the right artifact from a page of links.
fn platform_url_matchers() -> Vec<&'static str> {
    if cfg!(target_os = "macos") {
        vec![".dmg"]
    } else if cfg!(target_os = "windows") {
        vec!["-win64.exe", "win64.exe"]
    } else if cfg!(target_os = "linux") {
        match std::env::consts::ARCH {
            "aarch64" | "arm" => vec!["_arm.deb"],
            _ => vec!["_amd64.deb"],
        }
    } else {
        vec![]
    }
}

/// Classify a download URL into latest / stable / dev using path + version in the name.
fn classify_download_url(url: &str) -> Option<(String, String, String)> {
    // filename
    let filename = url.rsplit('/').next()?.to_string();
    if filename.is_empty() {
        return None;
    }
    // Must look like an LMS installer for this platform
    let matchers = platform_url_matchers();
    if !matchers.iter().any(|m| filename.contains(m) || url.contains(m)) {
        return None;
    }
    // Ignore tarballs / squash when we want deb/dmg/exe only
    if filename.ends_with(".tgz") || filename.ends_with(".tcz") || filename.ends_with(".rpm") {
        return None;
    }
    if cfg!(target_os = "linux") && !filename.ends_with(".deb") {
        return None;
    }
    if cfg!(target_os = "macos") && !filename.ends_with(".dmg") {
        return None;
    }
    if cfg!(target_os = "windows") && !filename.ends_with(".exe") {
        return None;
    }

    // Version: LyrionMusicServer-9.1.1.dmg or lyrionmusicserver_9.1.2~stamp_amd64.deb
    let version = extract_version_from_name(&filename).unwrap_or_else(|| "unknown".into());
    let channel = if url.contains("/nightly/") {
        // Nightlies: 9.2.x → dev, else stable bugfix branch
        if version.starts_with("9.2") || version.starts_with("10.") {
            CHANNEL_DEV.to_string()
        } else {
            CHANNEL_STABLE.to_string()
        }
    } else if url.contains("LyrionMusicServer_v") || url.contains("/LyrionMusicServer_v") {
        CHANNEL_LATEST.to_string()
    } else if url.contains("/nightly/") {
        CHANNEL_STABLE.to_string()
    } else {
        CHANNEL_LATEST.to_string()
    };
    Some((channel, version, filename))
}

fn extract_version_from_name(name: &str) -> Option<String> {
    // Match X.Y.Z
    let re = regex_lite_version(name)?;
    Some(re)
}

/// Minimal X.Y.Z finder without pulling the regex crate.
fn regex_lite_version(name: &str) -> Option<String> {
    let bytes = name.as_bytes();
    let mut i = 0;
    while i + 4 < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            let mut dots = 0;
            let mut j = i;
            while j < bytes.len() {
                let c = bytes[j];
                if c.is_ascii_digit() {
                    j += 1;
                } else if c == b'.' && dots < 2 && j + 1 < bytes.len() && bytes[j + 1].is_ascii_digit() {
                    dots += 1;
                    j += 1;
                } else {
                    break;
                }
            }
            if dots == 2 {
                return Some(name[start..j].to_string());
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    None
}

fn extract_urls_from_html(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let needle = "https://downloads.lms-community.org/";
    let mut rest = html;
    while let Some(idx) = rest.find(needle) {
        let s = &rest[idx..];
        let end = s
            .find(|c: char| c == '"' || c == '\'' || c == '<' || c == '>' || c.is_whitespace() || c == ')')
            .unwrap_or(s.len());
        let url = s[..end].trim_end_matches(['.', ',', ';']).to_string();
        if !out.contains(&url) {
            out.push(url);
        }
        rest = &s[needle.len()..];
    }
    out
}

/// Scrape live download pages and pick best URL per channel for this platform.
async fn fetch_live_packages() -> Result<HashMap<String, LmsPackageInfo>, String> {
    let client = http_client()?;
    let mut html = String::new();
    for page in [DOWNLOADS_PAGE_URL, DOWNLOADS_REPO_PAGE_URL] {
        match client.get(page).send().await {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(t) = resp.text().await {
                    html.push_str(&t);
                    html.push('\n');
                }
            }
            _ => {}
        }
    }
    if html.is_empty() {
        return Err("Could not fetch live download pages".into());
    }

    // channel -> (sort_key, info)  sort_key prefers higher version / longer build stamp
    let mut best: HashMap<String, (String, LmsPackageInfo)> = HashMap::new();

    for url in extract_urls_from_html(&html) {
        let Some((channel, version, filename)) = classify_download_url(&url) else {
            continue;
        };
        let (label, desc, _) = channel_meta(&channel);
        let sort_key = format!("{}|{}", version, filename); // higher filename often newer stamp
        let info = LmsPackageInfo {
            channel: channel.clone(),
            channel_label: label.into(),
            version: version.clone(),
            url: url.clone(),
            filename,
            platform: platform_id().into(),
            size_hint_mb: None,
            notes: {
                let mut n = platform_install_notes();
                n.push(' ');
                n.push_str(desc);
                n
            },
        };
        match best.get(&channel) {
            Some((prev, _)) if prev >= &sort_key => {}
            _ => {
                best.insert(channel, (sort_key, info));
            }
        }
    }

    if best.is_empty() {
        return Err("No platform packages found on live download pages".into());
    }

    // HEAD-check winners (drop 404s)
    let mut out = HashMap::new();
    for (ch, (_, info)) in best {
        if url_ok(&client, &info.url).await {
            out.insert(ch, info);
        } else {
            eprintln!("live package 404, skip: {}", info.url);
        }
    }
    if out.is_empty() {
        return Err("Live package URLs all failed HEAD check".into());
    }
    Ok(out)
}

fn parse_size_mb(size: &str) -> Option<u32> {
    // "43 MB" / "14 MB"
    let s = size.trim().to_lowercase().replace("mb", "").replace(' ', "");
    s.parse::<f32>().ok().map(|n| n.round() as u32)
}

/// Fallback package URLs when the catalog cannot be fetched (latest only).
fn package_fallback(channel: &str) -> Result<LmsPackageInfo, String> {
    let ch = normalize_channel(Some(channel));
    if ch != CHANNEL_LATEST {
        return Err(format!(
            "Cannot resolve '{}' channel offline — check network and retry.",
            ch
        ));
    }
    let ver = LMS_FALLBACK_VERSION;
    let folder = format!("LyrionMusicServer_v{}", ver);
    let (label, _, _) = channel_meta(CHANNEL_LATEST);

    if cfg!(target_os = "macos") {
        let filename = format!("LyrionMusicServer-{}.dmg", ver);
        return Ok(LmsPackageInfo {
            channel: CHANNEL_LATEST.into(),
            channel_label: label.into(),
            version: ver.into(),
            url: format!("{}/{}/{}", DOWNLOADS_BASE, folder, filename),
            filename,
            platform: "macos".into(),
            size_hint_mb: Some(43),
            notes: platform_install_notes(),
        });
    }
    if cfg!(target_os = "windows") {
        let filename = format!("LyrionMusicServer-{}-win64.exe", ver);
        return Ok(LmsPackageInfo {
            channel: CHANNEL_LATEST.into(),
            channel_label: label.into(),
            version: ver.into(),
            url: format!("{}/{}/{}", DOWNLOADS_BASE, folder, filename),
            filename,
            platform: "windows".into(),
            size_hint_mb: Some(14),
            notes: platform_install_notes(),
        });
    }
    if cfg!(target_os = "linux") {
        let arch = std::env::consts::ARCH;
        let (filename, size) = match arch {
            "aarch64" | "arm" => (format!("lyrionmusicserver_{}_arm.deb", ver), 26),
            _ => (format!("lyrionmusicserver_{}_amd64.deb", ver), 20),
        };
        return Ok(LmsPackageInfo {
            channel: CHANNEL_LATEST.into(),
            channel_label: label.into(),
            version: ver.into(),
            url: format!("{}/{}/{}", DOWNLOADS_BASE, folder, filename),
            filename,
            platform: "linux".into(),
            size_hint_mb: Some(size),
            notes: platform_install_notes(),
        });
    }
    Err("Guided install is not available on this platform.".into())
}

async fn fetch_downloads_catalog() -> Result<serde_yaml::Value, String> {
    let client = http_client()?;
    let text = client
        .get(DOWNLOADS_YAML_URL)
        .send()
        .await
        .map_err(|e| format!("Catalog download failed: {}", e))?
        .error_for_status()
        .map_err(|e| format!("Catalog HTTP error: {}", e))?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    serde_yaml::from_str(&text).map_err(|e| format!("Catalog parse error: {}", e))
}

fn package_from_catalog(catalog: &serde_yaml::Value, channel: &str) -> Result<LmsPackageInfo, String> {
    let ch = normalize_channel(Some(channel));
    let (label, desc, _) = channel_meta(&ch);
    let node = catalog
        .get(&ch)
        .ok_or_else(|| format!("Unknown channel '{}' in catalog", ch))?;
    let pkg_key = platform_package_key()?;
    let pkg = node
        .get(pkg_key)
        .filter(|v| !v.is_null())
        .ok_or_else(|| format!("No {} package for channel {}", pkg_key, ch))?;

    let url = pkg
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or("Package URL missing")?
        .to_string();
    let filename = pkg
        .get("name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            url.rsplit('/')
                .next()
                .unwrap_or("lyrion-package")
                .to_string()
        });
    let version = node
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let size_hint_mb = pkg
        .get("size")
        .and_then(|v| v.as_str())
        .and_then(parse_size_mb);
    let mut notes = platform_install_notes();
    notes.push(' ');
    notes.push_str(desc);

    Ok(LmsPackageInfo {
        channel: ch,
        channel_label: label.into(),
        version,
        url,
        filename,
        platform: platform_id().into(),
        size_hint_mb,
        notes,
    })
}

async fn resolve_package(channel: Option<String>, version: Option<String>) -> Result<LmsPackageInfo, String> {
    // Explicit version still supported (latest-style fixed path only)
    if let Some(ver) = version.filter(|v| !v.trim().is_empty()) {
        let mut info = package_fallback(CHANNEL_LATEST)?;
        let folder = format!("LyrionMusicServer_v{}", ver.trim());
        if cfg!(target_os = "macos") {
            info.filename = format!("LyrionMusicServer-{}.dmg", ver.trim());
            info.url = format!("{}/{}/{}", DOWNLOADS_BASE, folder, info.filename);
        } else if cfg!(target_os = "windows") {
            info.filename = format!("LyrionMusicServer-{}-win64.exe", ver.trim());
            info.url = format!("{}/{}/{}", DOWNLOADS_BASE, folder, info.filename);
        } else if cfg!(target_os = "linux") {
            let arch = std::env::consts::ARCH;
            info.filename = match arch {
                "aarch64" | "arm" => format!("lyrionmusicserver_{}_arm.deb", ver.trim()),
                _ => format!("lyrionmusicserver_{}_amd64.deb", ver.trim()),
            };
            info.url = format!("{}/{}/{}", DOWNLOADS_BASE, folder, info.filename);
        }
        info.version = ver.trim().to_string();
        // Validate
        let client = http_client()?;
        if !url_ok(&client, &info.url).await {
            return Err(format!("Package not found (404): {}", info.url));
        }
        return Ok(info);
    }

    let ch = normalize_channel(channel.as_deref());

    // 1) Live pages (preferred — always current)
    if let Ok(live) = fetch_live_packages().await {
        if let Some(info) = live.get(&ch) {
            return Ok(info.clone());
        }
        eprintln!("channel '{}' not found on live download page", ch);
    }

    // 2) YAML catalog with HEAD check (often stale for nightlies)
    if let Ok(catalog) = fetch_downloads_catalog().await {
        if let Ok(info) = package_from_catalog(&catalog, &ch) {
            let client = http_client()?;
            if url_ok(&client, &info.url).await {
                return Ok(info);
            }
            eprintln!("catalog URL 404, skip: {}", info.url);
        }
    }

    // 3) Hardcoded latest fallback
    let fb = package_fallback(&ch)?;
    let client = http_client()?;
    if url_ok(&client, &fb.url).await {
        return Ok(fb);
    }
    Err(format!(
        "Could not resolve a valid package for channel '{}'. Check https://lyrion.org/downloads/",
        ch
    ))
}

/// List install channels (latest / stable nightly / development).
#[tauri::command]
pub async fn lms_channels() -> Result<Vec<LmsChannelInfo>, String> {
    let ids = [CHANNEL_LATEST, CHANNEL_STABLE, CHANNEL_DEV];
    let live = fetch_live_packages().await.ok();
    let mut out = Vec::new();
    for id in ids {
        let (label, description, recommended) = channel_meta(id);
        let version = live
            .as_ref()
            .and_then(|m| m.get(id))
            .map(|p| p.version.clone());
        out.push(LmsChannelInfo {
            id: id.into(),
            label: label.into(),
            description: description.into(),
            recommended,
            version,
        });
    }
    Ok(out)
}

#[tauri::command]
pub async fn lms_package_info(
    channel: Option<String>,
    version: Option<String>,
) -> Result<LmsPackageInfo, String> {
    resolve_package(channel, version).await
}

fn download_dir() -> Result<PathBuf, String> {
    let base = std::env::temp_dir().join("lyrion-install");
    std::fs::create_dir_all(&base).map_err(|e| format!("temp dir: {}", e))?;
    Ok(base)
}

pub(crate) async fn download_file(app: &AppHandle, url: &str, dest: &Path) -> Result<(), String> {
    emit_progress(app, "download", Some(0), format!("Downloading {}…", dest.file_name().and_then(|s| s.to_str()).unwrap_or("package")));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(600))
        .user_agent("Lyrion/0.3 (desktop install; +https://lyrion.org)")
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|e| e.to_string())?;
    // Fail fast on 404 before writing a tiny error body
    if !url_ok(&client, url).await {
        return Err(format!(
            "Package URL not found (404): {}\nTry another channel or open https://lyrion.org/downloads/",
            url
        ));
    }
    let mut resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Download failed: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!("Download HTTP {} for {}", resp.status(), url));
    }
    let total = resp.content_length().unwrap_or(0);
    let mut file = std::fs::File::create(dest).map_err(|e| format!("Create file: {}", e))?;
    let mut written: u64 = 0;
    while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
        use std::io::Write;
        file.write_all(&chunk).map_err(|e| e.to_string())?;
        written += chunk.len() as u64;
        if total > 0 {
            let pct = ((written as f64 / total as f64) * 100.0).min(99.0) as u8;
            emit_progress(
                app,
                "download",
                Some(pct),
                format!("Downloading… {} / {} MB", written / 1_000_000, total / 1_000_000),
            );
        }
    }
    emit_progress(app, "download", Some(100), "Download complete");
    Ok(())
}

fn open_path(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg(path)
            .spawn()
            .map_err(|e| format!("open: {}", e))?;
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    {
        Command::new("cmd")
            .args(["/C", "start", "", &path.display().to_string()])
            .spawn()
            .map_err(|e| format!("start: {}", e))?;
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        Command::new("xdg-open")
            .arg(path)
            .spawn()
            .map_err(|e| format!("xdg-open: {}", e))?;
        return Ok(());
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = path;
        Err("open not supported".into())
    }
}

/// Download the official installer and launch the platform install flow.
/// `channel`: "latest" | "stable" | "dev" (default latest).
#[tauri::command]
pub async fn lms_download_and_install(
    app: AppHandle,
    channel: Option<String>,
    version: Option<String>,
) -> Result<LmsPackageInfo, String> {
    let info = resolve_package(channel, version).await?;
    let dest = download_dir()?.join(&info.filename);

    emit_progress(&app, "prepare", Some(0), format!("Preparing {} install…", info.platform));
    download_file(&app, &info.url, &dest).await?;

    emit_progress(&app, "install", Some(0), "Launching installer…");

    if cfg!(target_os = "linux") {
        // Guided apt install with polkit elevation
        emit_progress(
            &app,
            "install",
            Some(10),
            "Requesting administrator rights to install the .deb…",
        );
        let status = Command::new("pkexec")
            .args([
                "apt-get",
                "install",
                "-y",
                &dest.display().to_string(),
            ])
            .status()
            .map_err(|e| {
                format!(
                    "pkexec apt-get failed ({}). You can install manually: sudo apt install {}",
                    e,
                    dest.display()
                )
            })?;
        if !status.success() {
            // Fallback: open file manager / let user double-click
            let _ = open_path(&dest);
            return Err(format!(
                "apt install exited with {}. Package saved at {} — install with: sudo apt install {}",
                status.code().unwrap_or(-1),
                dest.display(),
                dest.display()
            ));
        }
        // Best-effort service start
        let _ = Command::new("pkexec")
            .args(["systemctl", "start", "lyrionmusicserver"])
            .status();
        let _ = Command::new("pkexec")
            .args(["systemctl", "start", "logitechmediaserver"])
            .status();
        emit_progress(&app, "install", Some(100), "Package installed");
    } else if cfg!(target_os = "windows") {
        // Launch interactive installer (official signed LMS package)
        Command::new(&dest)
            .spawn()
            .map_err(|e| format!("Could not launch installer: {}", e))?;
        emit_progress(
            &app,
            "install",
            Some(50),
            "Installer opened — complete the wizard, then wait for the server to start",
        );
    } else if cfg!(target_os = "macos") {
        emit_progress(
            &app,
            "install",
            Some(20),
            "Installing from DMG (silent attach, no Finder)…",
        );
        match macos_install_dmg(&app, &dest) {
            Ok(app_path) => {
                emit_progress(
                    &app,
                    "install",
                    Some(100),
                    format!("Installed to {} — launching…", app_path.display()),
                );
                let _ = Command::new("open").arg(&app_path).spawn();
            }
            Err(e) => {
                // Fallback: open DMG for manual drag-install
                eprintln!("silent DMG install failed: {}", e);
                let _ = open_path(&dest);
                emit_progress(
                    &app,
                    "install",
                    Some(50),
                    format!(
                        "Automatic install failed ({}). DMG opened — drag the app to Applications.",
                        e
                    ),
                );
            }
        }
    }

    Ok(info)
}

/// Attach DMG without Finder UI, copy *.app into Applications (or ~/Applications).
#[cfg(target_os = "macos")]
fn macos_install_dmg(app: &AppHandle, dmg: &Path) -> Result<PathBuf, String> {
    let mount = std::env::temp_dir().join(format!("lyrion-lms-mnt-{}", std::process::id()));
    // Clean previous mountpoint
    let _ = std::fs::remove_dir_all(&mount);
    std::fs::create_dir_all(&mount).map_err(|e| format!("mount dir: {}", e))?;

    emit_progress(app, "install", Some(30), "Attaching disk image…");
    // -nobrowse: no Finder window; -readonly; private mountpoint
    let attach = Command::new("hdiutil")
        .args([
            "attach",
            "-nobrowse",
            "-readonly",
            "-quiet",
            "-mountpoint",
        ])
        .arg(&mount)
        .arg(dmg)
        .status()
        .map_err(|e| format!("hdiutil attach: {}", e))?;
    if !attach.success() {
        let _ = std::fs::remove_dir_all(&mount);
        return Err(format!(
            "hdiutil attach failed (exit {})",
            attach.code().unwrap_or(-1)
        ));
    }

    let result = (|| {
        emit_progress(app, "install", Some(50), "Locating Lyrion Music Server.app…");
        let src_app = find_app_bundle(&mount)
            .ok_or_else(|| "No .app bundle found inside the DMG".to_string())?;

        // Prefer /Applications; fall back to ~/Applications
        let dest_dir = PathBuf::from("/Applications");
        let dest_app = if dest_dir.exists() && is_writable_dir(&dest_dir) {
            dest_dir.join(src_app.file_name().unwrap_or_default())
        } else {
            let home_apps = dirs_next_home()
                .map(|h| h.join("Applications"))
                .ok_or_else(|| "No HOME for ~/Applications".to_string())?;
            std::fs::create_dir_all(&home_apps).map_err(|e| format!("~/Applications: {}", e))?;
            home_apps.join(src_app.file_name().unwrap_or_default())
        };

        emit_progress(
            app,
            "install",
            Some(70),
            format!("Copying to {}…", dest_app.display()),
        );
        // Remove previous install of the same name
        if dest_app.exists() {
            let _ = std::fs::remove_dir_all(&dest_app);
        }
        // ditto preserves resource forks / code signature better than cp -R
        let copy = Command::new("ditto")
            .arg(&src_app)
            .arg(&dest_app)
            .status()
            .map_err(|e| format!("ditto: {}", e))?;
        if !copy.success() {
            // Fallback cp -R
            let cp = Command::new("cp")
                .args(["-R"])
                .arg(&src_app)
                .arg(&dest_app)
                .status()
                .map_err(|e| format!("cp: {}", e))?;
            if !cp.success() {
                return Err(format!(
                    "Could not copy app to {} (permission denied?).",
                    dest_app.display()
                ));
            }
        }
        // Clear quarantine on the .app only — never -r (recursive).
        // LMS ships thousands of Perl files; xattr -dr walks them all, is very
        // slow, and floods stderr with Permission denied.
        let _ = Command::new("xattr")
            .args(["-d", "com.apple.quarantine"])
            .arg(&dest_app)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        Ok(dest_app)
    })();

    // Always detach
    let _ = Command::new("hdiutil")
        .args(["detach", "-quiet"])
        .arg(&mount)
        .status();
    // force if busy
    let _ = Command::new("hdiutil")
        .args(["detach", "-force", "-quiet"])
        .arg(&mount)
        .status();
    let _ = std::fs::remove_dir_all(&mount);

    result
}

#[cfg(target_os = "macos")]
fn find_app_bundle(root: &Path) -> Option<PathBuf> {
    // Prefer exact LMS names
    let preferred = [
        root.join("Lyrion Music Server.app"),
        root.join("Logitech Media Server.app"),
    ];
    for p in &preferred {
        if p.is_dir() {
            return Some(p.clone());
        }
    }
    // Any .app at top level
    if let Ok(rd) = std::fs::read_dir(root) {
        for ent in rd.flatten() {
            let p = ent.path();
            if p.extension().and_then(|e| e.to_str()) == Some("app") {
                return Some(p);
            }
        }
    }
    // One level deeper (some DMGs wrap in a folder)
    if let Ok(rd) = std::fs::read_dir(root) {
        for ent in rd.flatten() {
            let p = ent.path();
            if p.is_dir() {
                if let Ok(rd2) = std::fs::read_dir(&p) {
                    for ent2 in rd2.flatten() {
                        let p2 = ent2.path();
                        if p2.extension().and_then(|e| e.to_str()) == Some("app") {
                            return Some(p2);
                        }
                    }
                }
            }
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn is_writable_dir(path: &Path) -> bool {
    // Probe by creating+removing a temp file
    let probe = path.join(format!(".lyrion-write-test-{}", std::process::id()));
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

#[cfg(not(target_os = "macos"))]
#[allow(dead_code)]
fn macos_install_dmg(_app: &AppHandle, _dmg: &Path) -> Result<PathBuf, String> {
    Err("macOS only".into())
}

fn macos_server_script(name: &str) -> Option<PathBuf> {
    for app in macos_app_paths() {
        let nested = app
            .join("Contents/MacOS/Lyrion Music Server.app/Contents/Resources")
            .join(name);
        if nested.is_file() {
            return Some(nested);
        }
        let nested_old = app
            .join("Contents/MacOS/Logitech Media Server.app/Contents/Resources")
            .join(name);
        if nested_old.is_file() {
            return Some(nested_old);
        }
        let direct = app.join("Contents/Resources").join(name);
        if direct.is_file() {
            return Some(direct);
        }
    }
    None
}

fn run_macos_server_script(name: &str) -> Result<(), String> {
    let script = macos_server_script(name)
        .ok_or_else(|| format!("{name} not found inside Lyrion Music Server.app"))?;
    Command::new("bash")
        .arg(&script)
        .spawn()
        .map_err(|e| format!("{}: {}", script.display(), e))?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn macos_user_notification(title: &str, body: &str) {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    use std::ffi::CString;
    fn ns(s: &str) -> *mut Object {
        let c = CString::new(s).unwrap_or_else(|_| CString::new("").unwrap());
        unsafe { msg_send![class!(NSString), stringWithUTF8String: c.as_ptr()] }
    }
    unsafe {
        let n: *mut Object = msg_send![class!(NSUserNotification), new];
        if n.is_null() {
            return;
        }
        let _: () = msg_send![n, setTitle: ns(title)];
        let _: () = msg_send![n, setInformativeText: ns(body)];
        let center: *mut Object =
            msg_send![class!(NSUserNotificationCenter), defaultUserNotificationCenter];
        if !center.is_null() {
            let _: () = msg_send![center, deliverNotification: n];
        }
    }
}

/// Best-effort: launch the local LMS app / start service, then poll readiness.
#[tauri::command]
pub async fn lms_start_local(app: AppHandle) -> Result<LmsInstallStatus, String> {
    emit_progress(&app, "start", Some(0), "Démarrage en cours…");
    let _ = app.emit_all("lms-local-starting", ());
    #[cfg(target_os = "macos")]
    macos_user_notification("Lyrion Music Server", "Démarrage en cours…");

    if cfg!(target_os = "macos") {
        if path_exists_any(&macos_app_paths()).is_none() {
            return Err("Lyrion Music Server.app not found in /Applications. Install it first.".into());
        }
        // Prefer the official start-server.sh (same as the LMS menu bar item).
        if let Err(e) = run_macos_server_script("start-server.sh") {
            eprintln!("start-server.sh: {e}");
            if let Some(path) = path_exists_any(&macos_app_paths()) {
                let _ = Command::new("open").arg(&path).spawn();
            }
        }
    } else if cfg!(target_os = "windows") {
        // Prefer net start if service name is known; otherwise open web UI once up
        let _ = Command::new("net").args(["start", "Lyrion Music Server"]).status();
        let _ = Command::new("net").args(["start", "squeezesvc"]).status();
        let _ = Command::new("net").args(["start", "Logitech Media Server"]).status();
    } else if cfg!(target_os = "linux") {
        let _ = Command::new("systemctl")
            .args(["--user", "start", "lyrionmusicserver"])
            .status();
        let _ = Command::new("pkexec")
            .args(["systemctl", "start", "lyrionmusicserver"])
            .status();
        let _ = Command::new("pkexec")
            .args(["systemctl", "start", "logitechmediaserver"])
            .status();
    }

    // Poll localhost up to ~90s
    for i in 0..45 {
        if http_server_alive("127.0.0.1", 9000).await {
            emit_progress(&app, "ready", Some(100), "Serveur prêt");
            #[cfg(target_os = "macos")]
            macos_user_notification("Lyrion Music Server", "Serveur prêt");
            let mut payload = HashMap::new();
            payload.insert("host".to_string(), "127.0.0.1".to_string());
            payload.insert("port".to_string(), "9000".to_string());
            let _ = app.emit_all("lms-local-ready", payload);
            return lms_install_status().await;
        }
        emit_progress(
            &app,
            "start",
            Some(((i as f32 / 45.0) * 100.0) as u8),
            "En attente du serveur local…",
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    emit_progress(
        &app,
        "error",
        None,
        "Le serveur local ne répond pas. Dans la barre des menus, ouvrez Lyrion Music Server, puis Start Service.",
    );
    lms_install_status().await
}

/// Stop the local LMS process / service.
#[tauri::command]
pub async fn lms_stop_local(app: AppHandle) -> Result<LmsInstallStatus, String> {
    emit_progress(&app, "stop", Some(0), "Stopping local server…");

    if cfg!(target_os = "macos") {
        if let Err(e) = run_macos_server_script("stop-server.sh") {
            eprintln!("stop-server.sh: {e}");
            // Fallback: kill slimserver.pl
            let _ = Command::new("pkill")
                .args(["-f", "slimserver.pl"])
                .status();
        }
    } else if cfg!(target_os = "windows") {
        let _ = Command::new("net").args(["stop", "Lyrion Music Server"]).status();
        let _ = Command::new("net").args(["stop", "squeezesvc"]).status();
        let _ = Command::new("net").args(["stop", "Logitech Media Server"]).status();
    } else if cfg!(target_os = "linux") {
        let _ = Command::new("systemctl")
            .args(["--user", "stop", "lyrionmusicserver"])
            .status();
        let _ = Command::new("pkexec")
            .args(["systemctl", "stop", "lyrionmusicserver"])
            .status();
        let _ = Command::new("pkexec")
            .args(["systemctl", "stop", "logitechmediaserver"])
            .status();
    }

    for _ in 0..15 {
        if !http_server_alive("127.0.0.1", 9000).await {
            emit_progress(&app, "stop", Some(100), "Server stopped");
            return lms_install_status().await;
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    emit_progress(&app, "stop", Some(100), "Stop requested");
    lms_install_status().await
}

/// Open LMS server settings in the default browser (fallback if the in-app dialog cannot).
#[tauri::command]
pub fn lms_open_material() -> Result<(), String> {
    open_url("http://127.0.0.1:9000/material/settings/server/basic.html")
}

/// Poll until local (or given) server answers HTTP, or timeout.
#[tauri::command]
pub async fn lms_wait_ready(host: Option<String>, port: Option<u16>, timeout_secs: Option<u64>) -> Result<bool, String> {
    let host = host.unwrap_or_else(|| "127.0.0.1".into());
    let port = port.unwrap_or(9000);
    let timeout = timeout_secs.unwrap_or(90);
    let steps = (timeout / 2).max(1);
    for _ in 0..steps {
        if http_server_alive(&host, port).await {
            return Ok(true);
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Ok(false)
}

/// Open the official getting-started page in the browser.
#[tauri::command]
pub fn lms_open_getting_started() -> Result<(), String> {
    let url = if cfg!(target_os = "macos") {
        "https://lyrion.org/getting-started/mac-install/"
    } else {
        "https://lyrion.org/getting-started/"
    };
    open_url(url)
}

pub(crate) fn open_url(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg(url).spawn().map_err(|e| e.to_string())?;
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    {
        Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    {
        Command::new("xdg-open").arg(url).spawn().map_err(|e| e.to_string())?;
        return Ok(());
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = url;
        Err("open url unsupported".into())
    }
}
