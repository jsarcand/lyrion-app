# Lyrion.app

Version **1.0b** (bundle version `1.0.0-b`).

Turnkey desktop client for [Lyrion Music Server](https://lyrion.org/).

[Lire en français](#français)

## Download

The compiled bundles are on the release page:

https://github.com/jsarcand/lyrion-app/releases/tag/v1.0b

| System | Processor | Package |
| --- | --- | --- |
| macOS 10.13 and later | Apple Silicon | DMG |
| macOS 10.13 and later, including Catalina | Intel | DMG |
| Windows 10 and 11 | 64-bit Intel or AMD | EXE and MSI |
| Linux | x86-64 | deb and AppImage |
| Linux | ARM64 | deb |

These builds are unsigned. On macOS, if Gatekeeper blocks the app:

```bash
xattr -cr /Applications/Lyrion.app
```

Windows on ARM is not in this release. The bundled player is Squeezelite, and this project has no native Windows ARM build of it.

Linux packages expect ALSA and the usual decoder libraries (FLAC, Vorbis, mpg123, FAAD, MAD) on the system. The deb does not vendor those libraries.

## What it does

- Discover LMS on the local network (or prefer a server already on this machine)
- **Install LMS locally** from official packages ([getting started](https://lyrion.org/getting-started/), [macOS](https://lyrion.org/getting-started/mac-install/))
- Built-in Squeezelite player (+ soft preview player)
- Material Skin UI in a native shell (menubar / floating window)

See **[docs/TURNKEY.md](docs/TURNKEY.md)** for architecture, install flows, signing, and release CI.

## Platforms

| OS | Processor | Status |
| --- | --- | --- |
| macOS 10.13 and later | Apple Silicon, Intel | Built |
| Windows 10 and 11 | x64 | Built |
| Linux | x86-64, ARM64 | Built |
| Windows on ARM | ARM64 | Not in 1.0b |

## Development

Tauri 1 + Vue 3 + TypeScript. Ships pre-built Squeezelite sidecars per target.

```bash
pnpm install
pnpm tauri dev
```

```bash
pnpm tauri build
```

## Installation

1.0b is unsigned. A signed release needs an Apple Developer ID and a Windows code-signing certificate, which are not set up here. macOS may ask you to remove the quarantine attribute, as shown above.

LMS itself is installed via the official community installers opened by the app. This project does not re-sign those installers.

## Provenance

This client continues the public source of [longshadowdev/lyrion-minim](https://github.com/longshadowdev/lyrion-minim). That repository has no license file. The bundled player is [Squeezelite](https://github.com/ralph-irving/squeezelite), released under GPLv3.

## Acknowledgements

- [Lyrion Music Server](https://github.com/LMS-Community)
- [LMS Material skin](https://github.com/CDrummond/lms-material)
- [Squeezelite](https://github.com/ralph-irving/squeezelite)
- [mprisqueeze](https://github.com/jecaro/mprisqueeze) (UDP discover inspiration)
- [Tauri](https://tauri.app/)
- [LMS Forums](https://forums.lyrion.org/forum/)
- [Lyrion Minim](https://github.com/longshadowdev/lyrion-minim), the project this client continues

---

## Français

Version **1.0b**. Client de bureau pour [Lyrion Music Server](https://lyrion.org/).

[Read in English](#lyrionapp)

### Téléchargement

https://github.com/jsarcand/lyrion-app/releases/tag/v1.0b

| Système | Processeur | Paquet |
| --- | --- | --- |
| macOS 10.13 et plus récent | Apple Silicon | DMG |
| macOS 10.13 et plus récent, y compris Catalina | Intel | DMG |
| Windows 10 et 11 | Intel ou AMD 64 bits | EXE et MSI |
| Linux | x86-64 | deb et AppImage |
| Linux | ARM64 | deb |

Les binaires ne sont pas signés. Sur macOS, si Gatekeeper bloque l’application :

```bash
xattr -cr /Applications/Lyrion.app
```

Windows sur ARM n’est pas dans cette version. Le lecteur inclus est Squeezelite, et ce projet n’en a pas de compilation native pour Windows ARM.

Les paquets Linux s’attendent à trouver ALSA et les décodeurs habituels (FLAC, Vorbis, mpg123, FAAD, MAD) sur le système.

### Ce que fait l’application

- Trouve un serveur Lyrion sur le réseau local, ou un serveur déjà présent sur la machine
- Peut installer LMS à partir des paquets officiels
- Lecteur Squeezelite intégré
- Interface Material dans une fenêtre native

Le détail d’architecture, d’installation et de signature est dans [docs/TURNKEY.md](docs/TURNKEY.md).

### Origine

Ce client poursuit le code public de [longshadowdev/lyrion-minim](https://github.com/longshadowdev/lyrion-minim). Ce dépôt n’a pas de fichier de licence. Le lecteur inclus est [Squeezelite](https://github.com/ralph-irving/squeezelite), sous GPLv3.
