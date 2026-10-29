// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::{
    CustomMenuItem, GlobalShortcutManager, Manager, SystemTray, SystemTrayEvent, SystemTrayMenu,
    SystemTrayMenuItem, SystemTraySubmenu,
};
use tauri_plugin_positioner::{Position, WindowExt};
use discover::discover;
mod discover;
mod lms_install;
mod lms_plugins;
mod shell_proxy;

/// True once the frontend has restored a saved window position.
static HAS_SAVED_POSITION: AtomicBool = AtomicBool::new(false);

/// When true: show Dock icon, do not auto-hide on focus loss (free-floating window).
static SHOW_DOCK_ICON: AtomicBool = AtomicBool::new(false);

/// Release shell may be loaded once IPC allowlist and the proxy are up.
#[cfg(all(target_os = "macos", not(debug_assertions)))]
static SHELL_NAV_READY: AtomicBool = AtomicBool::new(false);

/// Read `window.showDockIcon` from the on-disk config before the webview boots.
/// Default false (menubar popup) matches first-run; a saved true must win
/// immediately so FocusLost cannot hide the window before Vue restores prefs.
fn load_config_json(app: &tauri::App) -> Option<serde_json::Value> {
    let dir = app.path_resolver().app_config_dir()?;
    let text = std::fs::read_to_string(dir.join("config.json")).ok()?;
    serde_json::from_str(&text).ok()
}

fn load_show_dock_icon_pref(app: &tauri::App) -> bool {
    load_config_json(app)
        .as_ref()
        .and_then(|v| v.get("window"))
        .and_then(|w| w.get("showDockIcon").or_else(|| w.get("detached")))
        .and_then(|x| x.as_bool())
        .unwrap_or(false)
}

fn load_window_geometry(app: &tauri::App) -> Option<(f64, f64, f64, f64)> {
    let win = load_config_json(app)?.get("window")?.clone();
    let w = win.get("width")?.as_f64().unwrap_or(0.0);
    let h = win.get("height")?.as_f64().unwrap_or(0.0);
    let x = win.get("x")?.as_f64()?;
    let y = win.get("y")?.as_f64()?;
    if w < 320.0 || h < 480.0 {
        return None;
    }
    Some((w, h, x, y))
}

/// Currently registered global show/hide accelerator.
static CURRENT_HOTKEY: Mutex<String> = Mutex::new(String::new());

/// Default global hotkey (macOS-friendly, avoids Spotlight).
const DEFAULT_TOGGLE_HOTKEY: &str = "CmdOrControl+Shift+Space";

#[derive(Clone)]
struct MenuServer {
    host: String,
    port: String,
    name: String,
    is_local: bool,
}

static MENU_SERVERS: Mutex<Vec<MenuServer>> = Mutex::new(Vec::new());
pub(crate) static CURRENT_LMS: Mutex<Option<(String, String)>> = Mutex::new(None);
static DEFAULT_LMS: Mutex<Option<(String, String)>> = Mutex::new(None);
static MENU_LMS_FLAGS: Mutex<(bool, bool)> = Mutex::new((false, false)); // running, installed
static MENU_FINGERPRINT: Mutex<String> = Mutex::new(String::new());
/// Last menubar icon rect (physical px): x, y, width, height.
/// On macOS, tao's y is the bottom edge of the status item, not its top.
static TRAY_GEOM: Mutex<Option<(f64, f64, f64, f64)>> = Mutex::new(None);
static APP_HANDLE: Mutex<Option<tauri::AppHandle>> = Mutex::new(None);
static PLAYERS_SHOWN_AT: Mutex<Option<Instant>> = Mutex::new(None);
static PLAYERS_VIBRANCY: AtomicBool = AtomicBool::new(false);
static PLAYERS_FADE_GEN: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct MenuPlayer {
    id: String,
    name: String,
}

static MENU_PLAYERS: Mutex<Vec<MenuPlayer>> = Mutex::new(Vec::new());
static CURRENT_PLAYER: Mutex<Option<String>> = Mutex::new(None);

#[derive(Clone, serde::Deserialize, Default)]
struct PlatineChoice {
    key: String,
    label: String,
    selected: bool,
}

/// One row in Platine > Synchroniser. `selected` is membership in the current player's sync group.
#[derive(Clone, serde::Deserialize, Default)]
struct PlatinePlayer {
    id: String,
    name: String,
    #[serde(default)]
    selected: bool,
}

#[derive(Clone, serde::Deserialize, Default)]
struct PlatineMenuState {
    enabled: bool,
    /// 0 = not sleeping. Any positive value means a sleep timer is running.
    sleep: i64,
    synced: bool,
    sync_master: String,
    players: Vec<PlatinePlayer>,
    can_transfer: bool,
    inputs: Vec<PlatineChoice>,
    outputs: Vec<PlatineChoice>,
    dsp: Vec<PlatineChoice>,
    dsp_on: bool,
    dsp_known: bool,
    presets: Vec<PlatineChoice>,
    /// Stable sleep choice while a timer runs. Empty or `platine_sleep_0` when awake.
    #[serde(default)]
    sleep_key: String,
}

static PLATINE_MENU: Mutex<PlatineMenuState> = Mutex::new(PlatineMenuState {
    enabled: false,
    sleep: 0,
    synced: false,
    sync_master: String::new(),
    players: Vec::new(),
    can_transfer: false,
    inputs: Vec::new(),
    outputs: Vec::new(),
    dsp: Vec::new(),
    dsp_on: false,
    dsp_known: false,
    presets: Vec::new(),
    sleep_key: String::new(),
});
static PLATINE_FP: Mutex<String> = Mutex::new(String::new());



fn current_lms_active() -> bool {
    CURRENT_LMS
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .map(|(h, _)| !h.trim().is_empty())
        .unwrap_or(false)
}

#[tauri::command]
fn set_current_lms(app: tauri::AppHandle, host: String, port: u16) {
    let h = host.trim().to_string();
    if h.is_empty() {
        *CURRENT_LMS.lock().unwrap() = None;
    } else {
        *CURRENT_LMS.lock().unwrap() = Some((h, port.to_string()));
    }
    rebuild_menus_if_changed(&app);
}

#[tauri::command]
fn set_default_lms(host: String, port: u16) {
    *DEFAULT_LMS.lock().unwrap() = Some((host.trim().to_string(), port.to_string()));
}

#[tauri::command]
fn set_current_player(id: Option<String>) {
    *CURRENT_PLAYER.lock().unwrap() = id.and_then(|s| {
        let t = s.trim().to_string();
        if t.is_empty() {
            None
        } else {
            Some(t)
        }
    });
}

#[tauri::command]
fn get_current_player() -> Option<String> {
    CURRENT_PLAYER.lock().unwrap().clone()
}

fn place_players_hud(window: &tauri::Window) {
    let Ok(size) = window.outer_size() else {
        return;
    };
    let scale = window.scale_factor().unwrap_or(1.0);
    let screen = window.current_monitor().ok().flatten();
    let mut screen_w = screen
        .as_ref()
        .map(|m| m.size().width as i32)
        .unwrap_or(size.width as i32);
    let mut screen_x = screen.as_ref().map(|m| m.position().x).unwrap_or(0);

    let tray = TRAY_GEOM.lock().unwrap().clone();
    // tao reports the status item's bottom as y. Adding the icon height
    // parks the panel a full menu-bar below the bar.
    let (tray_cx, mut y) = if let Some((tx, ty, tw, th)) = tray {
        let bottom = if cfg!(target_os = "macos") { ty } else { ty + th };
        (tx + tw / 2.0, bottom.round() as i32)
    } else {
        let _ = window.move_window(Position::TrayBottomCenter);
        if let Ok(pos) = window.outer_position() {
            (
                pos.x as f64 + (size.width as f64) / 2.0,
                pos.y,
            )
        } else {
            return;
        }
    };

    #[cfg(target_os = "macos")]
    if let Some(bar) = players_menu_bar(window, tray_cx) {
        y = bar.y;
        screen_x = bar.screen_x;
        screen_w = bar.screen_w;
    }

    let margin = (8.0 * scale) as i32;
    let mut x = (tray_cx - (size.width as f64) / 2.0).round() as i32;
    let max_x = screen_x + screen_w - size.width as i32 - margin;
    x = x.clamp(screen_x + margin, max_x.max(screen_x + margin));
    let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
}

#[cfg(target_os = "macos")]
struct PlayersMenuBar {
    y: i32,
    screen_x: i32,
    screen_w: i32,
}

/// Top of the players panel, flush with the bottom of the menu bar.
/// AppKit y is inverted with the same `pixels_high - logical` tao uses for
/// `setFrameTopLeftPoint`, so a Retina display stays in that coordinate space.
#[cfg(target_os = "macos")]
fn players_menu_bar(window: &tauri::Window, tray_x: f64) -> Option<PlayersMenuBar> {
    if !macos_is_main_thread() {
        return None;
    }
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};

    #[repr(C)]
    struct NsPoint {
        x: f64,
        y: f64,
    }
    #[repr(C)]
    struct NsSize {
        width: f64,
        height: f64,
    }
    #[repr(C)]
    struct NsRect {
        origin: NsPoint,
        size: NsSize,
    }

    let window_scale = window.scale_factor().unwrap_or(1.0);
    let main_h = main_display_pixels_high();
    unsafe {
        let screens: *mut Object = msg_send![class!(NSScreen), screens];
        if screens.is_null() {
            return None;
        }
        let count: usize = msg_send![screens, count];
        let mut fallback: Option<PlayersMenuBar> = None;
        for i in 0..count {
            let screen: *mut Object = msg_send![screens, objectAtIndex: i];
            if screen.is_null() {
                continue;
            }
            let frame: NsRect = msg_send![screen, frame];
            let visible: NsRect = msg_send![screen, visibleFrame];
            let screen_scale: f64 = msg_send![screen, backingScaleFactor];
            let scale = if screen_scale > 0.0 {
                screen_scale
            } else {
                window_scale
            };
            let left = frame.origin.x * scale;
            let width = frame.size.width * scale;
            let right = left + width;
            if tray_x < left - 2.0 || tray_x > right + 2.0 {
                continue;
            }
            let visible_top = visible.origin.y + visible.size.height;
            let menu = (frame.origin.y + frame.size.height) - visible_top;
            let place = PlayersMenuBar {
                y: ((main_h - visible_top) * window_scale).round() as i32,
                screen_x: left.round() as i32,
                screen_w: width.round() as i32,
            };
            if menu > 0.5 {
                return Some(place);
            }
            fallback = Some(place);
        }
        fallback
    }
}

#[cfg(target_os = "macos")]
fn macos_is_main_thread() -> bool {
    use objc::runtime::{BOOL, YES};
    use objc::{class, msg_send, sel, sel_impl};
    unsafe {
        let main: BOOL = msg_send![class!(NSThread), isMainThread];
        main == YES
    }
}

#[cfg(target_os = "macos")]
fn main_display_pixels_high() -> f64 {
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGMainDisplayID() -> u32;
        fn CGDisplayPixelsHigh(display: u32) -> u64;
    }
    unsafe { CGDisplayPixelsHigh(CGMainDisplayID()) as f64 }
}

#[tauri::command]
fn position_players_window(app: tauri::AppHandle) {
    let Some(window) = app.get_window("players") else {
        return;
    };
    let win = window.clone();
    if window
        .run_on_main_thread(move || place_players_hud(&win))
        .is_err()
    {
        if let Some(again) = app.get_window("players") {
            place_players_hud(&again);
        }
    }
}

#[tauri::command]
fn set_menu_players(app: tauri::AppHandle, players: Vec<MenuPlayer>, current: Option<String>) {
    *MENU_PLAYERS.lock().unwrap() = players;
    if let Some(id) = current {
        let t = id.trim().to_string();
        if !t.is_empty() {
            *CURRENT_PLAYER.lock().unwrap() = Some(t);
        }
    }
    rebuild_menus_if_changed(&app);
}

#[tauri::command]
fn set_platine_menu(app: tauri::AppHandle, state: PlatineMenuState) {
    let fp = format!(
        "{}|{}|{}|{}|{}|{}|{:?}|{:?}|{:?}|{:?}|{}|{}",
        state.enabled,
        state.sleep,
        state.sleep_key,
        state.synced,
        state.sync_master,
        state.can_transfer,
        state
            .players
            .iter()
            .map(|p| format!("{}:{}:{}", p.id, p.name, p.selected))
            .collect::<Vec<_>>(),
        state
            .inputs
            .iter()
            .map(|c| format!("{}:{}:{}", c.key, c.label, c.selected))
            .collect::<Vec<_>>(),
        state
            .outputs
            .iter()
            .map(|c| format!("{}:{}:{}", c.key, c.label, c.selected))
            .collect::<Vec<_>>(),
        state
            .dsp
            .iter()
            .map(|c| format!("{}:{}:{}", c.key, c.label, c.selected))
            .collect::<Vec<_>>(),
        state.dsp_on,
        state.dsp_known,
    ) + &state
        .presets
        .iter()
        .map(|c| format!("{}:{}:{}", c.key, c.label, c.selected))
        .collect::<Vec<_>>()
        .join(",");
    {
        let mut cur = PLATINE_FP.lock().unwrap();
        if *cur == fp {
            return;
        }
        *cur = fp;
    }
    *PLATINE_MENU.lock().unwrap() = state;
    rebuild_platine_menu(&app);
}

#[tauri::command]
fn this_machine_hosts() -> Vec<String> {
    local_ipv4_addrs().clone()
}

#[tauri::command]
async fn detect_lms_server() -> HashMap<String, String> {
    discover().await.unwrap_or_default()
}

#[tauri::command]
async fn detect_lms_servers() -> Vec<HashMap<String, String>> {
    discover::discover_all(std::time::Duration::from_secs(3))
        .await
        .unwrap_or_default()
}

/// Called by the frontend after it restores geometry from config.
#[tauri::command]
fn mark_window_positioned() {
    HAS_SAVED_POSITION.store(true, Ordering::SeqCst);
}

#[tauri::command]
fn is_show_dock_icon() -> bool {
    SHOW_DOCK_ICON.load(Ordering::SeqCst)
}

/// Show or hide the Dock icon (and free-floating vs menubar-popup behavior).
#[tauri::command]
fn set_show_dock_icon(app: tauri::AppHandle, show: bool) -> Result<bool, String> {
    apply_show_dock_icon(&app, show);
    Ok(show)
}

/// Legacy command name — keep so older frontends still work.
#[tauri::command]
fn set_detached(app: tauri::AppHandle, detached: bool) -> Result<bool, String> {
    set_show_dock_icon(app, detached)
}

#[tauri::command]
fn is_detached() -> bool {
    is_show_dock_icon()
}

#[tauri::command]
fn get_toggle_hotkey() -> String {
    CURRENT_HOTKEY
        .lock()
        .map(|g| {
            if g.is_empty() {
                DEFAULT_TOGGLE_HOTKEY.to_string()
            } else {
                g.clone()
            }
        })
        .unwrap_or_else(|_| DEFAULT_TOGGLE_HOTKEY.to_string())
}

/// Register (or replace) the global show/hide hotkey. Returns the accelerator on success.
#[tauri::command]
fn set_toggle_hotkey(app: tauri::AppHandle, accelerator: String) -> Result<String, String> {
    let accel = accelerator.trim().to_string();
    if accel.is_empty() {
        return Err("Hotkey cannot be empty".into());
    }
    register_toggle_hotkey(&app, &accel)?;
    // Refresh tray label for Show/Hide
    let _ = app
        .tray_handle()
        .get_item("toggle")
        .set_title(format!("Show / Hide ({})", accel));
    Ok(accel)
}

fn register_toggle_hotkey(app: &tauri::AppHandle, accelerator: &str) -> Result<(), String> {
    let mut gsm = app.global_shortcut_manager();
    let mut current = CURRENT_HOTKEY
        .lock()
        .map_err(|e| format!("hotkey lock: {}", e))?;

    if !current.is_empty() {
        let _ = gsm.unregister(current.as_str());
    }

    let handle = app.clone();
    gsm.register(accelerator, move || {
        toggle_main_window(&handle);
    })
    .map_err(|e| format!("Unable to register '{}': {}", accelerator, e))?;

    *current = accelerator.to_string();
    println!("Registered global toggle: {}", accelerator);
    Ok(())
}

fn apply_show_dock_icon(app: &tauri::AppHandle, show: bool) {
    SHOW_DOCK_ICON.store(show, Ordering::SeqCst);

    if let Some(window) = app.get_window("main") {
        if show {
            let _ = window.show();
            let _ = window.set_focus();
        }
    }

    let title = if show {
        if cfg!(target_os = "macos") {
            "Hide Dock Icon"
        } else {
            "Hide window when unfocused"
        }
    } else if cfg!(target_os = "macos") {
        "Show Dock Icon"
    } else {
        "Keep window open"
    };
    let _ = app.tray_handle().get_item("dock").set_title(title);

    #[cfg(target_os = "macos")]
    set_macos_activation_policy(show);

    let _ = app.emit_all("dock-icon-changed", show);
    let _ = app.emit_all("detached-changed", show);
    if !show {
        hide_players_hud(app);
    }
}

/// NSApplicationActivationPolicyRegular = 0, Accessory = 1
#[cfg(target_os = "macos")]
fn set_macos_activation_policy(show_dock: bool) {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    unsafe {
        let ns_app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        if !ns_app.is_null() {
            let policy: i64 = if show_dock { 0 } else { 1 };
            let _: bool = msg_send![ns_app, setActivationPolicy: policy];
            if show_dock {
                let _: () = msg_send![ns_app, activateIgnoringOtherApps: true];
            }
        }
    }
}

fn apply_window_vibrancy(window: &tauri::Window) {
    #[cfg(target_os = "macos")]
    {
        // Keep the window opaque. A transparent WKWebView does not composite
        // cross-origin iframes (Material), which reads as a frozen blank window.
        apply_macos_window_chrome(window);
    }

    #[cfg(target_os = "windows")]
    {
        use window_vibrancy::apply_blur;
        let _ = apply_blur(window, Some((18, 18, 18, 125)));
    }
}

/// Transparent HUD with native frost. Re-applied on show so blur does not drop.
fn apply_players_hud_chrome(window: &tauri::Window) {
    let _ = window.set_resizable(true);
    #[cfg(target_os = "macos")]
    {
        use objc::runtime::{Class, Object};
        use objc::{msg_send, sel, sel_impl};
        let ns_window: *mut Object = match window.ns_window() {
            Ok(ptr) => ptr as *mut Object,
            Err(_) => return,
        };
        if ns_window.is_null() {
            return;
        }
        unsafe {
            let _: () = msg_send![ns_window, setOpaque: false];
            let _: () = msg_send![ns_window, setHasShadow: true];
            let _: () = msg_send![ns_window, setAlphaValue: 1.0_f64];
            if let Some(ns_color) = Class::get("NSColor") {
                let clear: *mut Object = msg_send![ns_color, clearColor];
                if !clear.is_null() {
                    let _: () = msg_send![ns_window, setBackgroundColor: clear];
                }
            }
            let content: *mut Object = msg_send![ns_window, contentView];
            clear_hud_views(content);
            // Square mask. A 10px radius clips the top corners and leaves a
            // gap under the menu bar that CSS cannot fill back in.
            round_hud_views(content, 0.0);
            // Catalina paints a transparent WKWebView as an opaque black plate,
            // which hides the vibrancy view and shifts the content.
            pin_webview_surface(ns_window, true);
        }
        if !PLAYERS_VIBRANCY.swap(true, Ordering::SeqCst) {
            let _ = window_vibrancy::apply_vibrancy(
                window,
                window_vibrancy::NSVisualEffectMaterial::HudWindow,
                Some(window_vibrancy::NSVisualEffectState::Active),
                Some(0.0),
            );
        }
        unsafe {
            let content: *mut Object = msg_send![ns_window, contentView];
            round_hud_views(content, 0.0);
        }
    }
}

#[cfg(target_os = "macos")]
fn clear_hud_views(view: *mut objc::runtime::Object) {
    if view.is_null() {
        return;
    }
    use objc::runtime::{Class, Object};
    use objc::{msg_send, sel, sel_impl};
    use std::ffi::CStr;
    unsafe {
        let cls: *mut Object = msg_send![view, class];
        let name_obj: *mut Object = msg_send![cls, className];
        if !name_obj.is_null() {
            let utf8: *const i8 = msg_send![name_obj, UTF8String];
            if !utf8.is_null() {
                let name = CStr::from_ptr(utf8).to_string_lossy();
                if macos_major() < 11 && is_webview_class(&name) {
                    return;
                }
                if name.contains("WKWebView") || name.contains("WebView") {
                    if let Some(ns_str) = Class::get("NSString") {
                        let key: *mut Object =
                            msg_send![ns_str, stringWithUTF8String: b"drawsBackground\0".as_ptr()];
                        if let Some(ns_num) = Class::get("NSNumber") {
                            let no: *mut Object = msg_send![ns_num, numberWithBool: false];
                            let _: () = msg_send![view, setValue: no forKey: key];
                        }
                    }
                }
            }
        }
        let _: () = msg_send![view, setWantsLayer: true];
        let layer: *mut Object = msg_send![view, layer];
        if !layer.is_null() {
            if let Some(ns_color) = Class::get("NSColor") {
                let clear: *mut Object = msg_send![ns_color, clearColor];
                if !clear.is_null() {
                    let cg: *mut Object = msg_send![clear, CGColor];
                    let _: () = msg_send![layer, setBackgroundColor: cg];
                }
            }
        }
        let subviews: *mut Object = msg_send![view, subviews];
        if subviews.is_null() {
            return;
        }
        let n: usize = msg_send![subviews, count];
        for i in 0..n {
            let sub: *mut Object = msg_send![subviews, objectAtIndex: i];
            clear_hud_views(sub);
        }
    }
}

#[cfg(target_os = "macos")]
fn round_hud_views(view: *mut objc::runtime::Object, radius: f64) {
    if view.is_null() {
        return;
    }
    use objc::runtime::{Object, BOOL, YES};
    use objc::{msg_send, sel, sel_impl};
    unsafe {
        if macos_major() < 11 && is_webview_class(&view_class(view)) {
            return;
        }
        let _: () = msg_send![view, setWantsLayer: true];
        let clip_sel = sel!(setClipsToBounds:);
        let can_clip: BOOL = msg_send![view, respondsToSelector: clip_sel];
        if can_clip == YES {
            let _: () = msg_send![view, setClipsToBounds: YES];
        }
        let layer: *mut Object = msg_send![view, layer];
        if !layer.is_null() {
            let _: () = msg_send![layer, setCornerRadius: radius];
            let _: () = msg_send![layer, setMasksToBounds: YES];
        }
        let subviews: *mut Object = msg_send![view, subviews];
        if subviews.is_null() {
            return;
        }
        let n: usize = msg_send![subviews, count];
        for i in 0..n {
            let sub: *mut Object = msg_send![subviews, objectAtIndex: i];
            round_hud_views(sub, radius);
        }
    }
}

/// Native shadow + rounded content layer. Window stays opaque so Material's iframe paints.
#[cfg(target_os = "macos")]
fn apply_macos_window_chrome(window: &tauri::Window) {
    use objc::runtime::{Class, Object};
    use objc::{msg_send, sel, sel_impl};

    let ns_window: *mut Object = match window.ns_window() {
        Ok(ptr) => ptr as *mut Object,
        Err(_) => return,
    };
    if ns_window.is_null() {
        return;
    }
    unsafe {
        let _: () = msg_send![ns_window, setOpaque: true];
        let _: () = msg_send![ns_window, setHasShadow: true];
        let _: () = msg_send![ns_window, setAlphaValue: 1.0_f64];
        if let Some(ns_color) = Class::get("NSColor") {
            let rgb: *mut Object = msg_send![
                ns_color,
                colorWithCalibratedRed: 0.11_f64
                green: 0.11_f64
                blue: 0.118_f64
                alpha: 1.0_f64
            ];
            if !rgb.is_null() {
                let _: () = msg_send![ns_window, setBackgroundColor: rgb];
            }
        }
        darken_webview(ns_window);
        align_traffic_lights_ns(ns_window);
        // Opaque webview under the overlay titlebar. A transparent webview
        // shows the desktop on Catalina; a separate titlebar breaks the chrome.
        pin_webview_surface(ns_window, false);
    }
}

/// Material's main toolbar is 52px. Center the close / minimize / zoom
/// buttons in that band instead of leaving them on the top edge.
#[cfg(target_os = "macos")]
const TOOLBAR_BAND: f64 = 52.0;

/// Extra space between the window's left edge and the traffic lights.
/// The Material player name stays put (`--window-area-left` is unchanged).
/// Catalina's titlebar container is only as wide as the three buttons, so the
/// extra inset is applied from Big Sur upward.
#[cfg(target_os = "macos")]
const TRAFFIC_LEFT_EXTRA: f64 = 12.0;

#[cfg(target_os = "macos")]
fn macos_major() -> i32 {
    static MAJOR: OnceLock<i32> = OnceLock::new();
    *MAJOR.get_or_init(|| unsafe {
        use objc::runtime::Object;
        use objc::{class, msg_send, sel, sel_impl};
        let info: *mut Object = msg_send![class!(NSProcessInfo), processInfo];
        if info.is_null() {
            return 99;
        }
        let text: *mut Object = msg_send![info, operatingSystemVersionString];
        if text.is_null() {
            return 99;
        }
        let utf8: *const i8 = msg_send![text, UTF8String];
        if utf8.is_null() {
            return 99;
        }
        let version = std::ffi::CStr::from_ptr(utf8).to_string_lossy();
        let rest = version.trim_start_matches("Version ").trim();
        rest.split(['.', ' ', '('])
            .next()
            .and_then(|n| n.parse().ok())
            .unwrap_or(99)
    })
}

#[cfg(target_os = "macos")]
fn traffic_left_extra() -> f64 {
    if macos_major() >= 11 {
        TRAFFIC_LEFT_EXTRA
    } else {
        0.0
    }
}

#[cfg(target_os = "macos")]
struct TrafficBase {
    bar_x: Option<f64>,
    btn_x: [Option<f64>; 3],
}

#[cfg(target_os = "macos")]
static TRAFFIC_BASE: Mutex<TrafficBase> = Mutex::new(TrafficBase {
    bar_x: None,
    btn_x: [None, None, None],
});

/// Remember AppKit's original x once, then keep a fixed inset.
#[cfg(target_os = "macos")]
fn traffic_target_x(slot: &mut Option<f64>, current: f64) -> Option<f64> {
    if slot.is_none() {
        if current < 1.0 {
            return None;
        }
        *slot = Some(current);
    }
    Some(slot.unwrap_or(current) + traffic_left_extra())
}

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
struct NsPoint {
    x: f64,
    y: f64,
}

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
struct NsSize {
    width: f64,
    height: f64,
}

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
struct NsRect {
    origin: NsPoint,
    size: NsSize,
}

#[cfg(target_os = "macos")]
fn view_class(view: *mut objc::runtime::Object) -> String {
    unsafe {
        let class = objc::runtime::object_getClass(view);
        if class.is_null() {
            return String::new();
        }
        let cstr = objc::runtime::class_getName(class);
        if cstr.is_null() {
            return String::new();
        }
        std::ffi::CStr::from_ptr(cstr).to_string_lossy().into_owned()
    }
}

#[cfg(target_os = "macos")]
fn darken_view(view: *mut objc::runtime::Object) {
    if view.is_null() {
        return;
    }
    use objc::runtime::{Class, Object, BOOL, YES};
    use objc::{msg_send, sel, sel_impl};
    unsafe {
        if is_webview_class(&view_class(view)) {
            if let Some(ns_color) = Class::get("NSColor") {
                let rgb: *mut Object = msg_send![
                    ns_color,
                    colorWithCalibratedRed: 0.1098_f64
                    green: 0.1098_f64
                    blue: 0.1176_f64
                    alpha: 1.0_f64
                ];
                let sel_under = sel!(setUnderPageBackgroundColor:);
                let responds: BOOL = msg_send![view, respondsToSelector: sel_under];
                if responds == YES && !rgb.is_null() {
                    let _: () = msg_send![view, setUnderPageBackgroundColor: rgb];
                }
            }
        }
        let subviews: *mut Object = msg_send![view, subviews];
        if subviews.is_null() {
            return;
        }
        let n: usize = msg_send![subviews, count];
        for i in 0..n {
            let sub: *mut Object = msg_send![subviews, objectAtIndex: i];
            darken_view(sub);
        }
    }
}

#[cfg(target_os = "macos")]
fn darken_webview(ns_window: *mut objc::runtime::Object) {
    use objc::runtime::Object;
    use objc::{msg_send, sel, sel_impl};
    unsafe {
        let content: *mut Object = msg_send![ns_window, contentView];
        darken_view(content);
    }
}

/// Catalina clips the button row inside a titlebar view that is only as wide
/// as the three circles. Big Sur's overlay titlebar is the full window.
#[cfg(target_os = "macos")]
fn unclip_traffic_ancestors(start: *mut objc::runtime::Object) {
    if macos_major() >= 11 || start.is_null() {
        return;
    }
    use objc::runtime::{Object, BOOL, YES};
    use objc::{msg_send, sel, sel_impl};
    unsafe {
        let mut view = start;
        for _ in 0..6 {
            if view.is_null() {
                break;
            }
            let clip_sel = sel!(setClipsToBounds:);
            let can_clip: BOOL = msg_send![view, respondsToSelector: clip_sel];
            if can_clip == YES {
                let _: () = msg_send![view, setClipsToBounds: objc::runtime::NO];
            }
            let layer_sel = sel!(layer);
            let can_layer: BOOL = msg_send![view, respondsToSelector: layer_sel];
            if can_layer == YES {
                let layer: *mut Object = msg_send![view, layer];
                if !layer.is_null() {
                    let _: () = msg_send![layer, setMasksToBounds: objc::runtime::NO];
                }
            }
            let name = view_class(view);
            view = msg_send![view, superview];
            if name.contains("Content") || name.contains("ThemeFrame") {
                break;
            }
        }
    }
}

/// Widen a tight titlebar container so the zoom button stays inside it.
#[cfg(target_os = "macos")]
fn extend_tight_traffic_containers(start: *mut objc::runtime::Object) {
    if macos_major() >= 11 || start.is_null() {
        return;
    }
    use objc::runtime::Object;
    use objc::{msg_send, sel, sel_impl};
    unsafe {
        let mut view = start;
        for _ in 0..4 {
            if view.is_null() {
                break;
            }
            let parent: *mut Object = msg_send![view, superview];
            if parent.is_null() {
                break;
            }
            let child: NsRect = msg_send![view, frame];
            let mut parent_frame: NsRect = msg_send![parent, frame];
            let need = child.origin.x + child.size.width + 4.0;
            let name = view_class(parent);
            let tight = parent_frame.size.width < 240.0 && parent_frame.size.width + 0.5 < need;
            if tight && !name.contains("Content") && !name.contains("ThemeFrame") {
                parent_frame.size.width = need;
                let _: () = msg_send![parent, setFrame: parent_frame];
            }
            if name.contains("Content") || name.contains("ThemeFrame") {
                break;
            }
            view = parent;
        }
    }
}

#[cfg(target_os = "macos")]
fn align_traffic_lights_ns(ns_window: *mut objc::runtime::Object) {
    use objc::runtime::Object;
    use objc::{msg_send, sel, sel_impl};
    unsafe {
        let close: *mut Object = msg_send![ns_window, standardWindowButton: 0usize];
        if close.is_null() {
            return;
        }
        let bar: *mut Object = msg_send![close, superview];
        if bar.is_null() {
            return;
        }
        let bar_frame: NsRect = msg_send![bar, frame];
        let parent: *mut Object = msg_send![bar, superview];
        if parent.is_null() {
            return;
        }
        let parent_frame: NsRect = msg_send![parent, frame];
        let class_name = view_class(bar);
        let bar_is_content = class_name.contains("Content") || parent_frame.size.height <= bar_frame.size.height + 1.0;
        unclip_traffic_ancestors(bar);
        let mut base = TRAFFIC_BASE.lock().unwrap();
        if bar_is_content {
            let btn_frame: NsRect = msg_send![close, frame];
            let mut y = parent_frame.size.height - (TOOLBAR_BAND + btn_frame.size.height) / 2.0;
            for kind in [0usize, 1usize, 2usize] {
                let btn: *mut Object = msg_send![ns_window, standardWindowButton: kind];
                if btn.is_null() {
                    continue;
                }
                let frame: NsRect = msg_send![btn, frame];
                let mut x = match traffic_target_x(&mut base.btn_x[kind], frame.origin.x) {
                    Some(x) => x,
                    None => frame.origin.x,
                };
                if macos_major() < 11 {
                    let max_x = (bar_frame.size.width - frame.size.width).max(0.0);
                    let max_y = (bar_frame.size.height - frame.size.height).max(0.0);
                    x = x.clamp(0.0, max_x);
                    y = y.clamp(0.0, max_y);
                }
                if (frame.origin.x - x).abs() < 0.5 && (frame.origin.y - y).abs() < 0.5 {
                    continue;
                }
                let _: () = msg_send![btn, setFrameOrigin: NsPoint { x, y }];
            }
            drop(base);
            extend_tight_traffic_containers(close);
            return;
        }
        let mut y = parent_frame.size.height - (TOOLBAR_BAND + bar_frame.size.height) / 2.0;
        let mut x = match traffic_target_x(&mut base.bar_x, bar_frame.origin.x) {
            Some(x) => x,
            None => bar_frame.origin.x,
        };
        if macos_major() < 11 {
            let max_x = (parent_frame.size.width - bar_frame.size.width).max(0.0);
            let max_y = (parent_frame.size.height - bar_frame.size.height).max(0.0);
            x = x.clamp(0.0, max_x);
            y = y.clamp(0.0, max_y);
        }
        drop(base);
        if (bar_frame.origin.x - x).abs() >= 0.5 || (bar_frame.origin.y - y).abs() >= 0.5 {
            let _: () = msg_send![bar, setFrameOrigin: NsPoint { x, y }];
        }
        extend_tight_traffic_containers(bar);
    }
}

#[cfg(target_os = "macos")]
/// Hide the traffic lights while the player drawer is open so its close
/// chevron, which sits in that same corner, can be clicked.
#[cfg(target_os = "macos")]
fn set_traffic_lights_hidden_ns(ns_window: *mut objc::runtime::Object, hidden: bool) {
    use objc::runtime::{Object, BOOL, NO, YES};
    use objc::{msg_send, sel, sel_impl};
    let flag: BOOL = if hidden { YES } else { NO };
    unsafe {
        for kind in [0usize, 1usize, 2usize] {
            let btn: *mut Object = msg_send![ns_window, standardWindowButton: kind];
            if btn.is_null() {
                continue;
            }
            let _: () = msg_send![btn, setHidden: flag];
        }
    }
}

#[tauri::command]
fn set_traffic_lights_hidden(app: tauri::AppHandle, hidden: bool) {
    #[cfg(target_os = "macos")]
    {
        let window_app = app.clone();
        let _ = app.run_on_main_thread(move || {
            let Some(window) = window_app.get_window("main") else {
                return;
            };
            use objc::runtime::Object;
            let ns_window: *mut Object = match window.ns_window() {
                Ok(ptr) => ptr as *mut Object,
                Err(_) => return,
            };
            if ns_window.is_null() {
                return;
            }
            set_traffic_lights_hidden_ns(ns_window, hidden);
        });
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, hidden);
    }
}

fn align_traffic_lights(window: &tauri::Window) {
    #[cfg(target_os = "macos")]
    {
        if window.label() != "main" {
            return;
        }
        use objc::runtime::Object;
        let ns_window: *mut Object = match window.ns_window() {
            Ok(ptr) => ptr as *mut Object,
            Err(_) => return,
        };
        if ns_window.is_null() {
            return;
        }
        align_traffic_lights_ns(ns_window);
        darken_webview(ns_window);
        pin_webview_surface(ns_window, false);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = window;
    }
}

/// Native Quit uses `terminate:` with a nil target. Catalina's WKWebView, as
/// first responder, swallows that click. Cmd+Q still reaches NSApplication.
#[cfg(target_os = "macos")]
fn bind_app_quit_item() {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    unsafe {
        let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        if app.is_null() {
            return;
        }
        let menu: *mut Object = msg_send![app, mainMenu];
        if menu.is_null() {
            return;
        }
        retarget_quit_items(menu, app);
    }
}

#[cfg(target_os = "macos")]
fn retarget_quit_items(menu: *mut objc::runtime::Object, app: *mut objc::runtime::Object) {
    use objc::runtime::{Object, YES};
    use objc::{msg_send, sel, sel_impl};
    unsafe {
        let n: isize = msg_send![menu, numberOfItems];
        let quit_sel = sel!(terminate:);
        for i in 0..n {
            let item: *mut Object = msg_send![menu, itemAtIndex: i];
            if item.is_null() {
                continue;
            }
            let action: objc::runtime::Sel = msg_send![item, action];
            if action == quit_sel {
                let _: () = msg_send![item, setTarget: app];
                let _: () = msg_send![item, setEnabled: YES];
            }
            let sub: *mut Object = msg_send![item, submenu];
            if !sub.is_null() {
                retarget_quit_items(sub, app);
            }
        }
    }
}

#[cfg(all(target_os = "macos", not(debug_assertions)))]
fn shell_page_url() -> String {
    format!("http://localhost:{}/", shell_proxy::SHELL_PORT)
}

#[cfg(all(target_os = "macos", not(debug_assertions)))]
fn ns_utf8(obj: *mut objc::runtime::Object) -> String {
    if obj.is_null() {
        return String::new();
    }
    unsafe {
        use objc::{msg_send, sel, sel_impl};
        let utf8: *const i8 = msg_send![obj, UTF8String];
        if utf8.is_null() {
            return String::new();
        }
        std::ffi::CStr::from_ptr(utf8).to_string_lossy().into_owned()
    }
}

#[cfg(target_os = "macos")]
fn is_webview_class(name: &str) -> bool {
    // wry's view is WryWebView. Its container, WryWebViewParent, also contains
    // "WebView" and does not implement WKWebView methods.
    name == "WryWebView" || name == "WKWebView"
}

#[cfg(target_os = "macos")]
fn find_webview(view: *mut objc::runtime::Object) -> *mut objc::runtime::Object {
    if view.is_null() {
        return std::ptr::null_mut();
    }
    if is_webview_class(&view_class(view)) {
        return view;
    }
    unsafe {
        use objc::runtime::Object;
        use objc::{msg_send, sel, sel_impl};
        let subviews: *mut Object = msg_send![view, subviews];
        if subviews.is_null() {
            return std::ptr::null_mut();
        }
        let n: usize = msg_send![subviews, count];
        for i in 0..n {
            let sub: *mut Object = msg_send![subviews, objectAtIndex: i];
            let found = find_webview(sub);
            if !found.is_null() {
                return found;
            }
        }
    }
    std::ptr::null_mut()
}

/// Catalina does not run the page's scripts on `tauri://`, and `eval` never
/// fires there either. Load the HTTP shell from AppKit once the proxy is up.
/// The players HUD is its own document; sending it to the main shell replaces
/// that panel with the full app.
#[cfg(all(target_os = "macos", not(debug_assertions)))]
fn navigate_window_to_shell(window: &tauri::Window) {
    if window.label() == "players" {
        return;
    }
    if !SHELL_NAV_READY.load(Ordering::SeqCst) {
        return;
    }
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    let ns_window: *mut Object = match window.ns_window() {
        Ok(ptr) => ptr as *mut Object,
        Err(_) => return,
    };
    if ns_window.is_null() {
        return;
    }
    unsafe {
        let content: *mut Object = msg_send![ns_window, contentView];
        let web = find_webview(content);
        if web.is_null() {
            return;
        }
        let url_sel = sel!(URL);
        let can_url: objc::runtime::BOOL = msg_send![web, respondsToSelector: url_sel];
        if can_url != objc::runtime::YES {
            return;
        }
        let current: *mut Object = msg_send![web, URL];
        let current = if current.is_null() {
            String::new()
        } else {
            let abs: *mut Object = msg_send![current, absoluteString];
            ns_utf8(abs)
        };
        let target = shell_page_url();
        if current.starts_with(target.trim_end_matches('/')) {
            pin_webview_surface(ns_window, window.label() == "players");
            return;
        }
        let url: *mut Object = msg_send![class!(NSURL), URLWithString: ns_str(&target)];
        if url.is_null() {
            return;
        }
        let req: *mut Object = msg_send![class!(NSURLRequest), requestWithURL: url];
        if req.is_null() {
            return;
        }
        eprintln!("Lyrion shell: open {target}");
        let _: () = msg_send![web, loadRequest: req];
        pin_webview_surface(ns_window, window.label() == "players");
    }
}

/// A full-size transparent titlebar does not composite WKWebView on Catalina:
/// the interior shows the desktop. A normal titlebar paints. The players
/// panel is borderless and stays frosted.
#[cfg(target_os = "macos")]
fn pin_webview_surface(ns_window: *mut objc::runtime::Object, transparent: bool) {
    if macos_major() >= 11 || ns_window.is_null() {
        return;
    }
    use objc::runtime::{Class, Object, BOOL, NO, YES};
    use objc::{msg_send, sel, sel_impl};
    unsafe {
        if !transparent {
            let _: () = msg_send![ns_window, setTitlebarAppearsTransparent: NO];
            let _: () = msg_send![ns_window, setOpaque: YES];
            // NSWindowStyleMaskFullSizeContentView
            let mask: usize = msg_send![ns_window, styleMask];
            const FULL_SIZE: usize = 1 << 15;
            if mask & FULL_SIZE != 0 {
                let before: *mut Object = msg_send![ns_window, contentView];
                let _: () = msg_send![ns_window, setStyleMask: mask & !FULL_SIZE];
                let after: *mut Object = msg_send![ns_window, contentView];
                if after != before && !before.is_null() {
                    let _: () = msg_send![ns_window, setContentView: before];
                }
            }
            if let Some(ns_color) = Class::get("NSColor") {
                let rgb: *mut Object = msg_send![
                    ns_color,
                    colorWithCalibratedRed: 0.1098_f64
                    green: 0.1098_f64
                    blue: 0.1176_f64
                    alpha: 1.0_f64
                ];
                if !rgb.is_null() {
                    let _: () = msg_send![ns_window, setBackgroundColor: rgb];
                }
            }
        }
        let content: *mut Object = msg_send![ns_window, contentView];
        if content.is_null() {
            return;
        }
        let web = find_webview(content);
        if web.is_null() {
            return;
        }
        let bounds: NsRect = msg_send![content, bounds];
        if !transparent {
            static REPARENTED: AtomicBool = AtomicBool::new(false);
            let superview: *mut Object = msg_send![web, superview];
            if !superview.is_null() && !REPARENTED.swap(true, Ordering::SeqCst) {
                // Re-inserting forces Catalina to composite the view into the
                // now-opaque window. Repeating it drops the in-flight load.
                let _: () = msg_send![web, removeFromSuperview];
                let _: () = msg_send![superview, addSubview: web];
            }
        }
        let _: () = msg_send![web, setFrame: bounds];
        let draw_sel = sel!(_setDrawsTransparentBackground:);
        let can_draw: BOOL = msg_send![web, respondsToSelector: draw_sel];
        if can_draw == YES {
            let flag: BOOL = if transparent { YES } else { NO };
            let _: () = msg_send![web, _setDrawsTransparentBackground: flag];
        }
        let _: () = msg_send![web, setWantsLayer: YES];
        let layer: *mut Object = msg_send![web, layer];
        if layer.is_null() {
            return;
        }
        if let Some(ns_color) = Class::get("NSColor") {
            let color: *mut Object = if transparent {
                msg_send![ns_color, clearColor]
            } else {
                msg_send![
                    ns_color,
                    colorWithCalibratedRed: 0.1098_f64
                    green: 0.1098_f64
                    blue: 0.1176_f64
                    alpha: 1.0_f64
                ]
            };
            if !color.is_null() {
                let cg: *mut Object = msg_send![color, CGColor];
                let _: () = msg_send![layer, setBackgroundColor: cg];
            }
        }
        let _: () = msg_send![layer, setOpaque: if transparent { NO } else { YES }];
    }
}

fn is_local_host(host: &str) -> bool {
    let h = host.trim().to_ascii_lowercase();
    h == "127.0.0.1" || h == "localhost" || h == "::1"
}

fn local_ipv4_addrs() -> &'static Vec<String> {
    static IPS: OnceLock<Vec<String>> = OnceLock::new();
    IPS.get_or_init(|| {
        let mut out = vec![
            "127.0.0.1".to_string(),
            "localhost".to_string(),
            "::1".to_string(),
        ];
        if let Ok(output) = std::process::Command::new("ifconfig").output() {
            let text = String::from_utf8_lossy(&output.stdout);
            for line in text.lines() {
                let t = line.trim();
                if let Some(rest) = t.strip_prefix("inet ") {
                    let ip = rest.split_whitespace().next().unwrap_or("");
                    if !ip.is_empty() && ip != "127.0.0.1" && !out.iter().any(|x| x == ip) {
                        out.push(ip.to_string());
                    }
                }
            }
        }
        out
    })
}

fn is_this_machine(host: &str) -> bool {
    let h = host.trim();
    if h.is_empty() {
        return false;
    }
    if is_local_host(h) {
        return true;
    }
    local_ipv4_addrs()
        .iter()
        .any(|ip| ip.eq_ignore_ascii_case(h))
}

fn pair_matches(pair: &Option<(String, String)>, host: &str, port: &str) -> bool {
    pair.as_ref()
        .map(|(h, p)| h == host && p == port)
        .unwrap_or(false)
}

fn squeezelite_pid_path(app: &tauri::AppHandle) -> Option<PathBuf> {
    app.path_resolver()
        .app_config_dir()
        .map(|d| d.join("squeezelite-main.pid"))
}

fn read_pid_file(path: &PathBuf) -> Option<u32> {
    let text = std::fs::read_to_string(path).ok()?;
    text.trim().parse().ok()
}

fn pid_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

fn squeezelite_bin() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().ok_or_else(|| "no exe dir".to_string())?;
    let names = [
        "squeezelite",
        "squeezelite-aarch64-apple-darwin",
        "squeezelite-universal-apple-darwin",
        "squeezelite-x86_64-apple-darwin",
        "squeezelite-x86_64-pc-windows-msvc.exe",
        "squeezelite-x86_64-unknown-linux-gnu",
        "squeezelite-aarch64-unknown-linux-gnu",
    ];
    for name in names {
        let p = dir.join(name);
        if p.exists() {
            return Ok(p);
        }
        let p2 = dir.join("binaries").join(name);
        if p2.exists() {
            return Ok(p2);
        }
    }
    Err("squeezelite binary not found".into())
}

#[tauri::command]
fn squeezelite_running(app: tauri::AppHandle) -> bool {
    if let Some(path) = squeezelite_pid_path(&app) {
        if let Some(pid) = read_pid_file(&path) {
            return pid_is_alive(pid);
        }
    }
    false
}

#[tauri::command]
fn squeezelite_start(
    app: tauri::AppHandle,
    name: String,
    model: String,
    server: Option<String>,
) -> Result<u32, String> {
    if let Some(path) = squeezelite_pid_path(&app) {
        if let Some(pid) = read_pid_file(&path) {
            if pid_is_alive(pid) {
                return Ok(pid);
            }
        }
    }
    let bin = squeezelite_bin()?;
    let mut args = vec!["-n".to_string(), name, "-M".to_string(), model];
    if let Some(host) = server {
        let h = host.trim().to_string();
        if !h.is_empty() {
            args.push("-s".to_string());
            args.push(h);
        }
    }
    let pid = spawn_detached(&bin, &args)?;
    if let Some(dir) = app.path_resolver().app_config_dir() {
        let _ = std::fs::create_dir_all(&dir);
        if let Some(path) = squeezelite_pid_path(&app) {
            let _ = std::fs::write(path, pid.to_string());
        }
    }
    Ok(pid)
}

#[tauri::command]
fn squeezelite_reap_previews() {
    let _ = std::process::Command::new("pkill")
        .args(["-f", "squeezelite.*Preview"])
        .status();
}

#[tauri::command]
fn squeezelite_stop(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(path) = squeezelite_pid_path(&app) {
        if let Some(pid) = read_pid_file(&path) {
            if pid_is_alive(pid) {
                #[cfg(unix)]
                unsafe {
                    libc::kill(pid as i32, libc::SIGTERM);
                }
                #[cfg(not(unix))]
                {
                    let _ = pid;
                }
            }
        }
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

fn spawn_detached(bin: &PathBuf, args: &[String]) -> Result<u32, String> {
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    let child = cmd.spawn().map_err(|e| format!("squeezelite: {}", e))?;
    Ok(child.id())
}

async fn fetch_lms_players(host: &str, port: &str) -> Vec<MenuPlayer> {
    let host = host.trim();
    let port = if port.trim().is_empty() {
        "9000"
    } else {
        port.trim()
    };
    if host.is_empty() {
        return Vec::new();
    }
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .connect_timeout(Duration::from_secs(2))
        .pool_max_idle_per_host(0)
        .build()
    {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let body = serde_json::json!({
        "id": 1,
        "method": "slim.request",
        "params": ["", ["players", "0", "99"]],
    });
    let url = format!("http://{}:{}/jsonrpc.js", host, port);
    let resp = match client
        .post(&url)
        .header("Connection", "close")
        .json(&body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let v: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let loop_arr = v
        .pointer("/result/players_loop")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::new();
    for p in loop_arr {
        let id = p
            .get("playerid")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let name = p
            .get("name")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        if id.is_empty() {
            continue;
        }
        let low = name.to_ascii_lowercase();
        if low.contains("preview") {
            continue;
        }
        out.push(MenuPlayer { id, name });
    }
    out
}

async fn lms_jsonrpc(host: &str, port: &str, cmd: &[&str]) -> Result<(), String> {
    lms_player_request(host, port, "", cmd).await
}

async fn lms_player_request(host: &str, port: &str, player: &str, cmd: &[&str]) -> Result<(), String> {
    let host = host.trim();
    let port = if port.trim().is_empty() { "9000" } else { port.trim() };
    if host.is_empty() {
        return Err("no host".into());
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .connect_timeout(Duration::from_secs(3))
        .pool_max_idle_per_host(0)
        .build()
        .map_err(|e| e.to_string())?;
    let params: Vec<serde_json::Value> = cmd.iter().map(|s| serde_json::Value::from(*s)).collect();
    let body = serde_json::json!({
        "id": 1,
        "method": "slim.request",
        "params": [player, params],
    });
    let url = format!("http://{}:{}/jsonrpc.js", host, port);
    let resp = client
        .post(&url)
        .header("Connection", "close")
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    Ok(())
}

/// Volume for one player. Args stay strings; the connection is not pooled.
/// The HUD's webview client was leaving `mixer volume` unchanged.
#[tauri::command]
async fn lms_mixer_volume(host: String, port: String, player: String, volume: String) -> Result<(), String> {
    let cmd = ["mixer", "volume", volume.as_str()];
    lms_player_request(&host, &port, &player, &cmd).await
}

/// Playback for the player Material last reported (MAC), not the display name.
#[tauri::command]
async fn lms_player_command(cmd: Vec<String>) -> Result<(), String> {
    let (host, port) = CURRENT_LMS
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| "no server".to_string())?;
    let player = CURRENT_PLAYER
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_default();
    if player.is_empty() {
        return Err("no player".into());
    }
    let refs: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
    lms_player_request(&host, &port, &player, &refs).await
}

/// "none" | "name" (artists: name / default / reverse) | "all"
static SORT_MODE: Mutex<String> = Mutex::new(String::new());

fn sort_mode() -> String {
    let mode = SORT_MODE.lock().unwrap().clone();
    match mode.as_str() {
        "all" | "name" => mode,
        _ => "none".to_string(),
    }
}

fn sort_item_on(id: &str, server_on: bool) -> bool {
    if !server_on {
        return false;
    }
    match sort_mode().as_str() {
        "all" => true,
        "name" => matches!(id, "lib_sort_name" | "lib_sort_default" | "lib_sort_reverse"),
        _ => false,
    }
}

#[tauri::command]
fn set_sort_menu(app: tauri::AppHandle, mode: String) {
    let mode = match mode.as_str() {
        "all" | "name" => mode,
        _ => "none".to_string(),
    };
    {
        let mut cur = SORT_MODE.lock().unwrap();
        if *cur == mode {
            return;
        }
        *cur = mode;
    }
    update_app_menu_enabled(&app);
    let _ = app.tray_handle().set_menu(build_tray_menu());
}

/// "" until the browse view reports. Then "grid", "list", or "none" (fusion: neither).
static VIEW_LAYOUT: Mutex<String> = Mutex::new(String::new());

fn view_layout() -> String {
    VIEW_LAYOUT.lock().unwrap().clone()
}

fn layout_item_selected(id: &str) -> bool {
    match view_layout().as_str() {
        "grid" => id == "view_grid",
        "list" => id == "view_list",
        _ => false,
    }
}

fn with_layout_check(mut item: CustomMenuItem, id: &str) -> CustomMenuItem {
    if layout_item_selected(id) {
        item = item.selected();
    }
    item
}

fn apply_view_layout(app: &tauri::AppHandle, mode: &str) {
    let mode = match mode {
        "grid" | "list" | "none" => mode.to_string(),
        _ => return,
    };
    {
        let mut cur = VIEW_LAYOUT.lock().unwrap();
        if *cur == mode {
            return;
        }
        *cur = mode.clone();
    }
    if let Some(window) = app.get_window("main") {
        let mh = window.menu_handle();
        if let Some(item) = mh.try_get_item("view_grid") {
            let _ = item.set_selected(mode == "grid");
        }
        if let Some(item) = mh.try_get_item("view_list") {
            let _ = item.set_selected(mode == "list");
        }
    }
    let tray = app.tray_handle();
    let _ = tray.get_item("view_grid").set_selected(mode == "grid");
    let _ = tray.get_item("view_list").set_selected(mode == "list");
}

#[tauri::command]
fn set_view_layout(app: tauri::AppHandle, mode: String) {
    apply_view_layout(&app, &mode);
}

fn server_label(s: &MenuServer, selected: bool) -> String {
    let mut label = if !s.name.is_empty() && s.name != s.host {
        format!("{}  ({})", s.name, s.host)
    } else {
        format!("{}:{}", s.host, s.port)
    };
    if s.is_local {
        label.push_str("  — this computer");
    }
    if selected {
        label = format!("✓  {}", label);
    }
    label
}

fn local_status_label(running: bool, installed: bool) -> &'static str {
    if running {
        "Running locally"
    } else if installed {
        "Installed, not running"
    } else {
        "Not installed"
    }
}

fn add_local_lms_tray_items(
    mut menu: SystemTrayMenu,
    running: bool,
    installed: bool,
) -> SystemTrayMenu {
    let mut start = CustomMenuItem::new("srv_start", "Start");
    if !installed || running {
        start = start.disabled();
    }
    let mut stop = CustomMenuItem::new("srv_stop", "Stop");
    if !running {
        stop = stop.disabled();
    }
    let mut restart = CustomMenuItem::new("srv_restart", "Restart");
    if !running {
        restart = restart.disabled();
    }
    menu = menu.add_item(start).add_item(stop).add_item(restart);
    menu
}

fn add_remote_lms_tray_items(mut menu: SystemTrayMenu, idx: usize) -> SystemTrayMenu {
    menu = menu
        .add_item(CustomMenuItem::new(
            format!("srv_rpc_restart_{idx}"),
            "Restart",
        ))
        .add_item(CustomMenuItem::new(
            format!("srv_rpc_shutdown_{idx}"),
            "Shutdown",
        ));
    menu
}

fn server_controls_tray(
    idx: usize,
    s: &MenuServer,
    include_default: bool,
    is_active: bool,
    is_default: bool,
    running: bool,
    installed: bool,
) -> SystemTrayMenu {
    let mut menu = SystemTrayMenu::new();
    let mut use_it = CustomMenuItem::new(format!("srv_use_{idx}"), "Use this server");
    if is_active {
        use_it = use_it.selected();
    }
    menu = menu.add_item(use_it);
    if include_default {
        let mut def = CustomMenuItem::new(format!("srv_def_{idx}"), "Set as default");
        if is_default {
            def = def.selected();
        }
        menu = menu.add_item(def);
    }
    if s.is_local && installed {
        menu = menu.add_native_item(SystemTrayMenuItem::Separator);
        menu = add_local_lms_tray_items(menu, running, installed);
    } else if !s.is_local {
        menu = menu.add_native_item(SystemTrayMenuItem::Separator);
        menu = add_remote_lms_tray_items(menu, idx);
    }
    menu
}

fn server_tray_submenu() -> SystemTraySubmenu {
    let servers = MENU_SERVERS.lock().unwrap().clone();
    let current = CURRENT_LMS.lock().unwrap().clone();
    let default = DEFAULT_LMS.lock().unwrap().clone();
    let (running, installed) = *MENU_LMS_FLAGS.lock().unwrap();
    let mut menu = SystemTrayMenu::new();

    if servers.len() <= 1 {
        if let Some(s) = servers.first() {
            if s.is_local || installed {
                menu = menu.add_item(
                    CustomMenuItem::new("srv_status", local_status_label(running, installed))
                        .disabled(),
                );
            }
            if (s.is_local || is_this_machine(&s.host)) && installed {
                menu = add_local_lms_tray_items(menu, running, installed);
                menu = menu.add_native_item(SystemTrayMenuItem::Separator);
            } else if !s.is_local {
                menu = add_remote_lms_tray_items(menu, 0);
                menu = menu.add_native_item(SystemTrayMenuItem::Separator);
            }
        } else if installed {
            menu = menu.add_item(
                CustomMenuItem::new("srv_status", local_status_label(running, installed)).disabled(),
            );
            menu = add_local_lms_tray_items(menu, running, installed);
            menu = menu.add_native_item(SystemTrayMenuItem::Separator);
        } else {
            menu = menu.add_item(CustomMenuItem::new("srv_pick_none", "No servers found").disabled());
        }
    } else {
        for (i, s) in servers.iter().enumerate() {
            let active = pair_matches(&current, &s.host, &s.port);
            let is_def = pair_matches(&default, &s.host, &s.port);
            menu = menu.add_submenu(SystemTraySubmenu::new(
                server_label(s, active),
                server_controls_tray(i, s, true, active, is_def, running, installed),
            ));
        }
        menu = menu.add_native_item(SystemTrayMenuItem::Separator);
        if installed && !servers.iter().any(|s| s.is_local) {
            menu = add_local_lms_tray_items(menu, running, installed);
            menu = menu.add_native_item(SystemTrayMenuItem::Separator);
        }
    }

    menu = menu.add_item(tray_item(
        "srv_web",
        "Server settings…",
        current_lms_active(),
    ));
    menu = menu.add_item(CustomMenuItem::new(
        "srv_install",
        if installed {
            "Install or update…"
        } else {
            "Install Lyrion Music Server…"
        },
    ));
    if !installed {
        menu = menu.add_item(CustomMenuItem::new("srv_docs", "Getting started…"));
    }
    SystemTraySubmenu::new("Server", menu)
}

fn tray_item(id: &str, title: &str, enabled: bool) -> CustomMenuItem {
    let mut item = CustomMenuItem::new(id, title);
    if !enabled {
        item = item.disabled();
    }
    item
}

fn view_tray_submenu() -> SystemTraySubmenu {
    let on = current_lms_active();
    SystemTraySubmenu::new(
        "View",
        SystemTrayMenu::new()
            .add_item(tray_item("view_home", "Home", on))
            .add_item(tray_item("view_search", "Search", on).accelerator("CmdOrControl+F"))
            .add_item(tray_item("view_mymusic", "My Music", on))
            .add_item(tray_item("view_radio", "Radio", on))
            .add_item(tray_item("view_favorites", "Favorites", on))
            .add_item(tray_item("view_apps", "Apps", on))
            .add_item(tray_item("view_extras", "Extras", on))
            .add_item(tray_item("view_plugins", "Plugins", on))
            .add_native_item(SystemTrayMenuItem::Separator)
            .add_item(tray_item("view_refresh", "Refresh", on).accelerator("CmdOrControl+R"))
            .add_native_item(SystemTrayMenuItem::Separator)
            .add_item(with_layout_check(tray_item("view_grid", "as Grid", on), "view_grid"))
            .add_item(with_layout_check(tray_item("view_list", "as List", on), "view_list"))
            .add_native_item(SystemTrayMenuItem::Separator)
            .add_item(tray_item("lib_filter", "Filter…", on))
            .add_submenu(SystemTraySubmenu::new(
                "Sort",
                SystemTrayMenu::new()
                    .add_item(tray_item("lib_sort_name", "By name", sort_item_on("lib_sort_name", on)))
                    .add_item(tray_item("lib_sort_date", "By date", sort_item_on("lib_sort_date", on)))
                    .add_item(tray_item("lib_sort_default", "Default", sort_item_on("lib_sort_default", on)))
                    .add_item(tray_item("lib_sort_added", "Date added", sort_item_on("lib_sort_added", on)))
                    .add_native_item(SystemTrayMenuItem::Separator)
                    .add_item(tray_item("lib_sort_reverse", "Reverse", sort_item_on("lib_sort_reverse", on))),
            )),
    )
}

fn settings_tray_submenu() -> SystemTraySubmenu {
    let on = current_lms_active();
    SystemTraySubmenu::new(
        "Settings",
        SystemTrayMenu::new()
            .add_item(CustomMenuItem::new("settings_app", "Application settings…").accelerator("CmdOrControl+,"))
            .add_item(tray_item("settings_ui", "Interface settings…", on).accelerator("CmdOrControl+Alt+,"))
            .add_item(tray_item("settings_player", "Player preferences…", on).accelerator("CmdOrControl+Alt+P"))
            .add_item(tray_item("settings_server", "Server settings…", on).accelerator("CmdOrControl+Alt+S")),
    )
}

fn build_tray_menu() -> SystemTrayMenu {
    SystemTrayMenu::new()
        .add_item(CustomMenuItem::new(
            "toggle",
            format!("Show / Hide ({})", DEFAULT_TOGGLE_HOTKEY),
        ))
        .add_item(CustomMenuItem::new("dock", dock_menu_label()))
        .add_native_item(SystemTrayMenuItem::Separator)
        .add_submenu(view_tray_submenu())
        .add_submenu(server_tray_submenu())
        .add_submenu(settings_tray_submenu())
        .add_native_item(SystemTrayMenuItem::Separator)
        .add_item(CustomMenuItem::new("quit", "Quit").accelerator("CmdOrControl+Q"))
}

fn dock_menu_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "Show Dock Icon"
    } else {
        // Linux has no Dock; this is the same flag (do not hide on focus loss).
        "Keep window open"
    }
}

fn server_app_submenu() -> tauri::Submenu {
    use tauri::{Menu, Submenu};
    // Placeholder — macOS rebuilds this submenu to match the menuling.
    Submenu::new(
        "Server",
        Menu::new().add_item(CustomMenuItem::new("srv_status", "Checking…").disabled()),
    )
}

fn view_app_menu() -> tauri::Menu {
    use tauri::{Menu, MenuItem, Submenu};
    Menu::new()
        .add_item(CustomMenuItem::new("view_home", "Home"))
        .add_item(CustomMenuItem::new("view_search", "Search").accelerator("CmdOrControl+F"))
        .add_item(CustomMenuItem::new("view_mymusic", "My Music"))
        .add_item(CustomMenuItem::new("view_radio", "Radio"))
        .add_item(CustomMenuItem::new("view_favorites", "Favorites"))
        .add_item(CustomMenuItem::new("view_apps", "Apps"))
        .add_item(CustomMenuItem::new("view_extras", "Extras"))
        .add_item(CustomMenuItem::new("view_plugins", "Plugins"))
        .add_native_item(MenuItem::Separator)
        .add_item(CustomMenuItem::new("view_refresh", "Refresh").accelerator("CmdOrControl+R"))
        .add_native_item(MenuItem::Separator)
        .add_item(CustomMenuItem::new("view_grid", "as Grid"))
        .add_item(CustomMenuItem::new("view_list", "as List"))
        .add_native_item(MenuItem::Separator)
        .add_item(CustomMenuItem::new("lib_filter", "Filter…"))
        .add_submenu(Submenu::new(
            "Sort",
            Menu::new()
                .add_item(CustomMenuItem::new("lib_sort_name", "By name"))
                .add_item(CustomMenuItem::new("lib_sort_date", "By date"))
                .add_item(CustomMenuItem::new("lib_sort_default", "Default"))
                .add_item(CustomMenuItem::new("lib_sort_added", "Date added"))
                .add_native_item(MenuItem::Separator)
                .add_item(CustomMenuItem::new("lib_sort_reverse", "Reverse")),
        ))
}

fn settings_app_submenu() -> tauri::Submenu {
    use tauri::{Menu, MenuItem, Submenu};
    Submenu::new(
        "Settings",
        Menu::new()
            .add_item(CustomMenuItem::new("settings_app", "Application settings…").accelerator("CmdOrControl+,"))
            .add_item(CustomMenuItem::new("settings_ui", "Interface settings…").accelerator("CmdOrControl+Alt+,"))
            .add_item(CustomMenuItem::new("settings_player", "Player preferences…").accelerator("CmdOrControl+Alt+P"))
            .add_item(CustomMenuItem::new("settings_server", "Server settings…").accelerator("CmdOrControl+Alt+S"))
            .add_native_item(MenuItem::Separator)
            .add_item(CustomMenuItem::new(
                "settings_wizard",
                "Assistant de configuration",
            )),
    )
}

fn build_app_menu() -> tauri::Menu {
    use tauri::{Menu, MenuItem, Submenu};
    let server = server_app_submenu();
    let settings = settings_app_submenu();
    #[cfg(target_os = "macos")]
    {
        let app_menu = Menu::new()
            .add_native_item(MenuItem::About("Lyrion".into(), Default::default()))
            .add_native_item(MenuItem::Separator)
            .add_submenu(settings)
            .add_native_item(MenuItem::Separator)
            .add_native_item(MenuItem::Services)
            .add_native_item(MenuItem::Separator)
            .add_native_item(MenuItem::Hide)
            .add_native_item(MenuItem::HideOthers)
            .add_native_item(MenuItem::ShowAll)
            .add_native_item(MenuItem::Separator)
            .add_native_item(MenuItem::Quit);
        let file_menu = Menu::new().add_native_item(MenuItem::CloseWindow);
        let mut edit_menu = Menu::new();
        edit_menu = edit_menu
            .add_native_item(MenuItem::Undo)
            .add_native_item(MenuItem::Redo)
            .add_native_item(MenuItem::Separator)
            .add_native_item(MenuItem::Cut)
            .add_native_item(MenuItem::Copy)
            .add_native_item(MenuItem::Paste)
            .add_native_item(MenuItem::SelectAll);
        let view_menu = view_app_menu()
            .add_native_item(MenuItem::Separator)
            .add_native_item(MenuItem::EnterFullScreen);
        let window_menu = Menu::new()
            .add_native_item(MenuItem::Minimize)
            .add_native_item(MenuItem::Zoom)
            .add_native_item(MenuItem::Separator)
            .add_native_item(MenuItem::CloseWindow);
        let platine = Submenu::new(
            "Platine",
            Menu::new().add_item(CustomMenuItem::new("platine_alarm", "Alarme").disabled()),
        );
        return Menu::new()
            .add_submenu(Submenu::new("Lyrion", app_menu))
            .add_submenu(Submenu::new("File", file_menu))
            .add_submenu(Submenu::new("Edit", edit_menu))
            .add_submenu(Submenu::new("View", view_menu))
            .add_submenu(platine)
            .add_submenu(server)
            .add_submenu(Submenu::new("Window", window_menu));
    }
    #[cfg(not(target_os = "macos"))]
    {
        Menu::new()
            .add_submenu(Submenu::new(
                "Lyrion",
                Menu::new()
                    .add_item(CustomMenuItem::new(
                        "toggle",
                        format!("Show / Hide ({})", DEFAULT_TOGGLE_HOTKEY),
                    ))
                    .add_item(CustomMenuItem::new("dock", dock_menu_label()))
                    .add_submenu(settings)
                    .add_native_item(MenuItem::Separator)
                    .add_item(CustomMenuItem::new("quit", "Quit").accelerator("CmdOrControl+Q")),
            ))
            .add_submenu(Submenu::new("View", view_app_menu()))
            .add_submenu(Submenu::new(
                "Platine",
                Menu::new()
                    .add_item(CustomMenuItem::new("platine_alarm", "Alarme"))
                    .add_item(CustomMenuItem::new("platine_transfer", "Transfer"))
                    .add_item(CustomMenuItem::new("platine_preset_edit", "Modifier les préréglages…")),
            ))
            .add_submenu(server)
    }
}

fn merge_menu_servers(discovered: Vec<HashMap<String, String>>) -> Vec<MenuServer> {
    let mut out: Vec<MenuServer> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for s in discovered {
        let host = s.get("host").cloned().unwrap_or_default().trim().to_string();
        let port_raw = s.get("port").cloned().unwrap_or_else(|| "9000".into());
        let port = {
            let t = port_raw.trim();
            if t.is_empty() { "9000".to_string() } else { t.to_string() }
        };
        if host.is_empty() {
            continue;
        }
        let key = format!("{}:{}", host, port);
        if !seen.insert(key) {
            continue;
        }
        out.push(MenuServer {
            name: s.get("name").cloned().unwrap_or_default(),
            is_local: is_this_machine(&host),
            host,
            port,
        });
    }
    if let Some((h, p)) = CURRENT_LMS.lock().unwrap().clone() {
        let h = h.trim().to_string();
        let p = {
            let t = p.trim();
            if t.is_empty() { "9000".to_string() } else { t.to_string() }
        };
        if !h.is_empty() && !out.iter().any(|s| s.host == h && s.port == p) {
            out.insert(
                0,
                MenuServer {
                    name: String::new(),
                    is_local: is_this_machine(&h),
                    host: h,
                    port: p,
                },
            );
        }
    }
    out.sort_by(|a, b| {
        match (a.is_local, b.is_local) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.host.cmp(&b.host).then(a.port.cmp(&b.port)),
        }
    });
    out
}

fn attach_local_server(mut list: Vec<MenuServer>, installed: bool, running: bool) -> Vec<MenuServer> {
    if !(installed || running) {
        return list;
    }
    for s in list.iter_mut() {
        if is_this_machine(&s.host) {
            s.is_local = true;
            if s.name.is_empty() {
                s.name = "This computer".into();
            }
        }
    }
    if !list.iter().any(|s| s.is_local) {
        list.insert(
            0,
            MenuServer {
                host: "127.0.0.1".into(),
                port: "9000".into(),
                name: "This computer".into(),
                is_local: true,
            },
        );
    } else {
        let current = CURRENT_LMS.lock().unwrap().clone();
        let current_is_loopback = current
            .as_ref()
            .map(|(h, _)| is_local_host(h))
            .unwrap_or(false);
        let has_lan_local = list.iter().any(|s| s.is_local && !is_local_host(&s.host));
        if has_lan_local && !current_is_loopback {
            list.retain(|s| !(s.is_local && is_local_host(&s.host)));
        }
    }
    list
}

fn rebuild_menus_if_changed(app: &tauri::AppHandle) {
    let fp = menu_fingerprint();
    {
        let mut prev = MENU_FINGERPRINT.lock().unwrap();
        if *prev == fp {
            return;
        }
        *prev = fp;
    }
    let _ = app.tray_handle().set_menu(build_tray_menu());
    rebuild_app_server_menu(app);
    update_app_menu_enabled(app);
}

fn update_app_menu_enabled(app: &tauri::AppHandle) {
    let Some(window) = app.get_window("main") else {
        return;
    };
    let active = current_lms_active();
    let mh = window.menu_handle();
    for id in [
        "view_home",
        "view_search",
        "view_mymusic",
        "view_radio",
        "view_favorites",
        "view_apps",
        "view_extras",
        "view_plugins",
        "view_refresh",
        "view_grid",
        "view_list",
        "lib_filter",
        "settings_ui",
        "settings_player",
        "settings_server",
        "srv_web",
    ] {
        if let Some(item) = mh.try_get_item(id) {
            let _ = item.set_enabled(active);
        }
    }
    for id in [
        "lib_sort_name",
        "lib_sort_date",
        "lib_sort_default",
        "lib_sort_added",
        "lib_sort_reverse",
    ] {
        if let Some(item) = mh.try_get_item(id) {
            let _ = item.set_enabled(sort_item_on(id, active));
        }
    }
    #[cfg(target_os = "macos")]
    {
        let sort_on = active && sort_mode() != "none";
        let _ = app.run_on_main_thread(move || {
            set_named_submenu_enabled("View", "Sort", sort_on);
            sync_wizard_menu_item();
        });
    }
}

#[cfg(target_os = "macos")]
fn set_named_submenu_enabled(parent_title: &str, child_title: &str, enabled: bool) {
    use objc::runtime::{Object, NO, YES};
    use objc::{class, msg_send, sel, sel_impl};
    use std::ffi::CStr;
    unsafe fn title_eq(item: *mut Object, want: &str) -> bool {
        if item.is_null() {
            return false;
        }
        let title: *mut Object = msg_send![item, title];
        if title.is_null() {
            return false;
        }
        let utf8: *const i8 = msg_send![title, UTF8String];
        if utf8.is_null() {
            return false;
        }
        CStr::from_ptr(utf8).to_string_lossy() == want
    }
    unsafe {
        let ns_app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        if ns_app.is_null() {
            return;
        }
        let main: *mut Object = msg_send![ns_app, mainMenu];
        if main.is_null() {
            return;
        }
        let n: isize = msg_send![main, numberOfItems];
        for i in 0..n {
            let item: *mut Object = msg_send![main, itemAtIndex: i];
            if !title_eq(item, parent_title) {
                continue;
            }
            let sub: *mut Object = msg_send![item, submenu];
            if sub.is_null() {
                return;
            }
            let m: isize = msg_send![sub, numberOfItems];
            for j in 0..m {
                let child: *mut Object = msg_send![sub, itemAtIndex: j];
                if title_eq(child, child_title) {
                    let _: () = msg_send![child, setEnabled: if enabled { YES } else { NO }];
                    return;
                }
            }
            return;
        }
    }
}

/// "Assistant de configuration" stays out of the Lyrion menu until Option is held.
#[cfg(target_os = "macos")]
fn ns_item_title(item: *mut objc::runtime::Object) -> String {
    use objc::runtime::Object;
    use objc::{msg_send, sel, sel_impl};
    use std::ffi::CStr;
    if item.is_null() {
        return String::new();
    }
    unsafe {
        let title: *mut Object = msg_send![item, title];
        if title.is_null() {
            return String::new();
        }
        let utf8: *const i8 = msg_send![title, UTF8String];
        if utf8.is_null() {
            return String::new();
        }
        CStr::from_ptr(utf8).to_string_lossy().into_owned()
    }
}

#[cfg(target_os = "macos")]
fn find_submenu(menu: *mut objc::runtime::Object, title: &str) -> *mut objc::runtime::Object {
    use objc::runtime::Object;
    use objc::{msg_send, sel, sel_impl};
    if menu.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        let n: isize = msg_send![menu, numberOfItems];
        for i in 0..n {
            let item: *mut Object = msg_send![menu, itemAtIndex: i];
            if ns_item_title(item) != title {
                continue;
            }
            return msg_send![item, submenu];
        }
    }
    std::ptr::null_mut()
}

#[cfg(target_os = "macos")]
fn option_key_down() -> bool {
    use objc::{class, msg_send, sel, sel_impl};
    unsafe {
        let flags: usize = msg_send![class!(NSEvent), modifierFlags];
        // NSEventModifierFlagOption
        (flags & (1 << 19)) != 0
    }
}

#[cfg(target_os = "macos")]
fn settings_submenu() -> *mut objc::runtime::Object {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    unsafe {
        let ns_app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        if ns_app.is_null() {
            return std::ptr::null_mut();
        }
        let main: *mut Object = msg_send![ns_app, mainMenu];
        let lyrion = find_submenu(main, "Lyrion");
        find_submenu(lyrion, "Settings")
    }
}

#[cfg(target_os = "macos")]
fn sync_wizard_menu_item() {
    use objc::runtime::{Object, BOOL, NO, YES};
    use objc::{msg_send, sel, sel_impl};
    let menu = settings_submenu();
    if menu.is_null() {
        return;
    }
    let show = option_key_down();
    unsafe {
        let delegate = wizard_menu_delegate();
        let current: *mut Object = msg_send![menu, delegate];
        if current != delegate {
            let _: () = msg_send![menu, setDelegate: delegate];
        }
        let n: isize = msg_send![menu, numberOfItems];
        for i in 0..n {
            let item: *mut Object = msg_send![menu, itemAtIndex: i];
            if ns_item_title(item) != "Assistant de configuration" {
                continue;
            }
            let _: () = msg_send![item, setHidden: if show { NO } else { YES }];
            if i > 0 {
                let prev: *mut Object = msg_send![menu, itemAtIndex: i - 1];
                let sep: BOOL = msg_send![prev, isSeparatorItem];
                if sep == YES {
                    let _: () = msg_send![prev, setHidden: if show { NO } else { YES }];
                }
            }
            return;
        }
    }
}

#[cfg(target_os = "macos")]
extern "C" fn wizard_menu_needs_update(
    _this: &objc::runtime::Object,
    _sel: objc::runtime::Sel,
    _menu: *mut objc::runtime::Object,
) {
    sync_wizard_menu_item();
}

#[cfg(target_os = "macos")]
fn wizard_menu_delegate() -> *mut objc::runtime::Object {
    use objc::declare::ClassDecl;
    use objc::runtime::{Class, Object, Sel};
    use objc::{class, msg_send, sel, sel_impl};
    use std::sync::Once;
    static INIT: Once = Once::new();
    static mut DELEGATE: *mut Object = std::ptr::null_mut();
    INIT.call_once(|| unsafe {
        let super_cls = class!(NSObject);
        let mut decl =
            ClassDecl::new("LyrionWizardMenuDelegate", super_cls).expect("LyrionWizardMenuDelegate");
        decl.add_method(
            sel!(menuNeedsUpdate:),
            wizard_menu_needs_update as extern "C" fn(&Object, Sel, *mut Object),
        );
        let cls: &Class = decl.register();
        DELEGATE = msg_send![cls, new];
    });
    unsafe { DELEGATE }
}

#[cfg(target_os = "macos")]
fn install_wizard_option_reveal() {
    use block::ConcreteBlock;
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let mask: u64 = 1 << 12; // NSEventMaskFlagsChanged
        let block = ConcreteBlock::new(move |event: *mut Object| -> *mut Object {
            sync_wizard_menu_item();
            event
        });
        let block = block.copy();
        unsafe {
            let _: *mut Object = msg_send![
                class!(NSEvent),
                addLocalMonitorForEventsMatchingMask: mask
                handler: &*block
            ];
            std::mem::forget(block);
        }
    });
    sync_wizard_menu_item();
}

fn rebuild_app_server_menu(app: &tauri::AppHandle) {
    #[cfg(target_os = "macos")]
    {
        let handle = app.clone();
        let _ = app.run_on_main_thread(move || {
            rebuild_ns_server_menu(&handle);
        });
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
    }
}

#[cfg(target_os = "macos")]
fn ns_str(s: &str) -> *mut objc::runtime::Object {
    use objc::{class, msg_send, sel, sel_impl};
    use std::ffi::CString;
    let c = CString::new(s).unwrap_or_else(|_| CString::new("").unwrap());
    unsafe { msg_send![class!(NSString), stringWithUTF8String: c.as_ptr()] }
}

#[cfg(target_os = "macos")]
extern "C" fn handle_lyrion_menu(
    _this: &objc::runtime::Object,
    _sel: objc::runtime::Sel,
    sender: *mut objc::runtime::Object,
) {
    use objc::runtime::Object;
    use objc::{msg_send, sel, sel_impl};
    use std::ffi::CStr;
    unsafe {
        let rep: *mut Object = msg_send![sender, representedObject];
        if rep.is_null() {
            return;
        }
        let utf8: *const i8 = msg_send![rep, UTF8String];
        if utf8.is_null() {
            return;
        }
        let id = CStr::from_ptr(utf8).to_string_lossy().into_owned();
        let app = APP_HANDLE.lock().ok().and_then(|g| g.clone());
        if let Some(app) = app {
            handle_menu_id(&app, &id);
        }
    }
}

#[cfg(target_os = "macos")]
fn lyrion_menu_target() -> *mut objc::runtime::Object {
    use objc::declare::ClassDecl;
    use objc::runtime::{Class, Object, Sel};
    use objc::{class, msg_send, sel, sel_impl};
    use std::sync::Once;
    static INIT: Once = Once::new();
    static mut TARGET: *mut Object = std::ptr::null_mut();
    INIT.call_once(|| unsafe {
        let super_cls = class!(NSObject);
        let mut decl =
            ClassDecl::new("LyrionMenuTarget", super_cls).expect("LyrionMenuTarget");
        decl.add_method(
            sel!(handleLyrionMenu:),
            handle_lyrion_menu as extern "C" fn(&Object, Sel, *mut Object),
        );
        let cls: &Class = decl.register();
        TARGET = msg_send![cls, new];
    });
    unsafe { TARGET }
}

#[cfg(target_os = "macos")]
fn ns_new_menu(title: &str) -> *mut objc::runtime::Object {
    use objc::runtime::{Object, NO};
    use objc::{class, msg_send, sel, sel_impl};
    unsafe {
        let menu: *mut Object = msg_send![class!(NSMenu), alloc];
        let menu: *mut Object = msg_send![menu, initWithTitle: ns_str(title)];
        let _: () = msg_send![menu, setAutoenablesItems: NO];
        menu
    }
}

#[cfg(target_os = "macos")]
fn ns_add_item(
    menu: *mut objc::runtime::Object,
    id: &str,
    title: &str,
    enabled: bool,
    selected: bool,
) {
    use objc::runtime::{Object, NO, YES};
    use objc::{class, msg_send, sel, sel_impl};
    unsafe {
        let item: *mut Object = msg_send![class!(NSMenuItem), alloc];
        let item: *mut Object = msg_send![
            item,
            initWithTitle: ns_str(title)
            action: sel!(handleLyrionMenu:)
            keyEquivalent: ns_str("")
        ];
        let _: () = msg_send![item, setTarget: lyrion_menu_target()];
        let _: () = msg_send![item, setRepresentedObject: ns_str(id)];
        let _: () = msg_send![item, setEnabled: if enabled { YES } else { NO }];
        if selected {
            let _: () = msg_send![item, setState: 1_isize];
        }
        let _: () = msg_send![menu, addItem: item];
        let _: () = msg_send![item, release];
    }
}

#[cfg(target_os = "macos")]
fn ns_add_disabled(menu: *mut objc::runtime::Object, title: &str) {
    use objc::runtime::{Object, NO};
    use objc::{class, msg_send, sel, sel_impl};
    unsafe {
        let item: *mut Object = msg_send![class!(NSMenuItem), alloc];
        let item: *mut Object = msg_send![
            item,
            initWithTitle: ns_str(title)
            action: sel!(description)
            keyEquivalent: ns_str("")
        ];
        let _: () = msg_send![item, setEnabled: NO];
        let _: () = msg_send![menu, addItem: item];
        let _: () = msg_send![item, release];
    }
}

#[cfg(target_os = "macos")]
fn ns_add_sep(menu: *mut objc::runtime::Object) {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    unsafe {
        let sep: *mut Object = msg_send![class!(NSMenuItem), separatorItem];
        let _: () = msg_send![menu, addItem: sep];
    }
}

#[cfg(target_os = "macos")]
fn ns_add_submenu(
    menu: *mut objc::runtime::Object,
    title: &str,
    child: *mut objc::runtime::Object,
    enabled: bool,
) {
    use objc::runtime::{Object, NO, YES};
    use objc::{class, msg_send, sel, sel_impl};
    unsafe {
        let item: *mut Object = msg_send![class!(NSMenuItem), alloc];
        let item: *mut Object = msg_send![
            item,
            initWithTitle: ns_str(title)
            action: sel!(description)
            keyEquivalent: ns_str("")
        ];
        let _: () = msg_send![item, setSubmenu: child];
        let _: () = msg_send![item, setEnabled: if enabled { YES } else { NO }];
        let _: () = msg_send![menu, addItem: item];
        let _: () = msg_send![item, release];
        let _: () = msg_send![child, release];
    }
}

#[cfg(target_os = "macos")]
fn ns_add_choice_submenu(
    menu: *mut objc::runtime::Object,
    title: &str,
    items: &[PlatineChoice],
    enabled: bool,
) {
    if !enabled || items.is_empty() {
        ns_add_disabled(menu, title);
        return;
    }
    let child = ns_new_menu(title);
    for c in items {
        let label = if c.label.is_empty() {
            c.key.as_str()
        } else {
            c.label.as_str()
        };
        ns_add_item(child, &c.key, label, true, c.selected);
    }
    ns_add_submenu(menu, title, child, true);
}

#[cfg(target_os = "macos")]
fn platine_sleep_checked(state: &PlatineMenuState, key: &str) -> bool {
    if state.sleep <= 0 {
        return key == "platine_sleep_0";
    }
    state.sleep_key == key && key != "platine_sleep_0"
}

fn rebuild_platine_menu(app: &tauri::AppHandle) {
    #[cfg(target_os = "macos")]
    {
        let _ = app.run_on_main_thread(move || {
            rebuild_ns_platine_menu();
        });
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
    }
}

#[cfg(target_os = "macos")]
fn rebuild_ns_platine_menu() {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    use std::ffi::CStr;

    let state = PLATINE_MENU.lock().unwrap().clone();
    let on = state.enabled;
    let menu = ns_new_menu("Platine");

    ns_add_item(menu, "platine_alarm", "Alarme", on, false);

    if on {
        let sleep = ns_new_menu("Mise en veille");
        let sleep_rows = [
            ("platine_sleep_0", "Ne pas mettre en veille"),
            ("platine_sleep_900", "15 minutes"),
            ("platine_sleep_1800", "30 minutes"),
            ("platine_sleep_2700", "45 minutes"),
            ("platine_sleep_3600", "60 minutes"),
            ("platine_sleep_5400", "90 minutes"),
            ("platine_sleep_end", "Fin de la piste"),
        ];
        for (key, label) in sleep_rows {
            ns_add_item(sleep, key, label, true, platine_sleep_checked(&state, key));
        }
        ns_add_submenu(menu, "Mise en veille", sleep, true);
    } else {
        ns_add_disabled(menu, "Mise en veille");
    }

    if on {
        let sync = ns_new_menu("Synchroniser");
        ns_add_item(sync, "platine_sync_off", "Aucune", true, !state.synced);
        for p in &state.players {
            let label = if p.name.is_empty() {
                p.id.as_str()
            } else {
                p.name.as_str()
            };
            let key = format!("platine_sync:{}", p.id);
            ns_add_item(sync, &key, label, true, state.synced && p.selected);
        }
        ns_add_submenu(menu, "Synchroniser", sync, true);
    } else {
        ns_add_disabled(menu, "Synchroniser");
    }

    ns_add_item(
        menu,
        "platine_transfer",
        "Transfer",
        on && state.can_transfer,
        false,
    );
    ns_add_sep(menu);

    ns_add_choice_submenu(menu, "Input", &state.inputs, on);
    ns_add_choice_submenu(menu, "Output", &state.outputs, on);

    if on && state.dsp_known {
        let dsp = ns_new_menu("Preset DSP");
        for c in &state.dsp {
            let label = if c.label.is_empty() {
                c.key.as_str()
            } else {
                c.label.as_str()
            };
            ns_add_item(dsp, &c.key, label, true, state.dsp_on && c.selected);
        }
        if !state.dsp.is_empty() {
            ns_add_sep(dsp);
        }
        ns_add_item(dsp, "platine_dsp_off", "Désactiver", true, !state.dsp_on);
        ns_add_submenu(menu, "Preset DSP", dsp, true);
    } else {
        ns_add_disabled(menu, "Preset DSP");
    }

    if on {
        let presets = ns_new_menu("Préréglage");
        for c in &state.presets {
            let label = if c.label.is_empty() {
                c.key.as_str()
            } else {
                c.label.as_str()
            };
            ns_add_item(presets, &c.key, label, true, c.selected);
        }
        if !state.presets.is_empty() {
            ns_add_sep(presets);
        }
        ns_add_item(presets, "platine_preset_edit", "Modifier…", true, false);
        ns_add_submenu(menu, "Préréglage", presets, true);
    } else {
        ns_add_disabled(menu, "Préréglage");
    }

    unsafe {
        let ns_app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        if !ns_app.is_null() {
            let main: *mut Object = msg_send![ns_app, mainMenu];
            if !main.is_null() {
                let n: isize = msg_send![main, numberOfItems];
                for i in 0..n {
                    let item: *mut Object = msg_send![main, itemAtIndex: i];
                    let title: *mut Object = msg_send![item, title];
                    if title.is_null() {
                        continue;
                    }
                    let utf8: *const i8 = msg_send![title, UTF8String];
                    if utf8.is_null() {
                        continue;
                    }
                    let name = CStr::from_ptr(utf8).to_string_lossy();
                    if name == "Platine" {
                        let _: () = msg_send![item, setSubmenu: menu];
                        break;
                    }
                }
            }
        }
        let _: () = msg_send![menu, release];
    }
}

#[cfg(target_os = "macos")]
fn ns_add_local_controls(menu: *mut objc::runtime::Object, running: bool, installed: bool) {
    ns_add_item(menu, "srv_start", "Start", installed && !running, false);
    ns_add_item(menu, "srv_stop", "Stop", running, false);
    ns_add_item(menu, "srv_restart", "Restart", running, false);
}

#[cfg(target_os = "macos")]
fn ns_add_remote_controls(menu: *mut objc::runtime::Object, idx: usize) {
    ns_add_item(
        menu,
        &format!("srv_rpc_restart_{idx}"),
        "Restart",
        true,
        false,
    );
    ns_add_item(
        menu,
        &format!("srv_rpc_shutdown_{idx}"),
        "Shutdown",
        true,
        false,
    );
}

#[cfg(target_os = "macos")]
fn ns_server_controls(
    idx: usize,
    s: &MenuServer,
    include_default: bool,
    is_active: bool,
    is_default: bool,
    running: bool,
    installed: bool,
) -> *mut objc::runtime::Object {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    let child = ns_new_menu(&server_label(s, false));
    ns_add_item(
        child,
        &format!("srv_use_{idx}"),
        "Use this server",
        true,
        is_active,
    );
    if include_default {
        ns_add_item(
            child,
            &format!("srv_def_{idx}"),
            "Set as default",
            true,
            is_default,
        );
    }
    if s.is_local && installed {
        ns_add_sep(child);
        ns_add_local_controls(child, running, installed);
    } else if !s.is_local {
        ns_add_sep(child);
        ns_add_remote_controls(child, idx);
    }
    unsafe {
        let wrap: *mut Object = msg_send![class!(NSMenuItem), alloc];
        let wrap: *mut Object = msg_send![
            wrap,
            initWithTitle: ns_str(&server_label(s, false))
            action: sel!(description)
            keyEquivalent: ns_str("")
        ];
        let _: () = msg_send![wrap, setSubmenu: child];
        if is_active {
            let _: () = msg_send![wrap, setState: 1_isize];
        }
        let _: () = msg_send![child, release];
        wrap
    }
}

#[cfg(target_os = "macos")]
fn rebuild_ns_server_menu(_app: &tauri::AppHandle) {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    use std::ffi::CStr;

    let servers = MENU_SERVERS.lock().unwrap().clone();
    let current = CURRENT_LMS.lock().unwrap().clone();
    let default = DEFAULT_LMS.lock().unwrap().clone();
    let (running, installed) = *MENU_LMS_FLAGS.lock().unwrap();

    let menu = ns_new_menu("Server");
    if servers.len() <= 1 {
        if let Some(s) = servers.first() {
            if s.is_local || installed {
                ns_add_disabled(menu, local_status_label(running, installed));
            }
            if (s.is_local || is_this_machine(&s.host)) && installed {
                ns_add_local_controls(menu, running, installed);
                ns_add_sep(menu);
            } else if !s.is_local {
                ns_add_remote_controls(menu, 0);
                ns_add_sep(menu);
            }
        } else if installed {
            ns_add_disabled(menu, local_status_label(running, installed));
            ns_add_local_controls(menu, running, installed);
            ns_add_sep(menu);
        } else {
            ns_add_disabled(menu, "No servers found");
        }
    } else {
        unsafe {
            for (i, s) in servers.iter().enumerate() {
                let active = pair_matches(&current, &s.host, &s.port);
                let is_def = pair_matches(&default, &s.host, &s.port);
                let wrap = ns_server_controls(i, s, true, active, is_def, running, installed);
                let _: () = msg_send![menu, addItem: wrap];
                let _: () = msg_send![wrap, release];
            }
        }
        ns_add_sep(menu);
        if installed && !servers.iter().any(|s| s.is_local) {
            ns_add_local_controls(menu, running, installed);
            ns_add_sep(menu);
        }
    }
    ns_add_item(
        menu,
        "srv_web",
        "Server settings…",
        current_lms_active(),
        false,
    );
    ns_add_item(
        menu,
        "srv_install",
        if installed {
            "Install or update…"
        } else {
            "Install Lyrion Music Server…"
        },
        true,
        false,
    );
    if !installed {
        ns_add_item(menu, "srv_docs", "Getting started…", true, false);
    }

    unsafe {
        let ns_app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        if ns_app.is_null() {
            return;
        }
        let main: *mut Object = msg_send![ns_app, mainMenu];
        if main.is_null() {
            return;
        }
        let n: isize = msg_send![main, numberOfItems];
        for i in 0..n {
            let item: *mut Object = msg_send![main, itemAtIndex: i];
            let title: *mut Object = msg_send![item, title];
            if title.is_null() {
                continue;
            }
            let utf8: *const i8 = msg_send![title, UTF8String];
            if utf8.is_null() {
                continue;
            }
            let name = CStr::from_ptr(utf8).to_string_lossy();
            if name == "Server" {
                let _: () = msg_send![item, setSubmenu: menu];
                break;
            }
        }
    }
}

fn menu_fingerprint() -> String {
    let servers = MENU_SERVERS.lock().unwrap().clone();
    let current = CURRENT_LMS.lock().unwrap().clone();
    let default = DEFAULT_LMS.lock().unwrap().clone();
    let flags = *MENU_LMS_FLAGS.lock().unwrap();
    let players = MENU_PLAYERS.lock().unwrap().clone();
    let player = CURRENT_PLAYER.lock().unwrap().clone();
    format!(
        "{:?}|{:?}|{:?}|{:?}|{:?}|{:?}",
        servers
            .iter()
            .map(|s| format!("{}:{}:{}:{}", s.host, s.port, s.name, s.is_local))
            .collect::<Vec<_>>(),
        current,
        default,
        flags,
        players
            .iter()
            .map(|p| format!("{}:{}", p.id, p.name))
            .collect::<Vec<_>>(),
        player
    )
}

async fn refresh_server_menu(app: &tauri::AppHandle) {
    let found = discover::discover_all(std::time::Duration::from_secs(2))
        .await
        .unwrap_or_default();
    let st = lms_install::lms_install_status().await.ok();
    let running = st.as_ref().map(|s| s.server_running).unwrap_or(false);
    let installed = st.as_ref().map(|s| s.app_installed).unwrap_or(false);
    *MENU_LMS_FLAGS.lock().unwrap() = (running, installed);

    let list = attach_local_server(merge_menu_servers(found), installed, running);
    *MENU_SERVERS.lock().unwrap() = list;

    let current = CURRENT_LMS.lock().unwrap().clone();
    if let Some((h, p)) = current {
        let players = fetch_lms_players(&h, &p).await;
        if !players.is_empty() {
            *MENU_PLAYERS.lock().unwrap() = players;
        }
    }

    rebuild_menus_if_changed(app);
}

fn spawn_server_menu_refresh(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            refresh_server_menu(&app).await;
            tokio::time::sleep(std::time::Duration::from_secs(15)).await;
        }
    });
}

fn emit_select_server(window: &tauri::Window, s: &MenuServer, as_default: bool) {
    *CURRENT_LMS.lock().unwrap() = Some((s.host.clone(), s.port.clone()));
    if as_default {
        *DEFAULT_LMS.lock().unwrap() = Some((s.host.clone(), s.port.clone()));
    }
    let mut payload = HashMap::new();
    payload.insert("host".to_string(), s.host.clone());
    payload.insert("port".to_string(), s.port.clone());
    payload.insert("name".to_string(), s.name.clone());
    payload.insert(
        "default".to_string(),
        if as_default { "1" } else { "0" }.to_string(),
    );
    payload.insert(
        "local".to_string(),
        if s.is_local { "1" } else { "0" }.to_string(),
    );
    let _ = window.show();
    let _ = window.set_focus();
    let _ = window.emit("window-visibility", true);
    let _ = window.emit("select-server", payload);
}

fn handle_menu_id(app: &tauri::AppHandle, id: &str) {
    let Some(window) = app.get_window("main") else {
        return;
    };
    match id {
        "quit" => {
            let _ = window.emit("quit", ());
        }
        "toggle" => {
            toggle_main_window(app);
        }
        "config" | "prefs" | "settings_app" => {
            let _ = window.show();
            let _ = window.set_focus();
            let _ = window.emit("window-visibility", true);
            let _ = window.emit("config", ());
        }
        "settings_wizard" | "srv_install" => {
            let _ = window.show();
            let _ = window.set_focus();
            let _ = window.emit("window-visibility", true);
            let _ = window.emit("open-wizard", ());
        }
        "settings_ui" => {
            let _ = window.show();
            let _ = window.set_focus();
            let _ = window.emit("window-visibility", true);
            let _ = window.emit("open-ui-settings", ());
        }
        "settings_player" => {
            let id = CURRENT_PLAYER.lock().unwrap().clone().unwrap_or_default();
            let players = MENU_PLAYERS.lock().unwrap().clone();
            let name = players
                .iter()
                .find(|p| p.id == id)
                .map(|p| p.name.clone())
                .unwrap_or_default();
            let mut payload = HashMap::new();
            payload.insert("id".to_string(), id);
            payload.insert("name".to_string(), name);
            let _ = window.show();
            let _ = window.set_focus();
            let _ = window.emit("window-visibility", true);
            let _ = window.emit("open-player-settings", payload);
        }
        "srv_def_active" => {
            let current = CURRENT_LMS.lock().unwrap().clone();
            if let Some((h, p)) = current {
                *DEFAULT_LMS.lock().unwrap() = Some((h.clone(), p.clone()));
                let servers = MENU_SERVERS.lock().unwrap().clone();
                if let Some(s) = servers.iter().find(|s| s.host == h && s.port == p) {
                    emit_select_server(&window, s, true);
                } else {
                    let s = MenuServer {
                        host: h,
                        port: p,
                        name: String::new(),
                        is_local: false,
                    };
                    emit_select_server(&window, &s, true);
                }
                let handle = app.clone();
                tauri::async_runtime::spawn(async move {
                    refresh_server_menu(&handle).await;
                });
            }
        }
        "settings_server" | "srv_web" => {
            let _ = window.show();
            let _ = window.set_focus();
            let _ = window.emit("window-visibility", true);
            let _ = window.emit("open-server-settings", ());
        }
        "srv_docs" => {
            let _ = lms_install::lms_open_getting_started();
        }
        "dock" => {
            let next = !SHOW_DOCK_ICON.load(Ordering::SeqCst);
            apply_show_dock_icon(app, next);
            if next {
                let _ = window.show();
                let _ = window.set_focus();
                let _ = window.emit("window-visibility", true);
            }
        }
        "srv_start" => {
            let _ = window.show();
            let _ = window.set_focus();
            let _ = window.emit("window-visibility", true);
            let _ = window.emit("lms-local-starting", ());
            let handle = app.clone();
            tauri::async_runtime::spawn(async move {
                let _ = lms_install::lms_start_local(handle.clone()).await;
                refresh_server_menu(&handle).await;
            });
        }
        "srv_stop" => {
            let handle = app.clone();
            tauri::async_runtime::spawn(async move {
                let _ = lms_install::lms_stop_local(handle.clone()).await;
                refresh_server_menu(&handle).await;
            });
        }
        "srv_restart" => {
            let handle = app.clone();
            tauri::async_runtime::spawn(async move {
                let _ = lms_install::lms_stop_local(handle.clone()).await;
                let _ = lms_install::lms_start_local(handle.clone()).await;
                refresh_server_menu(&handle).await;
            });
        }
        id if id.starts_with("srv_rpc_restart_") || id.starts_with("srv_rpc_shutdown_") => {
            let shutdown = id.starts_with("srv_rpc_shutdown_");
            let idx: usize = id
                .trim_start_matches("srv_rpc_restart_")
                .trim_start_matches("srv_rpc_shutdown_")
                .parse()
                .unwrap_or(usize::MAX);
            let servers = MENU_SERVERS.lock().unwrap().clone();
            if let Some(s) = servers.get(idx).cloned() {
                let msg = if shutdown {
                    format!("Shutting down {}…", if s.name.is_empty() { &s.host } else { &s.name })
                } else {
                    format!("Restarting {}…", if s.name.is_empty() { &s.host } else { &s.name })
                };
                let _ = window.emit("lms-server-cmd", msg.clone());
                let handle = app.clone();
                tauri::async_runtime::spawn(async move {
                    let cmd: &[&str] = if shutdown {
                        &["stopserver"]
                    } else {
                        &["restartserver"]
                    };
                    let res = lms_jsonrpc(&s.host, &s.port, cmd).await;
                    let done = match res {
                        Ok(()) => {
                            if shutdown {
                                format!("{} is shutting down", if s.name.is_empty() { &s.host } else { &s.name })
                            } else {
                                format!("Restart requested for {}", if s.name.is_empty() { &s.host } else { &s.name })
                            }
                        }
                        Err(e) => format!("Server command failed: {e}"),
                    };
                    let _ = handle.emit_all("lms-server-cmd", done);
                    refresh_server_menu(&handle).await;
                });
            }
        }
        "view_plugins" => {
            let _ = window.show();
            let _ = window.set_focus();
            let _ = window.emit("window-visibility", true);
            let _ = window.emit("open-plugins", ());
        }
        "view_refresh" => {
            let _ = window.show();
            let _ = window.set_focus();
            let _ = window.emit("window-visibility", true);
            let _ = window.emit("refresh-player", ());
        }
        "view_home" | "view_search" | "view_mymusic" | "view_radio" | "view_favorites"
        | "view_apps" | "view_extras" | "view_grid" | "view_list" => {
            let _ = window.show();
            let _ = window.set_focus();
            let _ = window.emit("window-visibility", true);
            let action = match id {
                "view_home" => "home",
                "view_search" => "search",
                "view_mymusic" => "top:/mm",
                "view_radio" => "top:/ra",
                "view_favorites" => "top:/fav",
                "view_apps" => "top:/apps",
                "view_extras" => "top:/extra",
                "view_grid" => "grid",
                "view_list" => "list",
                _ => "",
            };
            if !action.is_empty() {
                let _ = window.emit("browse-nav", action);
            }
        }
        "lib_filter" | "lib_sort_name" | "lib_sort_date" | "lib_sort_default" | "lib_sort_added"
        | "lib_sort_reverse" => {
            let _ = window.show();
            let _ = window.set_focus();
            let _ = window.emit("window-visibility", true);
            let action = match id {
                "lib_filter" => "filter",
                "lib_sort_name" => "name",
                "lib_sort_date" => "date",
                "lib_sort_default" => "default",
                "lib_sort_added" => "added",
                "lib_sort_reverse" => "reverse",
                _ => "",
            };
            if !action.is_empty() {
                let _ = window.emit("library-cmd", action);
            }
        }
        id if id.starts_with("srv_use_") || id.starts_with("srv_def_") || id.starts_with("srv_pick_") => {
            let as_default = id.starts_with("srv_def_");
            let idx: usize = id
                .trim_start_matches("srv_use_")
                .trim_start_matches("srv_def_")
                .trim_start_matches("srv_pick_")
                .parse()
                .unwrap_or(usize::MAX);
            let servers = MENU_SERVERS.lock().unwrap().clone();
            if let Some(s) = servers.get(idx) {
                emit_select_server(&window, s, as_default);
                let handle = app.clone();
                tauri::async_runtime::spawn(async move {
                    refresh_server_menu(&handle).await;
                });
            }
        }
        id if id.starts_with("ply_") => {
            let idx: usize = id.trim_start_matches("ply_").parse().unwrap_or(usize::MAX);
            let players = MENU_PLAYERS.lock().unwrap().clone();
            if let Some(p) = players.get(idx) {
                *CURRENT_PLAYER.lock().unwrap() = Some(p.id.clone());
                let mut payload = HashMap::new();
                payload.insert("id".to_string(), p.id.clone());
                payload.insert("name".to_string(), p.name.clone());
                let _ = window.show();
                let _ = window.set_focus();
                let _ = window.emit("window-visibility", true);
                let _ = window.emit("open-player-settings", payload);
                let handle = app.clone();
                tauri::async_runtime::spawn(async move {
                    refresh_server_menu(&handle).await;
                });
            }
        }
        id if id.starts_with("platine_") => {
            let _ = window.emit("platine-action", id.to_string());
        }
        _ => {}
    }
}

fn toggle_main_window(app: &tauri::AppHandle) {
    let Some(window) = app.get_window("main") else {
        return;
    };
    let visible = window.is_visible().unwrap_or(false);
    if visible {
        let _ = window.hide();
        let _ = window.emit("window-visibility", false);
    } else {
        if !HAS_SAVED_POSITION.load(Ordering::SeqCst) && !SHOW_DOCK_ICON.load(Ordering::SeqCst) {
            let _ = window.move_window(Position::TrayCenter);
        }
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        let _ = window.emit("window-visibility", true);
    }
}

fn set_players_ns_alpha(window: &tauri::Window, alpha: f64, duration: f64) {
    #[cfg(target_os = "macos")]
    {
        use objc::runtime::Object;
        use objc::{class, msg_send, sel, sel_impl};
        let ns_window: *mut Object = match window.ns_window() {
            Ok(ptr) => ptr as *mut Object,
            Err(_) => return,
        };
        if ns_window.is_null() {
            return;
        }
        unsafe {
            if duration <= 0.001 {
                let _: () = msg_send![ns_window, setAlphaValue: alpha];
                return;
            }
            let _: () = msg_send![class!(NSAnimationContext), beginGrouping];
            let ctx: *mut Object = msg_send![class!(NSAnimationContext), currentContext];
            let _: () = msg_send![ctx, setDuration: duration];
            let animator: *mut Object = msg_send![ns_window, animator];
            let _: () = msg_send![animator, setAlphaValue: alpha];
            let _: () = msg_send![class!(NSAnimationContext), endGrouping];
        }
    }
}

fn hide_players_hud(app: &tauri::AppHandle) {
    let Some(window) = app.get_window("players") else {
        return;
    };
    if !window.is_visible().unwrap_or(false) {
        let _ = window.emit("players-visibility", false);
        return;
    }
    let gen = PLAYERS_FADE_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let win = window.clone();
    let _ = window.run_on_main_thread(move || {
        set_players_ns_alpha(&win, 0.0, 0.25);
    });
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(260)).await;
        if PLAYERS_FADE_GEN.load(Ordering::SeqCst) != gen {
            return;
        }
        if let Some(w) = app.get_window("players") {
            let w2 = w.clone();
            let _ = w.run_on_main_thread(move || {
                let _ = w2.hide();
                set_players_ns_alpha(&w2, 1.0, 0.0);
            });
            let _ = w.emit("players-visibility", false);
        }
    });
}

#[tauri::command]
fn fade_hide_players_window(app: tauri::AppHandle) {
    hide_players_hud(&app);
}

#[tauri::command]
fn restore_players_window_alpha(app: tauri::AppHandle) {
    PLAYERS_FADE_GEN.fetch_add(1, Ordering::SeqCst);
    if let Some(window) = app.get_window("players") {
        let win = window.clone();
        let _ = window.run_on_main_thread(move || {
            set_players_ns_alpha(&win, 1.0, 0.12);
        });
    }
}

fn show_players_hud(app: &tauri::AppHandle) {
    let Some(window) = app.get_window("players") else {
        return;
    };
    PLAYERS_FADE_GEN.fetch_add(1, Ordering::SeqCst);
    *PLAYERS_SHOWN_AT.lock().unwrap() = Some(Instant::now());
    apply_players_hud_chrome(&window);
    place_players_hud(&window);
    let win = window.clone();
    let _ = window.run_on_main_thread(move || {
        set_players_ns_alpha(&win, 1.0, 0.0);
    });
    let _ = window.set_always_on_top(true);
    let _ = window.show();
    let _ = window.set_focus();
    let current = CURRENT_PLAYER.lock().unwrap().clone().unwrap_or_default();
    let _ = window.emit("players-visibility", true);
    let _ = window.emit("players-current", current);
}

fn toggle_players_hud(app: &tauri::AppHandle) {
    if !SHOW_DOCK_ICON.load(Ordering::SeqCst) {
        toggle_main_window(app);
        return;
    }
    let Some(window) = app.get_window("players") else {
        toggle_main_window(app);
        return;
    };
    let visible = window.is_visible().unwrap_or(false);
    if visible {
        hide_players_hud(app);
    } else {
        show_players_hud(app);
    }
}

#[cfg(target_os = "macos")]
fn emit_media_key(app: &tauri::AppHandle, action: &'static str) {
    static LAST: Mutex<Option<(String, Instant)>> = Mutex::new(None);
    {
        let mut last = LAST.lock().unwrap();
        if let Some((prev, at)) = last.as_ref() {
            if prev == action && at.elapsed() < Duration::from_millis(280) {
                return;
            }
        }
        *last = Some((action.to_string(), Instant::now()));
    }
    let _ = app.emit_all("media-key", action);
}

#[cfg(target_os = "macos")]
fn media_key_action(event: *mut objc::runtime::Object) -> Option<&'static str> {
    use objc::{msg_send, sel, sel_impl};
    if event.is_null() {
        return None;
    }
    unsafe {
        let subtype: isize = msg_send![event, subtype];
        // NX_SUBTYPE_AUX_CONTROL_BUTTONS
        if subtype != 8 {
            return None;
        }
        let data1: isize = msg_send![event, data1];
        let key_code = (data1 & 0xFFFF0000) >> 16;
        let key_flags = data1 & 0xFFFF;
        let key_state = (key_flags & 0xFF00) >> 8;
        let key_repeat = key_flags & 0x1;
        // 0xA is key down. Ignore repeats so one press is one action.
        if key_state != 0xA || key_repeat != 0 {
            return None;
        }
        match key_code {
            16 => Some("playpause"),
            17 | 19 => Some("next"),
            18 | 20 => Some("previous"),
            _ => None,
        }
    }
}

#[cfg(target_os = "macos")]
fn install_media_keys(app: tauri::AppHandle) {
    use block::ConcreteBlock;
    use objc::runtime::{Object, BOOL, YES};
    use objc::{class, msg_send, sel, sel_impl};

    #[link(name = "MediaPlayer", kind = "framework")]
    extern "C" {}

    let mask: u64 = 1 << 14; // NSEventMaskSystemDefined

    let local_app = app.clone();
    let local = ConcreteBlock::new(move |event: *mut Object| -> *mut Object {
        if let Some(action) = media_key_action(event) {
            emit_media_key(&local_app, action);
            return std::ptr::null_mut();
        }
        event
    });
    let local = local.copy();

    let global_app = app.clone();
    let global = ConcreteBlock::new(move |event: *mut Object| {
        unsafe {
            let ns_app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
            let active: BOOL = if ns_app.is_null() {
                YES
            } else {
                msg_send![ns_app, isActive]
            };
            // The local monitor already handled the focused app.
            if active == YES {
                return;
            }
        }
        if let Some(action) = media_key_action(event) {
            emit_media_key(&global_app, action);
        }
    });
    let global = global.copy();

    unsafe {
        // Pass the heap block itself (`&*rc`), not `&RcBlock`. NSEvent copies
        // the handler and crashes if it is handed the Rust wrapper.
        let _: *mut Object = msg_send![
            class!(NSEvent),
            addLocalMonitorForEventsMatchingMask: mask
            handler: &*local
        ];
        let _: *mut Object = msg_send![
            class!(NSEvent),
            addGlobalMonitorForEventsMatchingMask: mask
            handler: &*global
        ];
        std::mem::forget(local);
        std::mem::forget(global);

        let center: *mut Object = match objc::runtime::Class::get("MPRemoteCommandCenter") {
            Some(cls) => msg_send![cls, sharedCommandCenter],
            None => std::ptr::null_mut(),
        };
        if !center.is_null() {
            let bind = |getter: objc::runtime::Sel, action: &'static str, app: tauri::AppHandle| {
                unsafe {
                    let cmd: *mut Object = msg_send![center, performSelector: getter];
                    if cmd.is_null() {
                        return;
                    }
                    let _: () = msg_send![cmd, setEnabled: YES];
                    let block = ConcreteBlock::new(move |_event: *mut Object| -> isize {
                        emit_media_key(&app, action);
                        0
                    });
                    let block = block.copy();
                    let _: *mut Object = msg_send![cmd, addTargetWithHandler: &*block];
                    std::mem::forget(block);
                }
            };
            bind(sel!(togglePlayPauseCommand), "playpause", app.clone());
            bind(sel!(playCommand), "playpause", app.clone());
            bind(sel!(pauseCommand), "playpause", app.clone());
            bind(sel!(nextTrackCommand), "next", app.clone());
            bind(sel!(previousTrackCommand), "previous", app);
        }
    }
}

fn main() {
    #[cfg(not(debug_assertions))]
    shell_proxy::reserve();

    let builder = tauri::Builder::default()
        .on_page_load(|window, payload| {
            // Catalina WKWebView does not run scripts from tauri://, so eval
            // never moves the page. AppKit loads the HTTP shell instead.
            #[cfg(all(target_os = "macos", not(debug_assertions)))]
            {
                let url = payload.url();
                let custom = url.starts_with("tauri:") || url.contains("tauri.localhost");
                if custom {
                    navigate_window_to_shell(&window);
                }
            }
            #[cfg(not(all(target_os = "macos", not(debug_assertions))))]
            {
                let _ = (&window, payload);
            }
        })
        .setup(|app| {
            // The release window leaves tauri:// for http://localhost:14321.
            // Tauri treats that as a remote page and drops IPC unless the
            // domain is allowlisted. Without IPC the wizard splash never advances.
            app.ipc_scope().configure_remote_access(
                tauri::scope::ipc::RemoteDomainAccessScope::new("localhost")
                    .allow_on_scheme("http")
                    .add_window("main")
                    .add_window("players")
                    .enable_tauri_api(),
            );

            #[cfg(not(debug_assertions))]
            shell_proxy::start(app.handle());
            #[cfg(all(target_os = "macos", not(debug_assertions)))]
            SHELL_NAV_READY.store(true, Ordering::SeqCst);

            let show_dock = load_show_dock_icon_pref(app);
            SHOW_DOCK_ICON.store(show_dock, Ordering::SeqCst);

            #[cfg(target_os = "macos")]
            app.set_activation_policy(if show_dock {
                tauri::ActivationPolicy::Regular
            } else {
                tauri::ActivationPolicy::Accessory
            });

            if let Some(window) = app.get_window("main") {
                apply_window_vibrancy(&window);
                if let Some((w, h, x, y)) = load_window_geometry(app) {
                    let _ = window.set_size(tauri::LogicalSize::new(w, h));
                    let _ = window.set_position(tauri::LogicalPosition::new(x, y.max(28.0)));
                    HAS_SAVED_POSITION.store(true, Ordering::SeqCst);
                }
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
                let _ = window.emit("window-visibility", true);
            }
            *APP_HANDLE.lock().unwrap() = Some(app.handle());
            // Drop a leftover soft-preview squeezelite. One LMS player is enough.
            squeezelite_reap_previews();
            #[cfg(all(target_os = "macos", not(debug_assertions)))]
            {
                if let Some(window) = app.get_window("main") {
                    navigate_window_to_shell(&window);
                }
            }
            if let Some(players) = app.get_window("players") {
                apply_players_hud_chrome(&players);
                let _ = players.hide();
            }
            rebuild_app_server_menu(&app.handle());
            rebuild_platine_menu(&app.handle());
            #[cfg(target_os = "macos")]
            bind_app_quit_item();

            // Default hotkey — frontend may replace it after loading config
            if let Err(err) = register_toggle_hotkey(&app.handle(), DEFAULT_TOGGLE_HOTKEY) {
                eprintln!("{}", err);
            }

            spawn_server_menu_refresh(app.handle());

            #[cfg(target_os = "macos")]
            install_media_keys(app.handle());
            #[cfg(target_os = "macos")]
            install_wizard_option_reveal();

            #[cfg(target_os = "macos")]
            {
                let handle = app.handle();
                tauri::async_runtime::spawn(async move {
                    for ms in [200u64, 700, 1500] {
                        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                        let h = handle.clone();
                        let _ = handle.run_on_main_thread(move || {
                            if let Some(window) = h.get_window("main") {
                                #[cfg(not(debug_assertions))]
                                navigate_window_to_shell(&window);
                                align_traffic_lights(&window);
                            }
                        });
                    }
                });
            }

            Ok(())
        })
        .plugin(tauri_plugin_positioner::init())
        .system_tray(SystemTray::new().with_menu(build_tray_menu()))
        .on_system_tray_event(move |app, event| {
            tauri_plugin_positioner::on_tray_event(app, &event);

            match event {
                SystemTrayEvent::LeftClick {
                    position,
                    size,
                    ..
                } => {
                    *TRAY_GEOM.lock().unwrap() =
                        Some((position.x, position.y, size.width, size.height));
                    if SHOW_DOCK_ICON.load(Ordering::SeqCst) {
                        toggle_players_hud(app);
                    } else {
                        toggle_main_window(app);
                    }
                    let handle = app.clone();
                    tauri::async_runtime::spawn(async move {
                        refresh_server_menu(&handle).await;
                    });
                }
                SystemTrayEvent::MenuItemClick { id, .. } => {
                    handle_menu_id(app, id.as_str());
                }
                _ => {}
            }
        });

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    let builder = builder.menu(build_app_menu()).on_menu_event(|event| {
        handle_menu_id(&event.window().app_handle(), event.menu_item_id());
    });

    builder
        .on_window_event(|event| match event.event() {
            tauri::WindowEvent::Focused(is_focused) => {
                if event.window().label() == "players" {
                    let _ = event.window().emit("players-focus", *is_focused);
                    if !*is_focused {
                        let recent = PLAYERS_SHOWN_AT
                            .lock()
                            .ok()
                            .and_then(|g| *g)
                            .map(|t| t.elapsed() < Duration::from_millis(400))
                            .unwrap_or(false);
                        if !recent {
                            hide_players_hud(&event.window().app_handle());
                        }
                    }
                    return;
                }
                if !is_focused && !SHOW_DOCK_ICON.load(Ordering::SeqCst) {
                    let _ = event.window().emit("window-visibility", false);
                    event.window().hide().unwrap();
                }
            }
            tauri::WindowEvent::Resized(_) => {
                #[cfg(target_os = "macos")]
                if event.window().label() == "main" {
                    align_traffic_lights(event.window());
                }
            }
            tauri::WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                let _ = event.window().hide();
                if event.window().label() == "players" {
                    let _ = event.window().emit("players-visibility", false);
                } else {
                    let _ = event.window().emit("window-visibility", false);
                }
            }
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            detect_lms_server,
            detect_lms_servers,
            set_current_lms,
            set_default_lms,
            set_current_player,
            get_current_player,
            lms_player_command,
            lms_mixer_volume,
            set_sort_menu,
            set_view_layout,
            set_traffic_lights_hidden,
            position_players_window,
            fade_hide_players_window,
            restore_players_window_alpha,
            set_menu_players,
            set_platine_menu,
            this_machine_hosts,
            squeezelite_start,
            squeezelite_stop,
            squeezelite_running,
            squeezelite_reap_previews,
            mark_window_positioned,
            set_show_dock_icon,
            is_show_dock_icon,
            set_detached,
            is_detached,
            get_toggle_hotkey,
            set_toggle_hotkey,
            lms_install::lms_install_status,
            lms_install::lms_channels,
            lms_install::lms_package_info,
            lms_install::lms_download_and_install,
            lms_install::lms_start_local,
            lms_install::lms_stop_local,
            lms_install::lms_wait_ready,
            lms_install::lms_open_getting_started,
            lms_install::lms_open_material,
            lms_install::lms_probe_server,
            lms_plugins::lms_machine_name,
            lms_plugins::lms_pick_library_folder,
            lms_plugins::lms_wizard_run
        ])
        .run(tauri::generate_context!())
        .expect("Error while running Lyrion");
}
