//! First-run wizard: seed Material + slim philippe44 bridges into the user
//! Plugins folder, chmod helpers, then apply prefs once LMS is up.

use crate::lms_install::{download_file, emit_progress, lms_download_and_install, lms_install_status, lms_start_local};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use tauri::AppHandle;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WizardOptions {
    pub server_name: String,
    pub library_path: String,
    pub language: String,
    pub channel: String,
    pub group_library: bool,
    pub group_streaming: bool,
    pub group_mix: bool,
    pub group_endpoints: bool,
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// User-managed plugins dir (NOT Cache/InstalledPlugins — LMS wipes that).
fn plugins_dir() -> PathBuf {
    if cfg!(target_os = "macos") {
        home_dir().join("Library/Application Support/Squeezebox/Plugins")
    } else if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(home_dir)
            .join("Squeezebox/Plugins")
    } else {
        home_dir().join(".lms/Plugins")
    }
}

fn helper_name(stem: &str) -> String {
    if cfg!(target_os = "windows") {
        return format!("{}.exe", stem);
    }
    let os = if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    let arch = if cfg!(target_arch = "aarch64") {
        if cfg!(target_os = "macos") {
            "arm64"
        } else {
            "aarch64"
        }
    } else if cfg!(target_os = "macos") {
        "x86_64"
    } else {
        "x86_64"
    };
    format!("{}-{}-{}-static", stem, os, arch)
}

fn make_executable(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(path).map_err(|e| format!("stat {}: {}", path.display(), e))?;
        let mut perms = meta.permissions();
        perms.set_mode(0o555);
        std::fs::set_permissions(path, perms)
            .map_err(|e| format!("chmod {}: {}", path.display(), e))?;
        if cfg!(target_os = "macos") {
            let _ = Command::new("xattr")
                .args(["-d", "com.apple.quarantine"])
                .arg(path)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }
    let _ = path;
    Ok(())
}

fn extract_zip(zip: &Path, dest: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| format!("mkdir {}: {}", dest.display(), e))?;
    #[cfg(target_os = "macos")]
    {
        let st = Command::new("ditto")
            .args(["-x", "-k"])
            .arg(zip)
            .arg(dest)
            .status()
            .map_err(|e| format!("ditto: {}", e))?;
        if !st.success() {
            return Err(format!("ditto unzip failed ({})", st.code().unwrap_or(-1)));
        }
        return Ok(());
    }
    #[cfg(not(target_os = "macos"))]
    {
        let st = Command::new("unzip")
            .args(["-o", "-q"])
            .arg(zip)
            .arg("-d")
            .arg(dest)
            .status()
            .map_err(|e| format!("unzip: {}", e))?;
        if !st.success() {
            return Err(format!("unzip failed ({})", st.code().unwrap_or(-1)));
        }
        Ok(())
    }
}

async fn github_json(url: &str) -> Result<serde_json::Value, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent("Lyrion/0.3 (desktop; +https://lyrion.org)")
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client.get(url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("GitHub HTTP {} for {}", resp.status(), url));
    }
    resp.json().await.map_err(|e| e.to_string())
}

async fn seed_material(app: &AppHandle) -> Result<(), String> {
    emit_progress(app, "material", Some(5), "Resolving Material Skin release…");
    let json = github_json("https://api.github.com/repos/CDrummond/lms-material/releases/latest").await?;
    let tag = json
        .get("tag_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "No Material release tag".to_string())?;
    let url = format!(
        "https://github.com/CDrummond/lms-material/releases/download/{tag}/lms-material-{tag}.zip"
    );
    let zip = std::env::temp_dir().join(format!("lms-material-{tag}.zip"));
    emit_progress(app, "material", Some(15), format!("Downloading Material {tag}…"));
    download_file(app, &url, &zip).await?;

    let dest = plugins_dir().join("MaterialSkin");
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
    let unpack = std::env::temp_dir().join(format!("lms-material-unpacked-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&unpack);
    extract_zip(&zip, &unpack)?;

    // Zip may be MaterialSkin/* or files at root.
    let src = if unpack.join("MaterialSkin").is_dir() {
        unpack.join("MaterialSkin")
    } else if unpack.join("Plugin.pm").is_file() {
        unpack.clone()
    } else {
        // one nested folder
        std::fs::read_dir(&unpack)
            .ok()
            .and_then(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .find(|p| p.is_dir() && p.join("Plugin.pm").is_file())
            })
            .unwrap_or(unpack.clone())
    };
    copy_dir(&src, &dest)?;
    let _ = std::fs::remove_dir_all(&unpack);
    emit_progress(app, "material", Some(100), "Material Skin installed");
    Ok(())
}

fn copy_dir(src: &Path, dest: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    for ent in std::fs::read_dir(src).map_err(|e| e.to_string())? {
        let ent = ent.map_err(|e| e.to_string())?;
        let from = ent.path();
        let to = dest.join(ent.file_name());
        if from.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            std::fs::copy(&from, &to).map_err(|e| format!("copy {}: {}", from.display(), e))?;
        }
    }
    Ok(())
}

struct BridgeSpec {
    repo: &'static str,
    plugin_name: &'static str,
    stem: &'static str,
    files: &'static [&'static str],
}

const BRIDGES: &[BridgeSpec] = &[
    BridgeSpec {
        repo: "philippe44/LMS-uPnP",
        plugin_name: "UPnPBridge",
        stem: "squeeze2upnp",
        files: &[
            "install.xml",
            "Plugin.pm",
            "Settings.pm",
            "Squeeze2upnp.pm",
        ],
    },
    BridgeSpec {
        repo: "philippe44/LMS-Cast",
        plugin_name: "CastBridge",
        stem: "squeeze2cast",
        files: &[
            "install.xml",
            "Plugin.pm",
            "Settings.pm",
            "Squeeze2cast.pm",
        ],
    },
    BridgeSpec {
        repo: "philippe44/LMS-RAOP",
        plugin_name: "RaopBridge",
        stem: "squeeze2raop",
        files: &[
            "install.xml",
            "Plugin.pm",
            "Settings.pm",
            "Squeeze2raop.pm",
        ],
    },
];

async fn seed_bridges(app: &AppHandle) -> Result<(), String> {
    let n = BRIDGES.len();
    for (i, spec) in BRIDGES.iter().enumerate() {
        let pct = (20 + (i * 70) / n) as u8;
        emit_progress(
            app,
            "bridges",
            Some(pct),
            format!("Installing {}…", spec.plugin_name),
        );
        seed_one_bridge(app, spec).await?;
    }
    emit_progress(app, "bridges", Some(100), "Local endpoint bridges installed");
    Ok(())
}

async fn seed_one_bridge(app: &AppHandle, spec: &BridgeSpec) -> Result<(), String> {
    let dest = plugins_dir().join(spec.plugin_name);
    std::fs::create_dir_all(dest.join("Bin")).map_err(|e| e.to_string())?;

    for file in spec.files {
        let url = format!(
            "https://raw.githubusercontent.com/{}/master/plugin/{}",
            spec.repo, file
        );
        let path = dest.join(file);
        match download_file(app, &url, &path).await {
            Ok(()) => {}
            Err(e) => {
                eprintln!("optional {}/{}: {}", spec.plugin_name, file, e);
            }
        }
    }

    let bin = helper_name(spec.stem);
    let mut urls = vec![format!(
        "https://github.com/{}/raw/master/plugin/Bin/{}",
        spec.repo, bin
    )];
    // Fallbacks: non-static, then generic macos name
    if bin.ends_with("-static") {
        urls.push(urls[0].trim_end_matches("-static").to_string());
    }
    if cfg!(target_os = "macos") {
        urls.push(format!(
            "https://github.com/{}/raw/master/plugin/Bin/{}-macos-static",
            spec.repo, spec.stem
        ));
    }

    let bin_path = dest.join("Bin").join(&bin);
    let mut last = String::new();
    let mut ok = false;
    for url in &urls {
        match download_file(app, url, &bin_path).await {
            Ok(()) => {
                ok = true;
                break;
            }
            Err(e) => last = e,
        }
    }
    if !ok {
        return Err(format!(
            "Could not download {} helper for this machine: {}",
            spec.plugin_name, last
        ));
    }
    make_executable(&bin_path)?;
    Ok(())
}

async fn jsonrpc(method: Vec<serde_json::Value>) -> Result<serde_json::Value, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let body = serde_json::json!({
        "id": 1,
        "method": "slim.request",
        "params": ["", method],
    });
    let resp = client
        .post("http://127.0.0.1:9000/jsonrpc.js")
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("jsonrpc HTTP {}", resp.status()));
    }
    resp.json().await.map_err(|e| e.to_string())
}

async fn apply_prefs(opts: &WizardOptions) -> Result<(), String> {
    let lang = if opts.language.trim().is_empty() {
        "EN"
    } else {
        opts.language.trim()
    };
    let _ = jsonrpc(vec![
        serde_json::json!("pref"),
        serde_json::json!("language"),
        serde_json::json!(lang),
    ])
    .await;
    if !opts.server_name.trim().is_empty() {
        let _ = jsonrpc(vec![
            serde_json::json!("pref"),
            serde_json::json!("libraryname"),
            serde_json::json!(opts.server_name.trim()),
        ])
        .await;
    }
    let lib = opts.library_path.trim();
    if !lib.is_empty() {
        let _ = jsonrpc(vec![
            serde_json::json!("pref"),
            serde_json::json!("mediadirs"),
            serde_json::json!(vec![lib]),
        ])
        .await;
        let _ = jsonrpc(vec![
            serde_json::json!("rescan"),
        ])
        .await;
    }
    // Built-in extras
    if opts.group_mix {
        let _ = jsonrpc(vec![
            serde_json::json!("pref"),
            serde_json::json!("plugin.dontstopthemusic:enabled"),
            serde_json::json!(1),
        ])
        .await;
    }
    Ok(())
}

#[tauri::command]
pub fn lms_machine_name() -> String {
    #[cfg(target_os = "macos")]
    {
        if let Ok(out) = Command::new("scutil").args(["--get", "ComputerName"]).output() {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !s.is_empty() {
                return s;
            }
        }
    }
    Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if s.is_empty() {
                None
            } else {
                Some(s.replace(".local", ""))
            }
        })
        .unwrap_or_else(|| "Lyrion".into())
}

/// Native folder picker (macOS: choose folder; elsewhere returns None).
#[tauri::command]
pub async fn lms_pick_library_folder() -> Result<Option<String>, String> {
    #[cfg(target_os = "macos")]
    {
        let out = Command::new("osascript")
            .args([
                "-e",
                "try\nPOSIX path of (choose folder with prompt \"Choose your music library\")\non error\nreturn \"\"\nend try",
            ])
            .output()
            .map_err(|e| format!("osascript: {}", e))?;
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if s.is_empty() {
            return Ok(None);
        }
        return Ok(Some(s));
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(None)
    }
}

/// Full local-install path used by the first-run wizard.
#[tauri::command]
pub async fn lms_wizard_run(app: AppHandle, opts: WizardOptions) -> Result<String, String> {
    std::fs::create_dir_all(plugins_dir()).map_err(|e| e.to_string())?;

    emit_progress(&app, "lms", Some(0), "Installing Lyrion Music Server…");
    let status = lms_install_status().await?;
    if !status.app_installed {
        lms_download_and_install(app.clone(), Some(opts.channel.clone()), None).await?;
    } else {
        emit_progress(&app, "lms", Some(100), "Lyrion Music Server already installed");
    }

    if let Err(e) = seed_material(&app).await {
        emit_progress(&app, "material", None, format!("Material: {}", e));
        return Err(e);
    }

    if opts.group_endpoints {
        seed_bridges(&app).await?;
    }

    emit_progress(&app, "start", Some(0), "Starting local server…");
    let st = lms_start_local(app.clone()).await?;
    if !st.server_running {
        return Err(st.message);
    }

    emit_progress(&app, "prefs", Some(80), "Applying server settings…");
    let _ = apply_prefs(&opts).await;

    // Best-effort 3rd-party plugins via LMS repo once the server is up
    if opts.group_library {
        for name in ["MusicArtistInfo", "RatingsLight", "ContextStats"] {
            let _ = jsonrpc(vec![
                serde_json::json!("apps"),
                serde_json::json!("install"),
                serde_json::json!(name),
            ])
            .await;
        }
    }
    if opts.group_streaming {
        for name in ["Spotty", "Qobuz", "YouTube"] {
            let _ = jsonrpc(vec![
                serde_json::json!("apps"),
                serde_json::json!("install"),
                serde_json::json!(name),
            ])
            .await;
        }
    }

    emit_progress(&app, "ready", Some(100), "Server is ready");
    Ok("http://127.0.0.1:9000/material/".into())
}
