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

- **Settings**: hotkey recorder (combos already taken by another app are
  rejected), clip length, microphone on/off and volume, clips folder,
  clipboard and notification toggles, and buttons to open/reload
  `config.ini` and the log. Changes save immediately. Clip length and the
  microphone switch apply after a restart; a bar offers to do that for you.

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
```

Command-line flags override the file for one run without changing it:
`--hotkey`, `-s/--seconds`, `--no-mic`, `--mic-gain`, `-o/--out`,
`--no-clipboard`, `--no-notifications`, `-h/--help`.

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
| `src/tray.rs` | Tray icon (drawn at runtime), menu, notifications, global hotkey, folder picker; applies changes requested by the window |
| `src/ui.rs` | The window (egui, on its own thread): Discord-styled sidebar + Settings page; sends commands to the tray thread and draws the snapshot it publishes back |
| `src/config.rs` | `config.ini` load/save/validation |
| `src/hotkey.rs` | Parsing/formatting `Ctrl+Alt+C`-style hotkeys |
| `src/capture.rs` | Finds the root Discord process; process-loopback / mic WASAPI streams in a fixed 48 kHz stereo format, event-driven; pads silence when the stream goes quiet so the buffer stays in step with real time |
| `src/ringbuffer.rs` | Fixed-size circular sample buffer |
| `src/clip.rs` | Mixes tracks (aligned at the hotkey press), writes 16-bit WAV, copies the file to the clipboard |
| `src/log.rs` | Log file |
