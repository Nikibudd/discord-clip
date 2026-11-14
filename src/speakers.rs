//! Who talked when in each clip (#66).
//!
//! When a clip is saved, its speech is found in the audio (a simple
//! voice-activity detector on the Discord and mic tracks), and Discord's
//! speaking events from `discord_voice` say who it was. Each person's
//! stretches of talking are stored per clip, by Discord **user id** (names
//! change, ids don't), in `%APPDATA%\discord-clip\clip-speakers.json`. Names
//! and avatars live once per user in `discord-users.json`.
//!
//! Like the volumes and uploaded marks, the data follows the clip when it's
//! renamed, trimmed or made Discord-ready, and entries of clips that are
//! gone are dropped whenever the file is written.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::discord_voice::Event;

/// `Segment::who` for your own speech (from the mic track).
pub const YOU: &str = "you";

/// One stretch of someone talking, in seconds from the clip's start.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub start: f32,
    pub end: f32,
    /// A Discord user id, `YOU`, or none for speech without a name (no
    /// speaking event matched it: game audio, a bot, or no Discord data).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub who: Option<String>,
}

/// Everything stored about one clip.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ClipSpeakers {
    /// The clip's length, so segments can be cut when it's trimmed.
    pub seconds: f32,
    /// Per person, so two people talking at once both have a segment.
    pub segments: Vec<Segment>,
}

/// What we last knew about a Discord user.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KnownUser {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
}

/// Clips by `key()` of their path.
pub type Table = BTreeMap<String, ClipSpeakers>;
/// Users by id.
pub type Users = BTreeMap<String, KnownUser>;

pub fn store_path() -> PathBuf {
    crate::config::app_dir().join("clip-speakers.json")
}

pub fn users_path() -> PathBuf {
    crate::config::app_dir().join("discord-users.json")
}

/// Windows paths are case-insensitive.
pub fn key(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

pub fn load() -> Table {
    read_json(&store_path())
}


pub fn load_users() -> Users {
    read_json(&users_path())
}

pub fn lookup<'a>(table: &'a Table, path: &Path) -> Option<&'a ClipSpeakers> {
    table.get(&key(path))
}

// ---------------------------------------------------------------------------
// Recording a clip's speakers
// ---------------------------------------------------------------------------

/// Works out who talked when in a clip that was just saved, from its
/// tracks (stereo, `rate` Hz, aligned at their end) and the speaking events
/// up to `end` (when the buffers were read), and stores it.
pub fn record(path: &Path, discord: &[f32], mic: &[f32], rate: u32, end: Instant) -> Result<()> {
    let frames = discord.len().max(mic.len()) / 2;
    let seconds = frames as f32 / rate as f32;
    // Each track ends at the clip's end; a shorter one starts later.
    let offset = |track: &[f32]| seconds - (track.len() / 2) as f32 / rate as f32;
    let shift = |spans: Vec<(f32, f32)>, by: f32| spans.into_iter().map(|(a, b)| (a + by, b + by)).collect::<Vec<_>>();
    let discord_speech = shift(speech(discord, rate), offset(discord));
    let mic_speech = shift(speech(mic, rate), offset(mic));

    let from = end.checked_sub(std::time::Duration::from_secs_f32(seconds)).unwrap_or(end);
    let (events, own_id) = crate::discord_voice::events_since(from);
    let spans = speaking_spans(&events, own_id.as_deref(), end, seconds);
    let segments = assign(&discord_speech, &mic_speech, &spans);

    let data = ClipSpeakers { seconds, segments };
    let ids: Vec<String> = data.segments.iter().filter_map(|s| s.who.clone()).filter(|w| w != YOU).collect();
    modify(&store_path(), |t| {
        t.insert(key(path), data);
    })?;
    remember_users(&ids)
}

/// Adds or refreshes the names of `ids` from what Discord told us (on save,
/// and when the Edit dialog showed them with fresh names, #67).
pub fn remember_users(ids: &[String]) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let known = crate::discord_voice::users();
    let path = users_path();
    let mut users: Users = read_json(&path);
    let mut changed = false;
    for id in ids {
        if let Some(u) = known.get(id) {
            let new = KnownUser { name: u.name.clone(), avatar: u.avatar.clone() };
            if users.get(id) != Some(&new) {
                users.insert(id.clone(), new);
                changed = true;
            }
        }
    }
    if changed {
        write_json(&path, &users)?;
    }
    Ok(())
}

/// Each other person's speaking spans in clip seconds, from the events
/// (stamped when they arrived) of a clip of `seconds` ending at `end`.
/// Your own events are left out: the mic track says exactly when you talk.
fn speaking_spans(events: &[Event], own_id: Option<&str>, end: Instant, seconds: f32) -> Vec<(String, f32, f32)> {
    let at = |e: &Event| -> f32 {
        match end.checked_duration_since(e.at) {
            Some(before_end) => seconds - before_end.as_secs_f32(),
            None => seconds + e.at.duration_since(end).as_secs_f32(),
        }
    };
    let mut open: BTreeMap<&str, f32> = BTreeMap::new();
    let mut spans = Vec::new();
    for e in events {
        if Some(e.user.as_str()) == own_id {
            continue;
        }
        let t = at(e).clamp(0.0, seconds);
        match (e.speaking, open.get(e.user.as_str()).copied()) {
            (true, None) => {
                open.insert(&e.user, t);
            }
            (false, Some(start)) => {
                open.remove(e.user.as_str());
                spans.push((e.user.clone(), start, t));
            }
            // A stop without a start in view: talking since the clip began.
            (false, None) => spans.push((e.user.clone(), 0.0, t)),
            (true, Some(_)) => {}
        }
    }
    // Still talking when the clip ended.
    spans.extend(open.into_iter().map(|(user, start)| (user.to_string(), start, seconds)));
    spans.retain(|(_, a, b)| b - a > 0.02);
    spans
}

/// How far a speaking event's edge may be moved to the speech found in
/// the audio: events arrive a little late and Discord's indicator holds on.
const SNAP: f32 = 0.4;

/// The clip's segments: your speech from the mic, each other person's
/// spans with their edges snapped to the Discord speech found in the
/// audio, and speech nobody's span covers as unnamed.
fn assign(discord_speech: &[(f32, f32)], mic_speech: &[(f32, f32)], spans: &[(String, f32, f32)]) -> Vec<Segment> {
    let round = |t: f32| (t * 100.0).round() / 100.0;
    let mut out: Vec<Segment> =
        mic_speech.iter().map(|&(a, b)| Segment { start: round(a), end: round(b), who: Some(YOU.into()) }).collect();

    let nearest = |t: f32, edges: &mut dyn Iterator<Item = f32>| -> f32 {
        edges.filter(|e| (e - t).abs() <= SNAP).min_by(|a, b| (a - t).abs().total_cmp(&(b - t).abs())).unwrap_or(t)
    };
    let mut named: Vec<(f32, f32)> = Vec::new();
    for (user, a, b) in spans {
        let start = nearest(*a, &mut discord_speech.iter().map(|s| s.0));
        let end = nearest(*b, &mut discord_speech.iter().map(|s| s.1)).max(start);
        if end - start < 0.02 {
            continue;
        }
        named.push((start, end));
        out.push(Segment { start: round(start), end: round(end), who: Some(user.clone()) });
    }
    for &(a, b) in discord_speech {
        let covered = named.iter().any(|&(s, e)| s < b && e > a);
        if !covered {
            out.push(Segment { start: round(a), end: round(b), who: None });
        }
    }
    out.sort_by(|x, y| x.start.total_cmp(&y.start));
    out
}

/// Gaps shorter than this between one person's segments are joined: a new
/// sentence after a short pause is still the same stretch of talking (#72).
pub const MERGE_GAP: f32 = 0.8;
/// Segments shorter than this (after joining) are left out: breaths,
/// clicks, flickers of Discord's speaking indicator (#72).
pub const MIN_SEGMENT: f32 = 0.25;

/// `segments` cleaned up for showing (#72): per person, join pieces closer
/// than `merge_gap`, then drop what's shorter than `min_len`. Different
/// people are never joined, so overlaps between them stay. The stored data
/// keeps every detail; this is only applied when it's used.
pub fn smoothed(segments: &[Segment], merge_gap: f32, min_len: f32) -> Vec<Segment> {
    let mut by_who: BTreeMap<Option<&str>, Vec<&Segment>> = BTreeMap::new();
    for s in segments {
        by_who.entry(s.who.as_deref()).or_default().push(s);
    }
    let mut out = Vec::new();
    for (_, mut list) in by_who {
        list.sort_by(|a, b| a.start.total_cmp(&b.start));
        let mut joined: Vec<Segment> = Vec::new();
        for s in list {
            match joined.last_mut() {
                Some(last) if s.start - last.end < merge_gap => last.end = last.end.max(s.end),
                _ => joined.push(s.clone()),
            }
        }
        out.extend(joined.into_iter().filter(|s| s.end - s.start >= min_len));
    }
    out.sort_by(|a, b| a.start.total_cmp(&b.start));
    out
}

// ---------------------------------------------------------------------------
// Finding speech in audio
// ---------------------------------------------------------------------------

/// Stretches of sound in interleaved stereo `samples`, in seconds: 20 ms
/// frames louder than the noise floor by a margin, held on briefly so words
/// in a sentence stay together. Discord gates and noise-suppresses voices,
/// so its track is near silence between them.
pub fn speech(samples: &[f32], rate: u32) -> Vec<(f32, f32)> {
    const FRAME_SECS: f32 = 0.02;
    const HOLD_FRAMES: usize = 15; // 300 ms
    const MIN_GAP: f32 = 0.25;
    const MIN_LEN: f32 = 0.12;
    let frame = ((rate as f32 * FRAME_SECS) as usize).max(1) * 2;
    let db: Vec<f32> = samples
        .chunks(frame)
        .map(|c| {
            let rms = (c.iter().map(|s| s * s).sum::<f32>() / c.len() as f32).sqrt();
            20.0 * rms.max(1e-6).log10()
        })
        .collect();
    if db.is_empty() {
        return Vec::new();
    }
    // The quietest tenth is the noise floor; never count very quiet sound.
    let mut sorted = db.clone();
    sorted.sort_by(f32::total_cmp);
    let floor = sorted[sorted.len() / 10];
    let threshold = (floor + 12.0).max(-50.0);

    // A stretch ends once it has been quiet for longer than the hold.
    let mut spans: Vec<(f32, f32)> = Vec::new();
    let mut start: Option<usize> = None;
    let mut last_loud = 0;
    for (i, &d) in db.iter().enumerate() {
        if d > threshold {
            last_loud = i;
            start.get_or_insert(i);
        } else if let Some(s) = start {
            if i - last_loud > HOLD_FRAMES {
                spans.push((s as f32 * FRAME_SECS, (last_loud + 1) as f32 * FRAME_SECS));
                start = None;
            }
        }
    }
    if let Some(s) = start {
        spans.push((s as f32 * FRAME_SECS, (last_loud + 1) as f32 * FRAME_SECS));
    }
    let length = samples.len() as f32 / 2.0 / rate as f32;
    let mut merged: Vec<(f32, f32)> = Vec::new();
    for (a, b) in spans {
        let b = b.min(length).max(a);
        match merged.last_mut() {
            Some(last) if a - last.1 < MIN_GAP => last.1 = b,
            _ => merged.push((a, b)),
        }
    }
    merged.retain(|(a, b)| b - a >= MIN_LEN);
    merged
}

// ---------------------------------------------------------------------------
// Following the clip
// ---------------------------------------------------------------------------

/// The clip was renamed, or replaced by a file with a new name (Discord-ready).
pub fn rename(from: &Path, to: &Path) -> Result<()> {
    if lookup(&load(), from).is_none() {
        return Ok(());
    }
    modify(&store_path(), |t| {
        if let Some(v) = t.remove(&key(from)) {
            t.insert(key(to), v);
        }
    })
}

/// The part `start..end` (0–1) of clip `from` was kept as `to` (the same
/// path when it replaced the clip): its segments are cut and shifted.
pub fn trim(from: &Path, to: &Path, start: f32, end: f32) -> Result<()> {
    let Some(data) = lookup(&load(), from).cloned() else { return Ok(()) };
    let new = trimmed(&data, start, end);
    modify(&store_path(), |t| {
        t.insert(key(to), new);
    })
}

fn trimmed(data: &ClipSpeakers, start: f32, end: f32) -> ClipSpeakers {
    let (a, b) = (start * data.seconds, end * data.seconds);
    let round = |t: f32| (t * 100.0).round() / 100.0;
    let segments = data
        .segments
        .iter()
        .filter(|s| s.end > a && s.start < b)
        .map(|s| Segment { start: round(s.start.max(a) - a), end: round(s.end.min(b) - a), who: s.who.clone() })
        .filter(|s| s.end > s.start)
        .collect();
    ClipSpeakers { seconds: b - a, segments }
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

fn read_json<T: serde::de::DeserializeOwned + Default>(path: &Path) -> T {
    std::fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(value)?;
    // Write a temporary file and move it over, so a crash never leaves half a file.
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("saving {}", path.display()))
}

/// Loads the table, changes it and writes it back, dropping clips that no
/// longer exist.
fn modify(store: &Path, change: impl FnOnce(&mut Table)) -> Result<()> {
    let mut table: Table = read_json(store);
    change(&mut table);
    table.retain(|path, _| Path::new(path).exists());
    write_json(store, &table)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const RATE: u32 = 48_000;

    /// Stereo audio of `seconds` with tone where `loud` says.
    fn audio(seconds: f32, loud: &[(f32, f32)]) -> Vec<f32> {
        let frames = (seconds * RATE as f32) as usize;
        let mut out = vec![0.0f32; frames * 2];
        for i in 0..frames {
            let t = i as f32 / RATE as f32;
            if loud.iter().any(|&(a, b)| t >= a && t < b) {
                let s = 0.3 * (t * 440.0 * std::f32::consts::TAU).sin();
                out[i * 2] = s;
                out[i * 2 + 1] = s;
            }
        }
        out
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.08
    }

    #[test]
    fn finds_speech_and_joins_short_gaps() {
        let spans = speech(&audio(6.0, &[(1.0, 2.0), (2.1, 2.5), (4.0, 4.05), (4.5, 5.0)]), RATE);
        // 1.0–2.5 is one stretch (a 0.1 s gap is held over); the 50 ms blip
        // is too short on its own (0.45 s before the next sound).
        assert_eq!(spans.len(), 2, "{spans:?}");
        assert!(close(spans[0].0, 1.0) && close(spans[0].1, 2.5), "{spans:?}");
        assert!(close(spans[1].0, 4.5) && close(spans[1].1, 5.0), "{spans:?}");
        assert!(speech(&audio(2.0, &[]), RATE).is_empty());
    }

    #[test]
    fn turns_events_into_spans_per_person() {
        let end = Instant::now() + Duration::from_secs(100);
        let at = |secs_before_end: f32| end - Duration::from_secs_f32(secs_before_end);
        let ev = |t: f32, user: &str, speaking: bool| Event { at: at(t), user: user.into(), speaking };
        // A 10 s clip: bob talks from before it starts until 7 s; alice from
        // 5 s to the end; "me" is left out (the mic covers it).
        let events = [
            ev(12.0, "bob", true),
            ev(5.0, "alice", true),
            ev(4.0, "me", true),
            ev(3.0, "bob", false),
            ev(2.0, "me", false),
        ];
        let spans = speaking_spans(&events, Some("me"), end, 10.0);
        assert_eq!(spans.len(), 2, "{spans:?}");
        assert!(spans.iter().any(|(u, a, b)| u == "bob" && close(*a, 0.0) && close(*b, 7.0)), "{spans:?}");
        assert!(spans.iter().any(|(u, a, b)| u == "alice" && close(*a, 5.0) && close(*b, 10.0)), "{spans:?}");
    }

    #[test]
    fn snaps_names_onto_speech_and_keeps_overlaps() {
        let discord = [(1.0, 3.0), (2.5, 4.0), (6.0, 7.0)];
        let mic = [(8.0, 9.0)];
        // Events come ~0.2 s late; bob and alice overlap; 6–7 s has no event.
        let spans = [("bob".to_string(), 1.2, 3.2), ("alice".to_string(), 2.7, 4.3)];
        let segs = assign(&discord, &mic, &spans);
        let find = |who: Option<&str>| segs.iter().filter(|s| s.who.as_deref() == who).collect::<Vec<_>>();
        let bob = find(Some("bob"));
        assert!(close(bob[0].start, 1.0) && close(bob[0].end, 3.0), "{segs:?}");
        let alice = find(Some("alice"));
        assert!(close(alice[0].start, 2.5) && close(alice[0].end, 4.0), "{segs:?}");
        let other = find(None);
        assert_eq!(other.len(), 1, "{segs:?}");
        assert!(close(other[0].start, 6.0));
        let you = find(Some(YOU));
        assert!(close(you[0].start, 8.0) && close(you[0].end, 9.0));
    }

    #[test]
    fn smoothing_joins_close_pieces_and_drops_blips() {
        let seg = |start: f32, end: f32, who: Option<&str>| Segment { start, end, who: who.map(str::to_string) };
        let segs = [
            // bob: three sentences with short pauses, then a blip far away.
            seg(1.0, 2.0, Some("bob")),
            seg(2.5, 3.0, Some("bob")),
            seg(3.4, 4.0, Some("bob")),
            seg(9.0, 9.1, Some("bob")),
            // alice overlaps bob: never joined with him.
            seg(2.2, 2.9, Some("alice")),
            // Short pieces close together become one line instead of vanishing.
            seg(6.0, 6.1, None),
            seg(6.3, 6.4, None),
            seg(6.6, 6.7, None),
        ];
        let out = smoothed(&segs, MERGE_GAP, MIN_SEGMENT);
        let got: Vec<(f32, f32, Option<&str>)> = out.iter().map(|s| (s.start, s.end, s.who.as_deref())).collect();
        assert_eq!(got, [(1.0, 4.0, Some("bob")), (2.2, 2.9, Some("alice")), (6.0, 6.7, None)]);
    }

    #[test]
    fn trimming_cuts_and_shifts_segments() {
        let data = ClipSpeakers {
            seconds: 10.0,
            segments: vec![
                Segment { start: 1.0, end: 3.0, who: Some("bob".into()) },
                Segment { start: 4.0, end: 6.0, who: Some(YOU.into()) },
                Segment { start: 8.0, end: 9.0, who: None },
            ],
        };
        // Keep 2 s .. 7 s.
        let t = trimmed(&data, 0.2, 0.7);
        assert!(close(t.seconds, 5.0));
        assert_eq!(t.segments.len(), 2);
        assert!(close(t.segments[0].start, 0.0) && close(t.segments[0].end, 1.0));
        assert!(close(t.segments[1].start, 2.0) && close(t.segments[1].end, 4.0));
    }

    #[test]
    fn stores_as_json_and_survives_garbage() {
        let dir = std::env::temp_dir().join(format!("discord-clip-speakers-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("clip.ogg");
        std::fs::write(&clip, b"x").unwrap();
        let store = dir.join("clip-speakers.json");
        std::fs::write(&store, "not json").unwrap();
        assert!(read_json::<Table>(&store).is_empty());
        let data = ClipSpeakers { seconds: 2.0, segments: vec![Segment { start: 0.5, end: 1.0, who: Some("1".into()) }] };
        modify(&store, |t| {
            t.insert(key(&clip), data.clone());
            t.insert("c:\\gone.ogg".into(), data.clone());
        })
        .unwrap();
        let table: Table = read_json(&store);
        assert_eq!(lookup(&table, &clip), Some(&data));
        assert_eq!(table.len(), 1, "clips that are gone are dropped");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
