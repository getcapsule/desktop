# capsule

A desktop music player with a Rust audio core, public streaming sources, and your own library.

![Platforms](https://img.shields.io/badge/platforms-Windows%20%7C%20Linux-30363d?style=flat-square)
![Tauri](https://img.shields.io/badge/Tauri-2-24c8db?style=flat-square)
![Status](https://img.shields.io/badge/status-in%20development-d4a24c?style=flat-square)
[![License](https://img.shields.io/badge/license-GPL--3.0--only-5686bd?style=flat-square)](LICENSE)

[Releases](https://github.com/yvesdeniz/capsule/releases) | [Build from source](#building-from-source) | [Issues](https://github.com/yvesdeniz/capsule/issues) | [Discord](https://discord.gg/77np9hVV7M)

## Contents

1. [Overview / About](#overview--about)
2. [Performance](#performance)
3. [Features](#features)
4. [Tech Stack](#tech-stack)
5. [Installation](#installation)
6. [Building from Source](#building-from-source)
7. [Configuration & Usage](#configuration--usage)
8. [Roadmap](#roadmap)
9. [FAQ](#faq)
10. [Contributing](#contributing)
11. [License](#license)

## Overview / About

Capsule brings local music, self-hosted libraries, and public streaming audio into one player. Import a playlist, browse its songs alongside your library, and play through the native audio engine.

**Playback sources and import providers are separate.** A Spotify playlist supplies metadata. Its tracks play through an available local file, Navidrome recording, YouTube recording, or SoundCloud stream.

| Service | Playback | Library / metadata |
| --- | --- | --- |
| Local files | Native file playback | Folder scanning and audio tags |
| Navidrome | Native playback through Subsonic | Connected server library |
| YouTube / YouTube Music | Public audio streams | Public track, album, and playlist imports |
| SoundCloud | Public audio streams | Public imports and song, artist, and album search |
| Spotify | Metadata only | Public track, album, and playlist imports |
| Apple Music | Metadata only | Public song, album, and playlist imports |

Capsule is in active development. Windows has the most recent real-device streaming validation. Linux is a release-build target, with broader playback and desktop acceptance testing ongoing.

## Performance

| Area | Implementation |
| --- | --- |
| Browsing | Local SQLite metadata, revision-cached views, paged queries, and virtualized lists |
| Streaming | Reused HTTP clients, bounded concurrency, and progressive buffering |
| YouTube transitions | Decoded audio prepared for the next verified recording, with sample-boundary handoff when ready |
| Recovery | Expiring stream URL caches, bounded retries, and automatic attempts across plausible recordings |
| Background work | Cancellation of superseded work and coalesced listening-state writes |

The YouTube handoff pipeline mixes at 48 kHz stereo. It is not bit-perfect output. Unavailable or unprepared successors can still cause a gap. Local files and Navidrome use a separate loading path.

Measure Capsule together with its WebView processes. The [Windows process sampler](scripts/performance-measure.ps1) records private memory, working set, and CPU usage. Compare development and release builds separately, using the same library, playback state, and cache conditions.

## Features

### Library and discovery

- Songs, albums, artists, playlists, favorites, and listening history.
- SoundCloud public catalog search for songs, artists, and albums.
- Public URL imports without manually supplying account tokens in the normal workflow.
- Library views populated from saved imports, with playlist order and repeated entries preserved.
- Cached search and sorting, local playlists, and local edits to imported collections.

### Playback and downloads

- Native playback of supported formats, including MP3, WAV, FLAC, AAC, and ALAC.
- Public YouTube audio and supported progressive or unencrypted HLS SoundCloud streams.
- Queue editing, play next, shuffle, repeat, seeking, and undo for queue changes.
- Automatic recording recovery before manual selection becomes necessary.
- Track and album downloads with progress, cancellation, retry, and folder reveal.
- Original encoded audio downloads and local playback links for downloaded canonical recordings.
- Paused session restoration, output-device selection, and a sleep timer.

### Lyrics and desktop integration

- Synced and plain lyrics from LRCLIB, including lookups for imported tracks.
- Click-to-seek lyrics, timing calibration, and retry for failed requests.
- System tray, close-to-tray behavior, and media controls.
- Windows taskbar playback buttons and Snap Layout support.
- Optional Last.fm scrobbling and Discord Rich Presence with cover artwork.
- Theme editing, previews, and TOML theme import/export.

## Tech Stack

| Layer | Technologies |
| --- | --- |
| Desktop shell | Tauri 2, WebView2 / WebKitGTK |
| Interface | React 19, TypeScript, Tailwind CSS 4, Vite |
| Core | Rust, Tokio |
| Audio | rodio, Symphonia, CPAL |
| Storage | SQLite through rusqlite, TOML settings, disk caches |
| Metadata and networking | reqwest, RustyPipe, public provider adapters, Subsonic |
| File metadata | lofty |
| Desktop services | souvlaki, OS credential storage, Discord Rich Presence |
| Development | Bun, Cargo, Vitest, Testing Library, Clippy |

## Installation

Check the [Releases page](https://github.com/yvesdeniz/capsule/releases) for available packages. If a package for your platform is unavailable, build from source.

| Platform | Package | Requirements |
| --- | --- | --- |
| Windows 10 / 11 | NSIS `.exe` installer | WebView2 Evergreen runtime; the installer can bootstrap it |
| Linux x86_64 | `.deb` or `.AppImage` | WebKitGTK 4.1, ALSA, D-Bus, and an AppIndicator-compatible tray library |

On Linux, GNOME Keyring, KWallet, or another Secret Service provider is needed for persistent credential storage. Media controls use MPRIS over the desktop session bus. Install `.deb` packages through your package manager; mark an AppImage executable before running it.

Acrylic and mica window materials are Windows-specific. Other platforms use the solid theme.

## Building from Source

### Prerequisites

- A current stable Rust toolchain, installed through rustup.
- Bun. Tauri's frontend development and build hooks invoke Bun directly.
- Git.
- Windows: MSVC build tools with the **Desktop development with C++** workload.
- Linux: the development packages below.

For Ubuntu 22.04, matching the release workflow:

```bash
sudo apt-get update
sudo apt-get install -y \
  build-essential patchelf file \
  libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev \
  libasound2-dev libdbus-1-dev libxdo-dev libssl-dev
```

### Run in development

```bash
git clone https://github.com/yvesdeniz/capsule.git
cd capsule
bun install --frozen-lockfile
bun run app
```

The first Rust build takes longer while dependencies compile.

### Build an installer

Windows:

```bash
bun run app:build
```

Linux:

```bash
bun run app:build --bundles deb,appimage
```

Packages are written to `src-tauri/target/release/bundle/`. The [release workflow](.github/workflows/release.yml) defines the platform build matrix.

### Run checks

```bash
bun run typecheck
bun run test
bun run build
cargo test --manifest-path src-tauri/Cargo.toml --lib
cargo clippy --manifest-path src-tauri/Cargo.toml --lib -- -D warnings
```

Tests requiring public services or real audio output are ignored by default. Run those explicitly when validating streaming or downloads.

## Configuration & Usage

### First run

1. Choose a playback source in setup or Settings.
2. For local files, select your music folders. For Navidrome, connect your server.
3. For public streaming, import a supported URL or search SoundCloud in Discover.
4. Open a playlist, album, or song and start playback. Use the queue or a track's More menu for listening actions.

Public YouTube and SoundCloud playback does not require an account connection. Private libraries, restricted recordings, and encrypted streams are outside this public playback workflow.

### Import a collection

Open **Settings > Import**, paste a supported public URL, preview the metadata, and save it as a Capsule playlist. Imports persist across restarts and source changes. Incomplete collections or unavailable tracks produce preview warnings.

Title, artist, version, and duration evidence guide automatic playback matching. A manual recording choice is a fallback when recovery cannot find suitable playable audio.

Public import adapters are unofficial and can need maintenance when provider websites change. Advanced metadata API access is optional and separate from the normal import and playback flows.

### Download music

Use **Download** in Discover, **Download all** in an album or playlist, or **More > Download track**. Choose a destination folder on the first download. Open **Downloads** to monitor progress, cancel, retry, or show a completed file.

Two transfers run at once. Each batch supports up to 500 tracks, with a 500-entry session queue/history limit and a 2 GB per-file limit. SoundCloud HLS transfers have additional manifest and audio limits. Existing files are not overwritten, and downloads keep the source's encoded format. Activity history is session-only; completed files and saved local playback links persist.

### Settings and credentials

The data directory contains `config.toml`, library databases, listening state, and caches:

| Platform | Default data directory |
| --- | --- |
| Windows | `%APPDATA%\com.deniz.capsule\` |
| Linux | `~/.local/share/com.deniz.capsule/` |

A minimal configuration:

```toml
source = "local" # local | navidrome | youtube | soundcloud

[local]
folders = []

[navidrome]
url = "https://music.example.com"
username = "your-username"

[lyrics]
offset_ms = 0
```

Use Settings for normal configuration. Passwords and account sessions use the OS credential store. Optional Last.fm and Discord application credentials can come from settings or compile-time environment variables; runtime settings take precedence. See [.env.example](.env.example) for supported keys and development overrides. Review configuration before sharing it in a bug report.

Desktop preferences use `desktop-preferences.json`. Listening state uses `listening-state.sqlite3`. Restored playback stays paused until you press Play.

### Keyboard shortcuts

Playback shortcuts apply outside text fields.

| Shortcut | Action |
| --- | --- |
| `Space` | Play / pause |
| `Alt+Left` / `Alt+Right` | Previous / next track |
| `Ctrl+K` | Command palette |
| `Ctrl+F` | Focus search |
| `Ctrl+Q` | Open queue |
| `Ctrl+A` | Select current library results |
| `Ctrl+Z` | Undo a queue change while the queue is open |

## Roadmap

- [x] Interface redesign, with the visual direction being developed separately.
- [ ] A high-quality equalizer with useful presets and precise controls.
- [ ] Further playback recovery and transition testing across Windows and Linux.
- [ ] Repeatable release-build benchmarks for memory, CPU, startup, and track changes.
- [ ] Continued lyrics, download, and provider-adapter improvements.
- [ ] Review external requests and keep optional integrations explicit.

Downloads, lyrics improvements, and the audio-format readout are implemented. This roadmap lists further work rather than release-date commitments.

## FAQ

### Do Spotify imports require Premium?

Public Spotify URL imports do not require Premium or a developer application. Optional authorized metadata API access has separate provider requirements. Capsule does not use Spotify for audio playback.

### Does importing from Apple Music make it a playback source?

No. Apple Music supplies import metadata. Playback uses one of Capsule's supported audio sources. The former Apple Music playback engine has been removed.

### Why does a downloaded YouTube track use M4A instead of MP3?

Capsule saves the original AAC audio in its MP4/M4A container. Converting it to MP3 would add processing and another lossy encoding step. A container name and an audio codec describe different parts of the file.

### Are downloads lossless?

They preserve the available source audio. A FLAC source remains lossless; saving a lossy stream does not turn it into lossless audio. Capsule does not re-encode normal downloads.

### Why can a public recording still fail to play?

Availability can depend on the recording, region, provider restrictions, or an upstream API change. Capsule refreshes eligible stream URLs and tries suitable alternatives before asking for a manual choice.

### Why are lyrics missing or temporarily unavailable?

LRCLIB may not have that recording, and its service can be unavailable. Use Retry for a fresh lookup. A failed request is not stored as a permanent missing result.

### Why does Task Manager show WebView processes?

The interface runs in the system webview. Include those processes when measuring Capsule's total resource usage. Audio decoding and playback state live in Rust.

### What leaves my machine?

Online imports, search, lyrics, and streaming contact their respective providers. Library data and listening state are stored locally. Last.fm and Discord are optional integrations. Signed Navidrome cover sharing requires its explicit opt-in setting; Capsule does not upload private local artwork for presence.

## Contributing

Bug reports, focused fixes, and improvements to provider reliability are welcome. Use [Issues](https://github.com/yvesdeniz/capsule/issues) for bugs and proposals, or [Discord](https://discord.gg/77np9hVV7M) for setup questions.

For a bug report, include your OS, Capsule version, playback source, reproduction steps, and the exact error. Developer mode provides diagnostics in Settings. Remove credentials and personal server details from reports.

Before opening a pull request:

1. Keep the change focused and describe the resulting behavior.
2. Run the relevant frontend and Rust checks from Building from Source.
3. Add regression coverage for behavior changes and report any live tests performed.
4. Preserve source boundaries, bounded background work, and cancellation behavior.

New Tauri commands must be registered in all three places:

- `src-tauri/src/lib.rs`: command handler registration.
- `src-tauri/build.rs`: generated command permission.
- `src-tauri/capabilities/main.json`: permission granted to the main window.

Keep playback and queue state in Rust, and keep credentials and signed media addresses out of logs and frontend state.

## License

Capsule is licensed under **GPL-3.0-only**. See [LICENSE](LICENSE).

Capsule is an independent project and is not affiliated with or endorsed by the music services it supports. Service names and trademarks belong to their owners.
