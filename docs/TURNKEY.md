# Lyrion — turnkey desktop client

**Goal:** Lyrion Minim becomes **Lyrion** — one signed app on macOS, Windows, and Linux that:

1. Detects a Lyrion Music Server on the LAN (or localhost)
2. Offers to **install LMS locally** (always available in Settings)
3. Downloads the **official** community package, runs the normal OS install flow
4. Opens / starts the server and connects Material Skin + built-in Squeezelite

Official install docs (source of package URLs and user steps):

- https://lyrion.org/getting-started/
- https://lyrion.org/getting-started/mac-install/

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│  Lyrion (Tauri shell)                                   │
│  ┌──────────────┐  ┌─────────────┐  ┌─────────────────┐ │
│  │ Squeezelite  │  │ Material    │  │ LMS install     │ │
│  │ + Preview    │  │ iframe      │  │ detect/download │ │
│  └──────────────┘  └─────────────┘  └────────┬────────┘ │
└──────────────────────────────────────────────┼──────────┘
                                               │ official
                                               ▼
                    downloads.lms-community.org (signed LMS packages)
                                               │
                                               ▼
                              Lyrion Music Server :9000
```

| Concern | Owner | Notes |
|--------|--------|--------|
| LMS binary / service | **LMS Community** installers | We do not bundle LMS; we download official builds |
| Client UI + Squeezelite | **This app** | Signed as `org.lyrion.app` |
| Material Skin | LMS plugin (your fork / deploy) | App loads `/material` |

### Why not embed LMS inside the app?

- LMS is a full Perl stack + plugins; embedding would force dual-signing, huge binaries, and fight official updates.
- Official Windows packages are already **SignPath-signed**; macOS DMG is community-notarized; reusing them is the safe path.
- User mental model matches lyrion.org: “install the server, then use a client.”

### Install flow (per OS)

| OS | Package | App action | User may still need |
|----|---------|------------|---------------------|
| **macOS** | `LyrionMusicServer-{ver}.dmg` | Download → `open` DMG + Applications | Drag to Applications, menu bar → **Start Service** |
| **Windows** | `LyrionMusicServer-{ver}-win64.exe` | Download → launch installer | Complete wizard (service usually auto-starts) |
| **Linux** | `lyrionmusicserver_{ver}_amd64.deb` (or `_arm.deb`) | Download → `pkexec apt-get install -y` | Admin password; optional `systemctl start` |

### Release channels (binary type)

User picks a channel in Settings → **Install Lyrion Music Server**:

| Channel id | Label | Source |
|------------|--------|--------|
| `latest` | Latest release (recommended) | Official stable from catalog |
| `stable` | Stable nightly | Bug-fix branch builds (`/nightly/…`) |
| `dev` | Development | Feature branch builds (`/nightly/…`) |

**URL resolution order**

1. Scrape **live** https://lyrion.org/downloads/ (+ server-repository page)  
2. HEAD-check each candidate (skip 404s — GitHub `downloads.yaml` is often stale for nightlies)  
3. YAML catalog only if HEAD succeeds  
4. Fallback: pinned **9.1.1** latest  

**macOS install:** `hdiutil attach -nobrowse` (no Finder window) → `ditto` `.app` into `/Applications` or `~/Applications` → `xattr -dr quarantine` → `open` the app. No manual drag-and-drop.

After install, Lyrion polls `http://127.0.0.1:9000/` and points the client at localhost.

## First-run wizard

On a machine with no saved config, Lyrion shows a splash (UDP discovery runs underneath), then a **list of every LMS** on the LAN plus **Install on this computer**. Connecting to an existing server does not push plugins.

Local install (3–4 pages): server name, library folder, language, plugin groups. Material is downloaded from Craig’s GitHub release. UPnP / Cast / AirPlay bridges install **one helper binary for this OS/arch** (`chmod 0555`, quarantine stripped) into the user `Plugins/` folder — not `Cache/InstalledPlugins`. Built-in Groups / Don’t Stop The Music are enabled via JSON-RPC after the server is up.

Multi-protocol devices (e.g. a WiiM exposing Squeezelite + UPnP + Cast + AirPlay) stay four LMS clients; Material should cluster them as **one endpoint** (canonical = Squeezelite). That is UI grouping, not LMS sync.

## Tray / menu (server)

Same **Server** actions on every OS: status, start, stop, restart, open Material, install.

| OS | Where |
|----|--------|
| **macOS** | Status item (right of the menu bar). Native **Server** menu too when the Dock icon is on. |
| **Windows** | System tray icon (same menu). |
| **Linux** | StatusNotifier / AppIndicator **if the desktop has a tray** (KDE, XFCE, Cinnamon, GNOME + AppIndicator extension). **Always:** window menu **Lyrion** + **Server** — GNOME vanilla has no tray. Start/stop uses `systemctl` (user unit, then polkit). Package `libayatana-appindicator3-1` (Debian/Ubuntu) for the icon to appear. |

---

## Rust / Tauri commands

| Command | Purpose |
|---------|---------|
| `detect_lms_server` | UDP discovery (prefer localhost) |
| `detect_lms_servers` | All discovery replies (~3s) |
| `lms_install_status` | Local app present? HTTP on :9000? |
| `lms_package_info` | Official URL for this platform |
| `lms_download_and_install` | Download + launch install path |
| `lms_start_local` | Best-effort start + wait ready |
| `lms_wait_ready` | Poll HTTP until ready / timeout |
| `lms_open_getting_started` | Open lyrion.org install docs |

Progress events: `lms-install-progress` `{ stage, percent, message }`.

## Product rename

| Before | After |
|--------|--------|
| Product name “Lyrion Minim” | **Lyrion** |
| Bundle id `com.lyrion-minim.app` | `org.lyrion.app` |
| Default player name | `Lyrion` |

Repo folder / Cargo package may stay `lyrion-minim` until a full rename PR.

## Code signing & notarization (release)

### macOS

1. Apple Developer ID Application certificate  
2. Build: `pnpm tauri build`  
3. Notarize: `xcrun notarytool submit … --wait` then staple  
4. Ship `.dmg` or `.app` zip  

CI secrets: `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_ID`, `APPLE_TEAM_ID`, `APPLE_APP_SPECIFIC_PASSWORD`.

### Windows

1. SignPath / EV or standard Authenticode (LMS itself uses SignPath Foundation)  
2. Sign the Tauri `.msi` / `.exe` after build  
3. Optional: SmartScreen reputation builds over time  

### Linux

1. Ship `.AppImage` / `.deb`  
2. Optional: GPG-sign packages and publish checksums on GitHub Releases  

### LMS packages

We **do not re-sign** LMS installers. Users install community-signed binaries from `downloads.lms-community.org` (Windows SignPath policy documented on lyrion.org).

## Release checklist

- [ ] Bump `package.version` in `tauri.conf.json` + git tag  
- [ ] Confirm `LMS_FALLBACK_VERSION` matches https://lyrion.org/downloads/  
- [ ] `pnpm tauri build` on macOS / Windows / Linux runners  
- [ ] Sign + notarize (mac), Authenticode (win), checksums (linux)  
- [ ] Smoke: first-run settings → Download & install → Start → Material loads  
- [ ] Smoke: auto-detect remote LMS on LAN  
- [ ] Smoke: Squeezelite + soft preview player  

## CI skeleton

See `.github/workflows/release.yml`. A tag `v*` builds macOS Apple Silicon, macOS Intel, Windows x64, Windows ARM64, Linux x64, and Linux ARM64. The bundles are unsigned unless Apple or Authenticode secrets are added later. The Windows ARM64 build leaves out Squeezelite. There is no native Windows ARM sidecar for that player.

## Out of scope / later

- Silent unattended LMS install on macOS (Gatekeeper + Start Service menu) without user steps  
- Bundling a private LMS build inside the app  
- Auto-update of LMS from inside Lyrion (point users at official updater / re-download)  
- Docker-only install path for desktop (document only)  

## Security notes

- Downloads only from `https://downloads.lms-community.org/…`  
- No arbitrary shell from the webview  
- Linux elevation only via `pkexec` for the single `.deb` path  
- Soft-preview player stays local and hidden from other UIs (`hidePlayers`)  
