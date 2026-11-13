//! Which clips are marked as uploaded to a Discord soundboard (#28).
//!
//! The mark doesn't change the file: it's kept in
//! `%APPDATA%\discord-clip\clip-uploaded.txt`, one clip path per line (like
//! the per-clip volumes in `volumes.rs`). Entries of clips that no longer
//! exist are dropped whenever the file is written.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Marked clips, by `key()` of their path.
pub type Set = HashSet<String>;

pub fn store_path() -> PathBuf {
    crate::config::app_dir().join("clip-uploaded.txt")
}

/// Windows paths are case-insensitive.
fn key(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

pub fn load() -> Set {
    load_from(&store_path())
}

pub fn is_marked(set: &Set, path: &Path) -> bool {
    set.contains(&key(path))
}

/// Marks or unmarks a clip.
pub fn set(path: &Path, uploaded: bool) -> Result<()> {
    set_in(&store_path(), path, uploaded)
}

/// The clip was renamed or replaced by a new file: its mark follows it.
pub fn rename(from: &Path, to: &Path) -> Result<()> {
    rename_in(&store_path(), from, to)
}

fn load_from(store: &Path) -> Set {
    let Ok(text) = std::fs::read_to_string(store) else { return Set::new() };
    text.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_lowercase).collect()
}

/// Loads the set, changes it and writes it back without clips that no
/// longer exist.
fn modify(store: &Path, change: impl FnOnce(&mut Set)) -> Result<()> {
    let mut set = load_from(store);
    change(&mut set);
    set.retain(|p| Path::new(p).exists());
    let mut paths: Vec<_> = set.into_iter().collect();
    paths.sort();
    let text: String = paths.iter().map(|p| format!("{p}\n")).collect();
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
            s.insert(key(path));
        } else {
            s.remove(&key(path));
        }
    })
}

fn rename_in(store: &Path, from: &Path, to: &Path) -> Result<()> {
    if !load_from(store).contains(&key(from)) {
        return Ok(()); // nothing to do (the common case)
    }
    modify(store, |s| {
        s.remove(&key(from));
        s.insert(key(to));
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_follow_renames_and_prune() {
        let dir = std::env::temp_dir().join(format!("discord-clip-uploaded-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
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
}
