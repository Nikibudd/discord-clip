//! Which clips are marked as uploaded to a Discord soundboard (#28), and to
//! which servers when the app uploaded them (#84).
//!
//! The mark doesn't change the file: it's kept in
//! `%APPDATA%\discord-clip\clip-uploaded.txt`, one clip path per line (like
//! the per-clip volumes in `volumes.rs`). A clip uploaded through the app has
//! a tab after the path, then its servers as `id=name` entries separated by
//! commas. Lines without a tab are marks made by hand. Entries of clips that
//! no longer exist are dropped whenever the file is written.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// A server a clip was uploaded to. The name is the one it had then: it may
/// be out of date if the server was renamed since.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Server {
    pub id: String,
    pub name: String,
}

/// Marked clips, by `key()` of their path, with the servers the app uploaded
/// them to (empty for marks made by hand).
pub type Set = HashMap<String, Vec<Server>>;

pub fn store_path() -> PathBuf {
    crate::config::app_dir().join("clip-uploaded.txt")
}

/// Windows paths are case-insensitive.
pub fn key(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

pub fn load() -> Set {
    load_from(&store_path())
}

pub fn is_marked(set: &Set, path: &Path) -> bool {
    set.contains_key(&key(path))
}

/// The servers the app uploaded this clip to; empty when it's not marked or
/// was marked by hand.
pub fn servers<'a>(set: &'a Set, path: &Path) -> &'a [Server] {
    set.get(&key(path)).map(Vec::as_slice).unwrap_or(&[])
}

/// Marks or unmarks a clip by hand. Unmarking forgets its servers too.
pub fn set(path: &Path, uploaded: bool) -> Result<()> {
    set_in(&store_path(), path, uploaded)
}

/// The app uploaded the clip to this server: marks it and adds the server.
pub fn add_server(path: &Path, server: Server) -> Result<()> {
    add_server_in(&store_path(), path, server)
}

/// The clip was renamed or replaced by a new file: its mark follows it.
pub fn rename(from: &Path, to: &Path) -> Result<()> {
    rename_in(&store_path(), from, to)
}

fn load_from(store: &Path) -> Set {
    let Ok(text) = std::fs::read_to_string(store) else { return Set::new() };
    text.lines().filter_map(parse_line).collect()
}

fn parse_line(line: &str) -> Option<(String, Vec<Server>)> {
    let (path, servers) = line.split_once('\t').unwrap_or((line, ""));
    let path = path.trim();
    if path.is_empty() {
        return None;
    }
    let servers = servers
        .split(',')
        .filter_map(|entry| {
            let (id, name) = entry.trim().split_once('=')?;
            Some(Server { id: id.trim().to_string(), name: unescape(name) })
        })
        .filter(|s| !s.id.is_empty())
        .collect();
    Some((path.to_lowercase(), servers))
}

fn format_line(path: &str, servers: &[Server]) -> String {
    if servers.is_empty() {
        return format!("{path}\n");
    }
    let list: Vec<String> = servers.iter().map(|s| format!("{}={}", escape(&s.id), escape(&s.name))).collect();
    format!("{path}\t{}\n", list.join(","))
}

/// Server names may contain anything, so the characters the format uses are
/// written as `%XX`.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '%' | ',' | '=' | '\t' | '\r' | '\n' => out.push_str(&format!("%{:02X}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        let code = rest.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok());
        match code {
            Some(b) => {
                out.push(b as char);
                rest = &rest[i + 3..];
            }
            None => {
                out.push('%');
                rest = &rest[i + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Loads the set, changes it and writes it back without clips that no
/// longer exist.
fn modify(store: &Path, change: impl FnOnce(&mut Set)) -> Result<()> {
    let mut set = load_from(store);
    change(&mut set);
    set.retain(|p, _| Path::new(p).exists());
    let mut entries: Vec<_> = set.into_iter().collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let text: String = entries.iter().map(|(p, s)| format_line(p, s)).collect();
    if let Some(dir) = store.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Write a temporary file and move it over, so a crash never leaves half a file.
    let tmp = store.with_extension("tmp");
    std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, store).with_context(|| format!("saving {}", store.display()))
}

fn set_in(store: &Path, path: &Path, uploaded: bool) -> Result<()> {
    modify(store, |s| {
        if uploaded {
            s.entry(key(path)).or_default();
        } else {
            s.remove(&key(path));
        }
    })
}

fn add_server_in(store: &Path, path: &Path, server: Server) -> Result<()> {
    modify(store, |s| {
        let servers = s.entry(key(path)).or_default();
        match servers.iter_mut().find(|s| s.id == server.id) {
            Some(known) => known.name = server.name, // uploaded again: the newer name
            None => servers.push(server),
        }
    })
}

fn rename_in(store: &Path, from: &Path, to: &Path) -> Result<()> {
    if !load_from(store).contains_key(&key(from)) {
        return Ok(()); // nothing to do (the common case)
    }
    modify(store, |s| {
        if let Some(servers) = s.remove(&key(from)) {
            s.insert(key(to), servers);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("discord-clip-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn server(id: &str, name: &str) -> Server {
        Server { id: id.into(), name: name.into() }
    }

    #[test]
    fn marks_follow_renames_and_prune() {
        let dir = temp_dir("uploaded");
        let store = dir.join("clip-uploaded.txt");
        let a = dir.join("discord_clip_20261109_213151.wav");
        let b = dir.join("discord_clip_20261109_213151.mp3");
        let c = dir.join("other.wav");
        for p in [&a, &c] {
            std::fs::write(p, b"x").unwrap();
        }

        assert!(!is_marked(&load_from(&store), &a));
        set_in(&store, &a, true).unwrap();
        set_in(&store, &c, true).unwrap();
        // Case doesn't matter, like Windows.
        let upper = PathBuf::from(a.to_string_lossy().to_uppercase());
        assert!(is_marked(&load_from(&store), &upper));

        // Replaced by a new file (e.g. Make Discord-ready): the mark moves.
        std::fs::rename(&a, &b).unwrap();
        rename_in(&store, &a, &b).unwrap();
        let s = load_from(&store);
        assert!(is_marked(&s, &b) && !is_marked(&s, &a));

        // Unmarking removes it; deleted clips are dropped on the next write.
        set_in(&store, &b, false).unwrap();
        std::fs::remove_file(&c).unwrap();
        set_in(&store, &b, true).unwrap();
        let s = load_from(&store);
        assert_eq!(s.len(), 1, "{s:?}");
        assert!(is_marked(&s, &b));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn servers_are_kept_and_follow_the_clip() {
        let dir = temp_dir("uploaded-servers");
        let store = dir.join("clip-uploaded.txt");
        let a = dir.join("A.wav");
        let b = dir.join("A.mp3");
        std::fs::write(&a, b"x").unwrap();

        add_server_in(&store, &a, server("1", "Soundboard testing")).unwrap();
        add_server_in(&store, &a, server("2", "Odd, name=100% \tfun")).unwrap();
        // Uploaded again to a renamed server: one entry, the newer name.
        add_server_in(&store, &a, server("1", "Testing")).unwrap();
        let s = load_from(&store);
        assert!(is_marked(&s, &a));
        assert_eq!(servers(&s, &a), [server("1", "Testing"), server("2", "Odd, name=100% \tfun")]);

        // Marking by hand again keeps them; the mark follows a rename.
        set_in(&store, &a, true).unwrap();
        std::fs::rename(&a, &b).unwrap();
        rename_in(&store, &a, &b).unwrap();
        let s = load_from(&store);
        assert_eq!(servers(&s, &b).len(), 2);
        assert!(servers(&s, &a).is_empty());

        // Unmarking forgets the servers.
        set_in(&store, &b, false).unwrap();
        set_in(&store, &b, true).unwrap();
        assert!(servers(&load_from(&store), &b).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reads_old_and_new_lines() {
        assert_eq!(parse_line(r"C:\Clips\A.mp3"), Some((r"c:\clips\a.mp3".into(), vec![])));
        assert_eq!(parse_line("   "), None);
        // Only the path is lowercased, not the server names.
        assert_eq!(
            parse_line("C:\\Clips\\A.mp3\t1=Big Server,2=a%2C b%25"),
            Some((r"c:\clips\a.mp3".into(), vec![server("1", "Big Server"), server("2", "a, b%")]))
        );
        assert_eq!(unescape("50%"), "50%");
        assert_eq!(unescape(&escape("x=y,%\t")), "x=y,%\t");
        assert_eq!(format_line("p", &[]), "p\n");
    }
}
