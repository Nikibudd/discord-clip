//! Per-clip volumes of the Discord and mic tracks (#7).
//!
//! Clips with separate tracks keep both tracks untouched in the file; the
//! volumes chosen on the Clips page are stored here, keyed by the clip's
//! path, and applied whenever the clip is played or shared. Stored in
//! `%APPDATA%\discord-clip\clip-volumes.txt` so nothing extra shows up in
//! the clips folder. One line per clip: `discord<TAB>mic<TAB>path`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Volume of each track of a clip, 1.0 = as recorded.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Volumes {
    pub discord: f32,
    pub mic: f32,
}

impl Default for Volumes {
    fn default() -> Self {
        Volumes { discord: 1.0, mic: 1.0 }
    }
}

impl Volumes {
    /// Highest volume the sliders go to (200%).
    pub const MAX: f32 = 2.0;

    pub fn is_default(&self) -> bool {
        (self.discord - 1.0).abs() < 0.005 && (self.mic - 1.0).abs() < 0.005
    }

    fn clamped(self) -> Volumes {
        let c = |v: f32| if v.is_finite() { v.clamp(0.0, Self::MAX) } else { 1.0 };
        Volumes { discord: c(self.discord), mic: c(self.mic) }
    }
}

pub fn store_path() -> PathBuf {
    crate::config::app_dir().join("clip-volumes.txt")
}

/// Windows paths are case-insensitive.
fn key(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

/// All stored volumes, by `key()` of the clip's path.
pub type Table = HashMap<String, Volumes>;

pub fn load() -> Table {
    load_from(&store_path())
}

/// The volumes of one clip (default if none are stored).
pub fn get(path: &Path) -> Volumes {
    lookup(&load(), path)
}

pub fn lookup(table: &Table, path: &Path) -> Volumes {
    table.get(&key(path)).copied().unwrap_or_default()
}

pub fn set(path: &Path, v: Volumes) -> Result<()> {
    set_in(&store_path(), path, v)
}

/// The clip was renamed: its volumes follow it.
pub fn rename(from: &Path, to: &Path) -> Result<()> {
    rename_in(&store_path(), from, to)
}

fn load_from(store: &Path) -> Table {
    let Ok(text) = std::fs::read_to_string(store) else { return Table::new() };
    text.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let discord = parts.next()?.trim().parse().ok()?;
            let mic = parts.next()?.trim().parse().ok()?;
            let path = parts.next()?.trim();
            (!path.is_empty()).then(|| (path.to_lowercase(), Volumes { discord, mic }.clamped()))
        })
        .collect()
}

/// Loads the table, changes it and writes it back. Entries of clips that no
/// longer exist (deleted, moved) are dropped on the way.
fn modify(store: &Path, change: impl FnOnce(&mut Table)) -> Result<()> {
    let mut table = load_from(store);
    change(&mut table);
    table.retain(|path, v| !v.is_default() && Path::new(path).exists());
    let mut entries: Vec<_> = table.into_iter().collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let text: String = entries.iter().map(|(p, v)| format!("{:.2}\t{:.2}\t{p}\n", v.discord, v.mic)).collect();
    if let Some(dir) = store.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Write a temporary file and move it over, so a crash never leaves half a file.
    let tmp = store.with_extension("tmp");
    std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, store).with_context(|| format!("saving {}", store.display()))
}

fn set_in(store: &Path, path: &Path, v: Volumes) -> Result<()> {
    modify(store, |t| {
        t.insert(key(path), v.clamped());
    })
}

fn rename_in(store: &Path, from: &Path, to: &Path) -> Result<()> {
    if !load_from(store).contains_key(&key(from)) {
        return Ok(()); // nothing to do (the common case)
    }
    modify(store, |t| {
        if let Some(v) = t.remove(&key(from)) {
            t.insert(key(to), v);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_follows_renames_and_prunes() {
        let dir = std::env::temp_dir().join(format!("discord-clip-volumes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = dir.join("clip-volumes.txt");
        let a = dir.join("discord_clip_20261109_213151.wav");
        let b = dir.join("discord_clip_20261109_213151 Bob.wav");
        let c = dir.join("other.wav");
        for p in [&a, &c] {
            std::fs::write(p, b"x").unwrap();
        }

        assert_eq!(lookup(&load_from(&store), &a), Volumes::default());
        let v = Volumes { discord: 0.5, mic: 1.5 };
        set_in(&store, &a, v).unwrap();
        // Case doesn't matter, like Windows.
        let upper = PathBuf::from(a.to_string_lossy().to_uppercase());
        assert_eq!(lookup(&load_from(&store), &upper), v);
        // Out-of-range values are clamped.
        set_in(&store, &c, Volumes { discord: 9.0, mic: -1.0 }).unwrap();
        assert_eq!(lookup(&load_from(&store), &c), Volumes { discord: 2.0, mic: 0.0 });

        std::fs::rename(&a, &b).unwrap();
        rename_in(&store, &a, &b).unwrap();
        let t = load_from(&store);
        assert_eq!(lookup(&t, &b), v);
        assert_eq!(lookup(&t, &a), Volumes::default());

        let copy = dir.join("copy.wav");
        std::fs::write(&copy, b"x").unwrap();
        set_in(&store, &copy, v).unwrap();
        assert_eq!(lookup(&load_from(&store), &copy), v);

        // Back to default removes the entry; deleted clips are dropped on the next write.
        set_in(&store, &copy, Volumes::default()).unwrap();
        std::fs::remove_file(&c).unwrap();
        set_in(&store, &b, v).unwrap();
        let t = load_from(&store);
        assert_eq!(t.len(), 1, "{t:?}");
        assert_eq!(lookup(&t, &b), v);

        // Junk lines are skipped.
        std::fs::write(&store, "nonsense\n1.0\tx\tC:\\a.wav\n0.25\t1\tC:\\b.wav\n").unwrap();
        let t = load_from(&store);
        assert_eq!(t.len(), 1);
        assert_eq!(lookup(&t, Path::new(r"C:\b.wav")), Volumes { discord: 0.25, mic: 1.0 });
        let _ = std::fs::remove_dir_all(&dir);
    }
}
