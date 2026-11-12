# discord-clip

Instant-replay for Discord voice chat on Windows. Sits in the system tray,
keeps the last N seconds of Discord audio in memory and saves it as a WAV when
you press the hotkey (**Ctrl+Alt+C** by default).

- **Other people:** captured with per-process WASAPI loopback of the Discord
  client, so only Discord audio is recorded (no game/music/browser audio).
  Discord keeps playing to your speakers as normal.
- **You:** your default *communications* microphone is mixed in.
- The saved clip is copied to the clipboard as a file, so **Ctrl+V in a Discord
  chat** attaches it. A notification confirms it; click it to show the file.
- Clips are 16-bit 48 kHz stereo: 30 s is about 5.8 MB, under Discord's 10 MB
  free upload limit.
- Waits for Discord if it isn't running yet, and re-attaches automatically when
  Discord restarts or updates. The tray icon is blurple while capturing Discord,
  gray while waiting.

Requires Windows 10 2004 (build 19041) or newer. Supports Discord, PTB and Canary.

## Window

Left-click the tray icon to open the Discord Clip window: a Discord-styled
window with a sidebar of pages. Closing it just hides it again.

- **Clips** (the page it opens on): every clip in the clips folder as a card
  with its waveform, date, length and size, newest first. The play button
  plays/pauses; click or drag on the waveform to scrub (playhead, time under
  the mouse). **Copy** puts it on the clipboard for Ctrl+V in Discord,
  **Show** opens it in Explorer, **Delete** moves it to the Recycle Bin. New
  clips appear on their own.
- **Settings**: hotkey recorder (combos already taken by another app are
  rejected), clip length, microphone on/off and volume, clips folder,
  clipboard and notification toggles, **Start with Windows**, and buttons to open/reload
  `config.ini` and the log. Changes save immediately. Clip length and the
  microphone switch apply after a restart; a bar offers to do that for you.
- **Updates**: current version, "Check for updates", and **Update now** when
  a newer release exists (with download progress and release notes link),
  plus the automatic-check toggle.

## Updates

With `check_updates = true` (the default) the app checks GitHub 20 seconds
after starting and then every 6 hours. When a newer release is out, a
notification appears (once per version; clicking it opens the Updates page)
and the sidebar's Updates entry gets a badge. Nothing is downloaded or
installed until you click **Update now**:

- **Installed copy** (`%LOCALAPPDATA%\Programs\discord-clip`): downloads the
  release's installer, checks its SHA-256 against `latest.json`, runs it
  silently and quits; the installer replaces the exe and starts it again.
- **Portable exe** (anywhere else): downloads the new portable exe, checks it,
  swaps it in for the running file (the old one is kept as `.old` until the
  next start) and restarts.

The first start after an update opens the window on the Updates page with an
"Updated from X to Y" banner (and a "What's new" link), plus a notification.

## Tray menu

Right-click the tray icon:

- **Open Discord Clip**
- **Save clip now**
- **Open clips folder**
- **Open log**: `%LOCALAPPDATA%\discord-clip\discord-clip.log`
- **Quit**

## Settings

`%APPDATA%\discord-clip\config.ini`, created on first run:

```ini
hotkey = Ctrl+Alt+C          # e.g. Ctrl+Shift+F9, F9, Win+Alt+R, Ctrl+Numpad0
seconds = 30                 # 1-300
mic = true
mic_gain = 1
out_dir = C:\Users\you\Music\Discord Clips   # full path; %USERPROFILE% etc. allowed
clipboard = true
notifications = true
check_updates = true         # look for new versions (never installs without asking)
```

Command-line flags override the file for one run without changing it:
`--hotkey`, `-s/--seconds`, `--no-mic`, `--mic-gain`, `-o/--out`,
`--no-clipboard`, `--no-notifications`, `-h/--help`.

## Releases

GitHub Actions builds and tests every push (`ci.yml`) and attaches the
installer + portable exe to the run as an artifact, versioned
`X.Y.Z-dev.g<commit>`.

Releases are published from `main` when a pull request is merged
(`release.yml`). The PR description decides the version, via its
`Release:` line (the PR template adds one):

| `Release:` | Publishes |
| --- | --- |
| `patch` / `minor` / `major` | The latest release bumped, e.g. 0.3.0 → 0.3.1 / 0.4.0 / 1.0.0 |
| `1.2.3` | Exactly that version. `1.2.3-beta.1` is a pre-release (doesn't count as "latest") |
| `none`, or no line | Nothing |

While the PR is open, its **release version** check shows what the merge will
publish, and fails on an invalid line (unknown word, not newer than the latest
release, tag already exists). After the merge the version is stamped into the
build, the merge commit is tagged `vX.Y.Z`, and the bot comments the release
link on the PR. The `version` in `Cargo.toml` is only used for local and dev
builds; nothing is committed back.

The release gets:

| Asset | What |
| --- | --- |
| `discord-clip-X.Y.Z-setup.exe` | Installer (Inno Setup, `installer/discord-clip.iss`): per-user, no admin, installs to `%LOCALAPPDATA%\Programs\discord-clip`, Start menu entry, optional start with Windows |
| `discord-clip-X.Y.Z-portable.exe` | The standalone exe |
| `latest.json` | Update manifest (below) |
| `SHA256SUMS.txt` | Checksums |

### Update manifest

The newest non-pre-release's manifest is always at
`https://github.com/Nikibudd/discord-clip/releases/latest/download/latest.json`
(a plain download, so no GitHub API rate limits):

```json
{
  "schema": 1,
  "version": "0.3.0",
  "tag": "v0.3.0",
  "published": "2026-11-12T18:00:00Z",
  "notes_url": "https://github.com/Nikibudd/discord-clip/releases/tag/v0.3.0",
  "min_windows": "10.0.19041",
  "installer": { "name": "discord-clip-0.3.0-setup.exe", "url": "...", "size": 4200000, "sha256": "..." },
  "portable":  { "name": "discord-clip-0.3.0-portable.exe", "url": "...", "size": 12700000, "sha256": "..." }
}
```

An in-app updater compares `version` with `CARGO_PKG_VERSION`, downloads
`installer.url`, checks `sha256`, runs it with
`/VERYSILENT /SUPPRESSMSGBOXES /NORESTART` and exits; the installer closes any
running copy, installs over it and starts the new version. The repo must be
public for the app to reach these URLs without a token.

## Building

With the MSVC toolchain this needs the "Desktop development with C++" workload
(Build Tools for Visual Studio). Without it, use the GNU toolchain plus a full
MinGW (the window's dependencies need a working `dlltool`, and the one rustup
ships can't run on its own):

```
scoop install mingw
rustup toolchain install stable-x86_64-pc-windows-gnu --profile minimal
cargo +stable-x86_64-pc-windows-gnu build --release
cargo +stable-x86_64-pc-windows-gnu test
```

Installed copy: `%LOCALAPPDATA%\Programs\discord-clip\discord-clip.exe` (quit
it from the tray before copying a new build over it).

## How it works

| File | Role |
| --- | --- |
| `src/main.rs` | Startup, single-instance check, CLI overrides, Discord supervisor thread |
| `src/tray/` | The tray app (main thread): `mod.rs` app state + message loop, `notify_icon.rs` tray icon/tooltip/notifications, `menu.rs` right-click menu, `global_hotkey.rs` save-clip hotkey, `actions.rs` saving clips + opening folders, `window.rs` snapshot/commands for the window, folder picker, update hand-off |
| `src/ui/` | The window (egui, own thread): `mod.rs` command/snapshot API + window lifecycle, `theme.rs` Discord colors/fonts, `widgets.rs` buttons/toggles/sliders, `sidebar.rs`, `keybind.rs` hotkey recorder, one file per page (`clips.rs`, `settings.rs`, `updates.rs`) |
| `src/icon.rs` | The app icon, drawn at runtime (tray + window) |
| `src/autostart.rs` | "Start with Windows": the `Discord Clip` value under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` (same as the installer), incl. Task Manager's Startup apps switch |
| `src/update.rs` | Update checks and in-app updates: fetches `latest.json` over WinHTTP, downloads + SHA-256-verifies the installer or portable exe, hands over to it |
| `src/config.rs` | `config.ini` load/save/validation |
| `src/hotkey.rs` | Parsing/formatting `Ctrl+Alt+C`-style hotkeys |
| `src/capture.rs` | Finds the root Discord process; process-loopback / mic WASAPI streams in a fixed 48 kHz stereo format, event-driven; pads silence when the stream goes quiet so the buffer stays in step with real time |
| `src/ringbuffer.rs` | Fixed-size circular sample buffer |
| `src/clip.rs` | Mixes tracks (aligned at the hotkey press), writes 16-bit WAV, copies the file to the clipboard |
| `src/library.rs` | The clips folder as a library: listing clips, reading length + waveform, Recycle Bin |
| `src/player.rs` | Clip playback with pause and seeking (`waveOut`) |
| `src/log.rs` | Log file |
