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

## Tray menu

Right- or left-click the tray icon:

- **Save clip now**
- **Open clips folder**
- **Change hotkey...**: press the new combination in the picker. Combos
  already taken by another app are rejected.
- **Change clips folder...**: standard folder picker; saved permanently
- **Edit settings file...** / **Reload settings**
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
(Build Tools for Visual Studio). Without it, the GNU toolchain works out of the
box:

```
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
| `src/tray.rs` | Tray icon (drawn at runtime), menu, notifications, hotkey dialog, folder picker |
| `src/config.rs` | `config.ini` load/save/validation |
| `src/hotkey.rs` | Parsing/formatting `Ctrl+Alt+C`-style hotkeys |
| `src/capture.rs` | Finds the root Discord process; process-loopback / mic WASAPI streams in a fixed 48 kHz stereo format, event-driven; pads silence when the stream goes quiet so the buffer stays in step with real time |
| `src/ringbuffer.rs` | Fixed-size circular sample buffer |
| `src/clip.rs` | Mixes tracks (aligned at the hotkey press), writes 16-bit WAV, copies the file to the clipboard |
| `src/log.rs` | Log file |
