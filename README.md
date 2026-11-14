# discord-clip

Instant-replay for Discord voice chat on Windows. Sits in the system tray,
keeps the last N seconds of Discord audio in memory and saves it as a clip
when you press the hotkey (**Ctrl+Alt+C** by default).

- **Other people:** captured with per-process WASAPI loopback of the Discord
  client, so only Discord audio is recorded (no game/music/browser audio).
  Discord keeps playing to your speakers as normal.
- **You:** your microphone is mixed in: by default Windows' default *communications* mic, or the one you pick in Settings.
- **Separate tracks** (on by default): each clip keeps Discord and your mic as
  separate tracks in one 4-channel file, so you can change their volumes later.
  Players that open it directly mix it to stereo, and copying a clip still
  gives Discord a normal stereo mix. About twice the file size; not possible
  with MP3.
- **Format:** Opus (`.ogg`, the default: a 30 s clip is well under 1 MB),
  MP3 128 or 192 kbps, FLAC (lossless) or WAV (uncompressed).
- The saved clip is copied to the clipboard as a file, so **Ctrl+V in a Discord
  chat** attaches it. A notification confirms it; click it to open the clip
  in the app.
- Waits for Discord if it isn't running yet, and re-attaches automatically when
  Discord restarts or updates. The tray icon is blurple while capturing Discord,
  gray while waiting. Hovering it shows the hotkey, the Discord status and your
  clips ("12 clips · 3 not uploaded · last saved 2 min ago").

Requires Windows 10 2004 (build 19041) or newer. Supports Discord, PTB and Canary.

## Window

Left-click the tray icon to open the Discord Clip window: a Discord-styled
window with a sidebar of pages. Closing it just hides it again. It follows
your Discord theme (see [Theme](#theme)).

### Clips

The page it opens on. Every clip in the clips folder is a card with its
waveform, date, length and size, newest first, in as many columns as the
window fits. New clips appear on their own.

- **Play:** the play button plays/pauses; click or drag on the waveform to
  scrub (playhead, time under the mouse).
- **Make Discord-ready** (the card's main button) turns the clip into a
  soundboard sound and replaces it, after a confirm: the tracks are mixed into
  one, saved as an MP3 of at most 5 s, and the original goes to the Recycle
  Bin. A clip that's longer opens in **Edit** with its last 5 s selected
  (where the moment usually is), to pick the part first. Once a clip is
  Discord-ready the button becomes **Show in Explorer**, to pick the file
  when you upload the sound in Discord.
- **Rename:** click a title (or **⋯ → Rename**). The file keeps its
  timestamp: `discord_clip_20261109_213151 Bob laughs.ogg`.
- **Search** by name or date, and **filter** by uploaded mark: **Not
  uploaded** (the default, so clips you're done with are out of the way),
  **Uploaded** or **All clips**.
- **⋯ menu:**
  - **Copy** puts the clip on the clipboard for Ctrl+V in Discord. A clip with
    separate tracks also offers **Copy Discord only** and **Copy mic only**.
  - **Edit…** opens the Edit dialog (below).
  - **Rename**.
  - **Mark as uploaded** / **Mark as not uploaded**: for clips you've added to
    a soundboard. The mark doesn't change the file and follows the clip when
    it's renamed or made Discord-ready.
  - **Show in Explorer**.
  - **Delete** moves the clip to the Recycle Bin, after a confirm.
- **Keyboard:** click a card to select it, then:

  | Key | |
  | --- | --- |
  | Arrow keys | Move the selection (up/down by a row) |
  | Space | Play / pause |
  | F2 | Rename |
  | Enter | Make Discord-ready |
  | Ctrl+C | Copy |
  | Delete | Delete (asks first) |
  | Esc | Clear the selection |

  In the confirm dialogs, Enter confirms and Esc cancels.

### Edit dialog

**⋯ → Edit…** shows a waveform per track (Discord and your mic) with a volume
slider each, and trim handles to keep just part of the clip. Drag or click to
scrub and **Play selection** (or Space) to listen; the length and size of the
result are shown as you go. **Save** changes the clip (a trimmed original goes
to the Recycle Bin), **Save as new clip** keeps the original, **Cancel** (or
Esc) throws the changes away. Volumes are stored next to the clip rather than
in the file, so they can be changed again later.

### Settings

- **Hotkey:** a recorder; combos already taken by another app are rejected.
- **Recording:** clip length, format, microphone on/off, which mic (falls back
  to the default if it's unplugged), separate tracks, and the mic's volume.
- **Clips folder:** open or change it; copy clips to the clipboard; show
  notifications.
- **Startup:** **Start with Windows**.
- **Appearance:** theme.
- **Advanced:** open or reload `config.ini`, open the log.

Changes save immediately. Clip length and the microphone switch apply after a
restart; a bar offers to do that for you.

### Updates

Current version, "Check for updates", and **Update now** when a newer release
exists (with download progress and release notes link), plus the
automatic-check toggle.

## Theme

**Settings → Appearance → Theme**: **Follow Discord** (the default), **Dark**
or **Light**.

Follow Discord uses the theme picked in Discord: Light, Ash, Dark or Onyx,
with Discord's Saturation setting, and Nitro gradients and custom themes as a
real gradient behind the sidebar and pages. The base theme switches within
about a second; a gradient or custom theme can take up to a minute, because
Discord only saves it to disk once a minute. If Discord's theme can't be
read, the last theme read is used (Dark if there never was one).

**What it reads:** `%APPDATA%\discord\settings.json` and a few exact keys in
Discord's Local Storage, read-only, only while the window is open. The same
Local Storage holds your Discord login token: it is never read, and nothing
read is logged or copied anywhere. Only the theme is kept, in
`discord-theme.txt` (below).

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

- The Discord and microphone status (not clickable)
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
mic_gain = 1                 # 0-10
mic_device =                 # empty = default communications mic; set from the app
format = opus                # opus, mp3-128, mp3-192, flac or wav
separate_tracks = true       # Discord + mic as separate tracks (4-channel clips)
out_dir = C:\Users\you\Music\Discord Clips   # full path; %USERPROFILE% etc. allowed
clipboard = true
notifications = true
check_updates = true         # look for new versions (never installs without asking)
theme = discord              # discord (follow Discord), dark or light
```

Command-line flags override the file for one run without changing it:
`--hotkey`, `-s/--seconds`, `--no-mic`, `--mic-gain`, `--format`, `-o/--out`,
`--no-clipboard`, `--no-notifications`, `--settings` (open the window right
away), `-h/--help`.

The same folder also holds what the app remembers about clips and the theme.
Each is a small text file, rewritten as things change:

| File | What |
| --- | --- |
| `clip-volumes.txt` | Track volumes set in the Edit dialog, per clip |
| `clip-uploaded.txt` | Clips marked as uploaded |
| `discord-theme.txt` | The last Discord theme read, used until the next read works |

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
| `src/tray/` | The tray app (main thread): `mod.rs` app state + message loop, `notify_icon.rs` tray icon/tooltip (incl. the clips line)/notifications, `menu.rs` right-click menu, `global_hotkey.rs` save-clip hotkey, `actions.rs` saving clips, opening folders, opening a saved clip in the window, `window.rs` snapshot/commands for the window, folder picker, update hand-off |
| `src/ui/` | The window (egui, own thread): `mod.rs` command/snapshot API + window lifecycle, `theme.rs` Discord colours/fonts, the palette that follows Discord and the gradient backdrop, `follow.rs` watching and reading Discord's theme, `widgets.rs` buttons/toggles/sliders/dropdowns, `sidebar.rs` page list + status card, `keybind.rs` hotkey recorder, `edit.rs` Edit dialog, `confirm.rs` confirm dialog, one file per page (`clips.rs`, `settings.rs`, `updates.rs`) |
| `src/icon.rs` | The app icon, drawn in code: at runtime for the tray and window, and at build time for the exe |
| `build.rs` | Embeds the icon and version info in the exe (`winresource`), so the taskbar, Start menu and Explorer show it |
| `src/autostart.rs` | "Start with Windows": the `Discord Clip` value under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` (same as the installer), incl. Task Manager's Startup apps switch |
| `src/update.rs` | Update checks and in-app updates: fetches `latest.json` over WinHTTP, downloads + SHA-256-verifies the installer or portable exe, hands over to it |
| `src/config.rs` | `config.ini` load/save/validation |
| `src/hotkey.rs` | Parsing/formatting `Ctrl+Alt+C`-style hotkeys |
| `src/capture.rs` | Finds the root Discord process; process-loopback / mic WASAPI streams in a fixed 48 kHz stereo format, event-driven; pads silence when the stream goes quiet so the buffer stays in step with real time |
| `src/ringbuffer.rs` | Fixed-size circular sample buffer |
| `src/clip.rs` | Builds a clip from the buffers (tracks aligned at the hotkey press), saves it, copies the file to the clipboard |
| `src/audio.rs` | Reading and writing the clip formats: Opus/Ogg, MP3 (Media Foundation), FLAC, WAV |
| `src/library.rs` | The clips folder as a library: listing clips, reading length + waveform, trimming, Discord-ready MP3s, the tray tooltip's summary, Recycle Bin |
| `src/volumes.rs` | Per-clip track volumes (`clip-volumes.txt`) |
| `src/uploaded.rs` | Uploaded marks (`clip-uploaded.txt`) |
| `src/discord_theme.rs` | Reads Discord's theme from `settings.json` and exact Local Storage keys (never anything else), remembers it in `discord-theme.txt` |
| `src/leveldb.rs` | A minimal read-only LevelDB reader for Discord's Local Storage |
| `src/player.rs` | Clip playback with pause and seeking (`waveOut`) |
| `src/log.rs` | Log file |
