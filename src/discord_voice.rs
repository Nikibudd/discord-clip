//! Who's talking in your Discord voice call (#66), so a saved clip can show
//! who spoke when.
//!
//! Two sources, both without logging in to anything:
//!
//! * **Which channel you're in:** Discord's own log, `logs/renderer_js.log`,
//!   which it writes as things happen. Only its voice connection lines are
//!   used (`[RTCConnection(<server>, default)] Connecting to RTC server …,
//!   channel: <id>`, `Updating channel: <id>`, `Destroy RTCConnection`).
//!   When the log says nothing (e.g. we started while already in a call),
//!   the `SelectedChannelStore` key in Local Storage is the fallback
//!   (`discord_theme::selected_voice_channel`), which can lag a minute.
//! * **Who's speaking:** Discord's local RPC pipe (`\\.\pipe\discord-ipc-N`),
//!   as our own Discord application. Subscribing to `SPEAKING_START` /
//!   `SPEAKING_STOP` for a channel, and `GET_USER` for names, work without
//!   OAuth (tested 2026-11-14; undocumented, so this may stop working, and
//!   then clips simply have no names).
//!
//! Nothing here touches the login token, and nothing read is logged beyond
//! "connected" / "channel changed".

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::windows::io::AsRawHandle;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::log;

/// Discord Clip's Discord application (public; no secret needed).
pub const APP_ID: &str = "1571135045777760296";

/// A speaking change, stamped when it arrived.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub at: Instant,
    pub user: String,
    pub speaking: bool,
}

/// What Discord told us about a user.
#[derive(Clone, Debug, PartialEq)]
pub struct User {
    /// Global (display) name, or the username without one.
    pub name: String,
    pub avatar: Option<String>,
}

#[derive(Default)]
struct State {
    events: VecDeque<Event>,
    users: HashMap<String, User>,
    /// You, from the pipe's READY message.
    own_id: Option<String>,
    /// Users the window wants fresh names for (#67), asked once per connection.
    wanted: Vec<String>,
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(Default::default)
}

/// Events from `from` on (and, per user, the last one before it, so a
/// clip knows who was already talking when it starts), plus your own id.
pub fn events_since(from: Instant) -> (Vec<Event>, Option<String>) {
    let s = state().lock().unwrap();
    let mut before: HashMap<&str, &Event> = HashMap::new();
    let mut out = Vec::new();
    for e in &s.events {
        if e.at < from {
            before.insert(&e.user, e);
        } else {
            out.push(e.clone());
        }
    }
    let mut earlier: Vec<Event> = before.into_values().cloned().collect();
    earlier.sort_by_key(|e| e.at);
    earlier.extend(out);
    (earlier, s.own_id.clone())
}

/// The users Discord has told us about this session.
pub fn users() -> HashMap<String, User> {
    state().lock().unwrap().users.clone()
}

/// Your own Discord user id, once connected.
pub fn own_id() -> Option<String> {
    state().lock().unwrap().own_id.clone()
}

/// Asks Discord for the current names of `ids` (e.g. people in a clip that's
/// being shown), so a rename shows up; `users()` has them once answered.
pub fn refresh_users(ids: impl IntoIterator<Item = String>) {
    state().lock().unwrap().wanted.extend(ids);
}

/// Starts listening on a background thread, keeping `keep` worth of events
/// (the clip length plus some slack).
pub fn spawn(running: Arc<AtomicBool>, keep: Duration) {
    let spawned = std::thread::Builder::new().name("discord-voice".into()).spawn(move || run(&running, keep));
    if let Err(e) = spawned {
        log!("discord voice: couldn't start: {e}");
    }
}

fn run(running: &AtomicBool, keep: Duration) {
    let mut channel = ChannelTracker::default();
    let mut rpc: Option<Rpc> = None;
    let mut subscribed: Option<String> = None;
    let mut last_connect: Option<Instant> = None;
    while running.load(Ordering::SeqCst) {
        let current = channel.poll();
        if rpc.is_none() && last_connect.is_none_or(|t| t.elapsed() > Duration::from_secs(10)) {
            last_connect = Some(Instant::now());
            match Rpc::connect() {
                Ok(r) => {
                    log!("discord voice: connected to Discord");
                    rpc = Some(r);
                    subscribed = None;
                }
                Err(_) => {} // Discord not running: try again later, quietly
            }
        }
        if let Some(r) = &mut rpc {
            let result = (|| -> std::io::Result<()> {
                while let Some(msg) = r.poll()? {
                    handle(r, &msg)?;
                }
                // Discord ignores commands sent before its READY.
                if r.ready && subscribed != current {
                    if let Some(old) = &subscribed {
                        r.speaking("UNSUBSCRIBE", old)?;
                    }
                    if let Some(new) = &current {
                        r.speaking("SUBSCRIBE", new)?;
                    }
                    subscribed = current.clone();
                }
                if r.ready {
                    let wanted = std::mem::take(&mut state().lock().unwrap().wanted);
                    for id in wanted {
                        if r.asked.insert(id.clone()) {
                            r.send(json!({ "cmd": "GET_USER", "args": { "id": id }, "nonce": format!("user-{id}") }))?;
                        }
                    }
                }
                Ok(())
            })();
            if result.is_err() {
                log!("discord voice: lost the connection to Discord");
                rpc = None;
            }
        }
        prune(keep);
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn handle(rpc: &mut Rpc, msg: &Value) -> std::io::Result<()> {
    let data = &msg["data"];
    match (msg["cmd"].as_str(), msg["evt"].as_str()) {
        (Some("DISPATCH"), Some("READY")) => {
            rpc.ready = true;
            let mut s = state().lock().unwrap();
            s.own_id = data["user"]["id"].as_str().map(str::to_string);
            if let (Some(id), Some(user)) = (s.own_id.clone(), parse_user(&data["user"])) {
                s.users.insert(id, user);
            }
        }
        (Some("DISPATCH"), Some(evt @ ("SPEAKING_START" | "SPEAKING_STOP"))) => {
            let Some(id) = data["user_id"].as_str() else { return Ok(()) };
            let unknown = {
                let mut s = state().lock().unwrap();
                s.events.push_back(Event { at: Instant::now(), user: id.to_string(), speaking: evt == "SPEAKING_START" });
                !s.users.contains_key(id)
            };
            if unknown && rpc.asked.insert(id.to_string()) {
                rpc.send(json!({ "cmd": "GET_USER", "args": { "id": id }, "nonce": format!("user-{id}") }))?;
            }
        }
        (Some("GET_USER"), _) => {
            if let (Some(id), Some(user)) = (data["id"].as_str(), parse_user(data)) {
                state().lock().unwrap().users.insert(id.to_string(), user);
            }
        }
        (_, Some("ERROR")) => {
            log!("discord voice: Discord refused {}: {}", msg["cmd"], data["message"]);
        }
        (Some("SUBSCRIBE"), _) if data["evt"] == "SPEAKING_START" => log!("discord voice: listening for who's talking"),
        _ => {}
    }
    Ok(())
}

fn parse_user(v: &Value) -> Option<User> {
    let name = v["global_name"].as_str().filter(|s| !s.is_empty()).or(v["username"].as_str())?;
    Some(User { name: name.to_string(), avatar: v["avatar"].as_str().map(str::to_string) })
}

fn prune(keep: Duration) {
    let mut s = state().lock().unwrap();
    while s.events.front().is_some_and(|e| e.at.elapsed() > keep) {
        s.events.pop_front();
    }
}

// ---------------------------------------------------------------------------
// The RPC pipe
// ---------------------------------------------------------------------------

struct Rpc {
    pipe: File,
    nonce: u64,
    /// Users we already asked for (once per connection).
    asked: HashSet<String>,
    /// Discord sent READY: commands are accepted from now on.
    ready: bool,
}

impl Rpc {
    fn connect() -> std::io::Result<Rpc> {
        let mut last = std::io::Error::other("no Discord pipe");
        for i in 0..10 {
            match std::fs::OpenOptions::new().read(true).write(true).open(format!(r"\\.\pipe\discord-ipc-{i}")) {
                Ok(pipe) => {
                    let mut rpc = Rpc { pipe, nonce: 0, asked: HashSet::new(), ready: false };
                    rpc.frame(0, &json!({ "v": 1, "client_id": APP_ID }))?;
                    return Ok(rpc);
                }
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    fn frame(&mut self, op: u32, body: &Value) -> std::io::Result<()> {
        let body = body.to_string();
        let mut buf = Vec::with_capacity(8 + body.len());
        buf.extend_from_slice(&op.to_le_bytes());
        buf.extend_from_slice(&(body.len() as u32).to_le_bytes());
        buf.extend_from_slice(body.as_bytes());
        self.pipe.write_all(&buf)
    }

    fn send(&mut self, mut body: Value) -> std::io::Result<()> {
        if body.get("nonce").is_none() {
            self.nonce += 1;
            body["nonce"] = json!(format!("n{}", self.nonce));
        }
        self.frame(1, &body)
    }

    fn speaking(&mut self, cmd: &str, channel: &str) -> std::io::Result<()> {
        for evt in ["SPEAKING_START", "SPEAKING_STOP"] {
            self.send(json!({ "cmd": cmd, "evt": evt, "args": { "channel_id": channel } }))?;
        }
        Ok(())
    }

    /// The next message if one has arrived, without blocking.
    fn poll(&mut self) -> std::io::Result<Option<Value>> {
        if self.available()? < 8 {
            return Ok(None);
        }
        let mut head = [0u8; 8];
        self.pipe.read_exact(&mut head)?;
        let op = u32::from_le_bytes([head[0], head[1], head[2], head[3]]);
        let len = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as usize;
        let mut body = vec![0u8; len];
        self.pipe.read_exact(&mut body)?;
        if op == 2 {
            return Err(std::io::Error::other("Discord closed the connection"));
        }
        Ok(serde_json::from_slice(&body).ok())
    }

    /// Bytes waiting in the pipe (reading would otherwise block, and a
    /// blocked read would hold up writes on the same handle).
    fn available(&self) -> std::io::Result<u32> {
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::Pipes::PeekNamedPipe;
        let mut avail = 0u32;
        unsafe { PeekNamedPipe(HANDLE(self.pipe.as_raw_handle()), None, 0, None, Some(&mut avail), None) }
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        Ok(avail)
    }
}

// ---------------------------------------------------------------------------
// Which voice channel you're in
// ---------------------------------------------------------------------------

/// What a log line says about the voice channel.
#[derive(Clone, Debug, PartialEq)]
enum LogVoice {
    Joined(String),
    Left,
}

/// Reads one line of `renderer_js.log`; only the voice connection lines of
/// the `default` context count (`stream` is Go Live).
fn parse_log_line(line: &str) -> Option<LogVoice> {
    if !line.contains("[RTCConnection(") || !line.contains(", default)]") {
        return None;
    }
    let digits = |s: &str| -> Option<String> {
        let id: String = s.chars().take_while(char::is_ascii_digit).collect();
        (!id.is_empty()).then_some(id)
    };
    if let Some(i) = line.find("Updating channel: ") {
        return digits(&line[i + "Updating channel: ".len()..]).map(LogVoice::Joined);
    }
    if line.contains("Connecting to RTC server") {
        let i = line.rfind("channel: ")?;
        return digits(&line[i + "channel: ".len()..]).map(LogVoice::Joined);
    }
    if line.contains("Destroy RTCConnection") {
        return Some(LogVoice::Left);
    }
    None
}

#[derive(Default)]
struct ChannelTracker {
    log: Option<PathBuf>,
    /// How far the log was read.
    offset: u64,
    /// What the log said last, if anything since we started reading it.
    from_log: Option<Option<String>>,
    /// The Local Storage fallback, and when it was last read.
    from_storage: Option<String>,
    storage_read: Option<Instant>,
    last_poll: Option<Instant>,
    current: Option<String>,
}

impl ChannelTracker {
    /// The channel you're in now, checked a few times a second.
    fn poll(&mut self) -> Option<String> {
        if self.last_poll.is_some_and(|t| t.elapsed() < Duration::from_millis(250)) {
            return self.current.clone();
        }
        self.last_poll = Some(Instant::now());
        self.read_log();
        if self.from_log.is_none() && self.storage_read.is_none_or(|t| t.elapsed() > Duration::from_secs(20)) {
            self.storage_read = Some(Instant::now());
            self.from_storage = crate::discord_theme::selected_voice_channel().flatten();
        }
        let now = self.from_log.clone().unwrap_or_else(|| self.from_storage.clone());
        if now != self.current {
            log!("discord voice: {}", if now.is_some() { "in a voice channel" } else { "not in a voice channel" });
            self.current = now;
        }
        self.current.clone()
    }

    fn read_log(&mut self) {
        let path = match &self.log {
            Some(p) => p.clone(),
            None => {
                let Some(dir) = crate::discord_theme::discord_dir() else { return };
                let p = dir.join("logs").join("renderer_js.log");
                // Start near the end: what's before only matters for the
                // last voice line, which is in the last few MB.
                let len = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                self.offset = len.saturating_sub(4 << 20);
                self.log = Some(p.clone());
                p
            }
        };
        let Ok(mut f) = File::open(&path) else { return };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len < self.offset {
            self.offset = 0; // rotated: a new, shorter file
        }
        if len == self.offset || f.seek(SeekFrom::Start(self.offset)).is_err() {
            return;
        }
        let mut bytes = Vec::with_capacity((len - self.offset) as usize);
        if f.take(len - self.offset).read_to_end(&mut bytes).is_err() {
            return;
        }
        // Only whole lines; a half-written last line is read next time.
        let Some(end) = bytes.iter().rposition(|&b| b == b'\n') else { return };
        self.offset += end as u64 + 1;
        for line in String::from_utf8_lossy(&bytes[..end]).lines() {
            match parse_log_line(line) {
                Some(LogVoice::Joined(id)) => self.from_log = Some(Some(id)),
                Some(LogVoice::Left) => self.from_log = Some(None),
                None => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_voice_lines_from_discords_log() {
        let join = "[2026-11-08 20:16:52.369] [info]  [RTCConnection(745220923686256690, default)] Connecting to RTC server wss://c-x.discord.media:8443/, rtc-connection-id: a8, channel: 1040151705263800371";
        assert_eq!(parse_log_line(join), Some(LogVoice::Joined("1040151705263800371".into())));
        let update = "[2026-11-14 13:25:39.881] [info]  [RTCConnection(745220923686256690, default)] Updating channel: 1040151705263800371(2)";
        assert_eq!(parse_log_line(update), Some(LogVoice::Joined("1040151705263800371".into())));
        let left = "[2026-11-14 11:19:24.137] [info]  [RTCConnection(1123304457900396585, default)] Destroy RTCConnection";
        assert_eq!(parse_log_line(left), Some(LogVoice::Left));
        // Go Live and everything else is ignored.
        let stream = "[2026-11-14 12:59:49.285] [info]  [RTCConnection(1571132530806226995, stream)] Connecting to RTC server wss://x/, rtc-connection-id: a8, channel: 104015";
        assert_eq!(parse_log_line(stream), None);
        assert_eq!(parse_log_line("[info] [Routing/Utils] transitionTo - /channels/1/2"), None);
    }

    #[test]
    fn events_since_keeps_the_state_at_the_start() {
        let t0 = Instant::now();
        {
            let mut s = state().lock().unwrap();
            s.events.clear();
            for (ms, user, speaking) in [(0, "a", true), (100, "b", true), (200, "a", false), (500, "b", false), (600, "a", true)] {
                s.events.push_back(Event { at: t0 + Duration::from_millis(ms), user: user.into(), speaking });
            }
        }
        let (events, _) = events_since(t0 + Duration::from_millis(300));
        let got: Vec<(&str, bool)> = events.iter().map(|e| (e.user.as_str(), e.speaking)).collect();
        // b was still talking at 300 ms (its start comes first), a had stopped.
        assert_eq!(got, [("b", true), ("a", false), ("b", false), ("a", true)]);
        state().lock().unwrap().events.clear();
    }
}
