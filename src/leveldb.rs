//! A small read-only reader for LevelDB folders, just enough to read a few
//! keys from Chromium's Local Storage (Discord's theme, #37).
//!
//! A normal LevelDB library takes the folder's `LOCK`, which the running
//! Chromium app holds, so this reads the files directly instead: the sorted
//! tables (`.ldb`/`.sst`, Snappy-compressed blocks) and the write-ahead log
//! (`.log`) with the newest changes. Files are opened with full sharing and
//! never written. Of every key the caller wants, the version with the highest
//! sequence number wins.
//!
//! Formats: <https://github.com/google/leveldb/blob/main/doc/table_format.md>
//! and <https://github.com/google/leveldb/blob/main/doc/log_format.md>.
//! Damaged or unknown data is skipped, never a panic.

use std::collections::HashMap;
use std::path::Path;

/// Newest value of each wanted key; deleted keys are left out.
pub fn read(dir: &Path, wanted: &dyn Fn(&[u8]) -> bool) -> std::io::Result<HashMap<Vec<u8>, Vec<u8>>> {
    // LevelDB may compact (rename and delete files) while we read: if a
    // listed file is gone, start over once.
    match read_once(dir, wanted) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => read_once(dir, wanted),
        other => other,
    }
}

struct Entry {
    seq: u64,
    /// `None` = deleted.
    value: Option<Vec<u8>>,
}

type Entries = HashMap<Vec<u8>, Entry>;

fn read_once(dir: &Path, wanted: &dyn Fn(&[u8]) -> bool) -> std::io::Result<HashMap<Vec<u8>, Vec<u8>>> {
    let mut entries = Entries::new();
    for item in std::fs::read_dir(dir)? {
        let path = item?.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        match ext.as_str() {
            "ldb" | "sst" => read_table(&std::fs::read(&path)?, wanted, &mut entries),
            "log" => read_log(&std::fs::read(&path)?, wanted, &mut entries),
            _ => {}
        }
    }
    Ok(entries.into_iter().filter_map(|(k, e)| Some((k, e.value?))).collect())
}

fn put(entries: &mut Entries, key: &[u8], seq: u64, value: Option<&[u8]>) {
    if entries.get(key).is_some_and(|e| e.seq >= seq) {
        return;
    }
    entries.insert(key.to_vec(), Entry { seq, value: value.map(<[u8]>::to_vec) });
}

/// A cursor over bytes where every read can fail instead of panicking.
struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Cursor { data, pos: 0 }
    }

    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let out = self.data.get(self.pos..self.pos.checked_add(n)?)?;
        self.pos += n;
        Some(out)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.bytes(1)?[0])
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.bytes(4)?.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.bytes(8)?.try_into().ok()?))
    }

    fn varint(&mut self) -> Option<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.u8()?;
            v |= u64::from(b & 0x7F) << shift;
            if b & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }

    fn varbytes(&mut self) -> Option<&'a [u8]> {
        let n = usize::try_from(self.varint()?).ok()?;
        self.bytes(n)
    }

    fn done(&self) -> bool {
        self.pos >= self.data.len()
    }
}

// ---------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------

const TABLE_MAGIC: u64 = 0xdb47_7524_8b80_fb57;

fn read_table(data: &[u8], wanted: &dyn Fn(&[u8]) -> bool, entries: &mut Entries) {
    let _ = (|| -> Option<()> {
        let footer = data.get(data.len().checked_sub(48)?..)?;
        if u64::from_le_bytes(footer[40..48].try_into().ok()?) != TABLE_MAGIC {
            return None;
        }
        let mut c = Cursor::new(footer);
        let _metaindex = (c.varint()?, c.varint()?);
        let index = read_block(data, c.varint()?, c.varint()?)?;
        for (_, handle) in block_entries(&index) {
            let mut h = Cursor::new(&handle);
            // A damaged or unknown block only loses that block.
            let Some(block) = (|| read_block(data, h.varint()?, h.varint()?))() else { continue };
            for (key, value) in block_entries(&block) {
                // Internal key = user key + 8 bytes of (sequence << 8 | kind).
                let Some(split) = key.len().checked_sub(8) else { continue };
                let (user_key, tag) = key.split_at(split);
                if !wanted(user_key) {
                    continue;
                }
                let tag = u64::from_le_bytes(tag.try_into().ok()?);
                let value = (tag & 0xFF == 1).then_some(value.as_slice());
                put(entries, user_key, tag >> 8, value);
            }
        }
        Some(())
    })();
}

/// A block's contents, decompressed. Each block is followed by a 1-byte
/// compression type and a 4-byte checksum.
fn read_block(data: &[u8], offset: u64, size: u64) -> Option<Vec<u8>> {
    let start = usize::try_from(offset).ok()?;
    let end = start.checked_add(usize::try_from(size).ok()?)?;
    let raw = data.get(start..end)?;
    match *data.get(end)? {
        0 => Some(raw.to_vec()),
        1 => snap::raw::Decoder::new().decompress_vec(raw).ok(),
        _ => None,
    }
}

/// The key/value pairs of a block. Keys share a prefix with the previous
/// key; the block ends with a list of restart points we don't need.
fn block_entries(block: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut out = Vec::new();
    let Some(n) = block.len().checked_sub(4).and_then(|i| block.get(i..)) else { return out };
    let restarts = u32::from_le_bytes(n.try_into().unwrap_or_default()) as usize;
    let Some(end) = restarts.checked_mul(4).and_then(|r| block.len().checked_sub(4 + r)) else { return out };
    let mut c = Cursor::new(&block[..end]);
    let mut key = Vec::new();
    while !c.done() {
        let Some((shared, unshared, value_len)) = (|| Some((c.varint()?, c.varint()?, c.varint()?)))() else { break };
        let (Ok(shared), Ok(unshared), Ok(value_len)) =
            (usize::try_from(shared), usize::try_from(unshared), usize::try_from(value_len))
        else {
            break;
        };
        if shared > key.len() {
            break;
        }
        let Some(rest) = c.bytes(unshared) else { break };
        key.truncate(shared);
        key.extend_from_slice(rest);
        let Some(value) = c.bytes(value_len) else { break };
        out.push((key.clone(), value.to_vec()));
    }
    out
}

// ---------------------------------------------------------------------------
// Write-ahead log
// ---------------------------------------------------------------------------

const LOG_BLOCK: usize = 32 * 1024;

/// The log is a series of 32 KiB blocks of records; a record holds a whole
/// write batch, or a fragment of one (first / middle / last).
fn read_log(data: &[u8], wanted: &dyn Fn(&[u8]) -> bool, entries: &mut Entries) {
    let mut batch: Vec<u8> = Vec::new();
    for block in data.chunks(LOG_BLOCK) {
        let mut c = Cursor::new(block);
        while block.len() - c.pos >= 7 {
            let Some((_crc, len, kind)) = (|| Some((c.u32()?, c.bytes(2)?, c.u8()?)))() else { break };
            let len = u16::from_le_bytes([len[0], len[1]]) as usize;
            if kind == 0 {
                break; // zero padding (pre-allocated, or the rest of a block)
            }
            let Some(fragment) = c.bytes(len) else { break };
            match kind {
                1 => read_batch(fragment, wanted, entries),
                2 => batch = fragment.to_vec(),
                3 => batch.extend_from_slice(fragment),
                4 => {
                    batch.extend_from_slice(fragment);
                    read_batch(&batch, wanted, entries);
                    batch.clear();
                }
                _ => break,
            }
        }
    }
}

/// A write batch: sequence number, count, then `count` puts / deletes. Each
/// gets the next sequence number.
fn read_batch(data: &[u8], wanted: &dyn Fn(&[u8]) -> bool, entries: &mut Entries) {
    let mut c = Cursor::new(data);
    let (Some(seq), Some(count)) = (c.u64(), c.u32()) else { return };
    for i in 0..u64::from(count) {
        let Some(kind) = c.u8() else { return };
        let Some(key) = c.varbytes() else { return };
        let value = match kind {
            1 => match c.varbytes() {
                Some(v) => Some(v),
                None => return,
            },
            0 => None,
            _ => return,
        };
        if wanted(key) {
            put(entries, key, seq + i, value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn varint(mut v: usize, out: &mut Vec<u8>) {
        while v >= 0x80 {
            out.push((v as u8) | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }

    /// A block with prefix-shared keys, as LevelDB writes them.
    fn block(pairs: &[(&[u8], &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut prev: &[u8] = &[];
        for (k, v) in pairs {
            let shared = prev.iter().zip(k.iter()).take_while(|(a, b)| a == b).count();
            varint(shared, &mut out);
            varint(k.len() - shared, &mut out);
            varint(v.len(), &mut out);
            out.extend_from_slice(&k[shared..]);
            out.extend_from_slice(v);
            prev = k;
        }
        out.extend_from_slice(&0u32.to_le_bytes()); // restart at 0
        out.extend_from_slice(&1u32.to_le_bytes());
        out
    }

    fn internal(key: &[u8], seq: u64, kind: u8) -> Vec<u8> {
        let mut k = key.to_vec();
        k.extend_from_slice(&((seq << 8) | u64::from(kind)).to_le_bytes());
        k
    }

    fn table(pairs: &[(&[u8], &[u8])], compress: bool) -> Vec<u8> {
        let data_block = block(pairs);
        let mut file = Vec::new();
        let stored = if compress { snap::raw::Encoder::new().compress_vec(&data_block).unwrap() } else { data_block };
        file.extend_from_slice(&stored);
        file.push(u8::from(compress));
        file.extend_from_slice(&[0; 4]);
        let mut handle = Vec::new();
        varint(0, &mut handle);
        varint(stored.len(), &mut handle);
        let last_key = pairs.last().unwrap().0;
        let index = block(&[(last_key, &handle)]);
        let index_offset = file.len();
        file.extend_from_slice(&index);
        file.push(0);
        file.extend_from_slice(&[0; 4]);
        let mut footer = Vec::new();
        varint(0, &mut footer); // metaindex (unused)
        varint(0, &mut footer);
        varint(index_offset, &mut footer);
        varint(index.len(), &mut footer);
        footer.resize(40, 0);
        footer.extend_from_slice(&TABLE_MAGIC.to_le_bytes());
        file.extend_from_slice(&footer);
        file
    }

    fn log_record(kind: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![0; 4];
        out.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        out.push(kind);
        out.extend_from_slice(payload);
        out
    }

    fn batch(seq: u64, ops: &[(&[u8], Option<&[u8]>)]) -> Vec<u8> {
        let mut out = seq.to_le_bytes().to_vec();
        out.extend_from_slice(&(ops.len() as u32).to_le_bytes());
        for (k, v) in ops {
            out.push(u8::from(v.is_some()));
            varint(k.len(), &mut out);
            out.extend_from_slice(k);
            if let Some(v) = v {
                varint(v.len(), &mut out);
                out.extend_from_slice(v);
            }
        }
        out
    }

    #[test]
    fn newest_value_wins_across_tables_and_log() {
        let dir = std::env::temp_dir().join(format!("discord-clip-leveldb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Older table, uncompressed.
        let (a1, b1, s1) = (internal(b"a", 1, 1), internal(b"b", 2, 1), internal(b"secret", 3, 1));
        let old: Vec<(&[u8], &[u8])> = vec![(&a1, b"a-old"), (&b1, b"b-old"), (&s1, b"token")];
        std::fs::write(dir.join("000005.ldb"), table(&old, false)).unwrap();
        // Newer table, Snappy-compressed: b changes, plus an unrelated key.
        let (b2, c2) = (internal(b"b", 10, 1), internal(b"c", 11, 1));
        let new: Vec<(&[u8], &[u8])> = vec![(&b2, b"b-new".as_slice()), (&c2, b"c-table")];
        std::fs::write(dir.join("000009.ldb"), table(&new, true)).unwrap();
        // Log: a changes, c is deleted; one batch split over two fragments.
        let whole = batch(20, &[(b"a", Some(b"a-log")), (b"c", None)]);
        let (first, last) = whole.split_at(9);
        let mut log = log_record(2, first);
        log.extend(log_record(4, last));
        log.extend(log_record(1, &batch(25, &[(b"d", Some(b"d-log"))])));
        log.resize(log.len() + 20, 0); // pre-allocated zeros at the end
        std::fs::write(dir.join("000010.log"), log).unwrap();

        let wanted = |k: &[u8]| k != b"secret";
        let got = read(&dir, &wanted).unwrap();
        let s = |k: &[u8]| got.get(k).map(|v| String::from_utf8_lossy(v).into_owned());
        assert_eq!(s(b"a").as_deref(), Some("a-log"));
        assert_eq!(s(b"b").as_deref(), Some("b-new"));
        assert_eq!(s(b"c"), None, "deleted in the log");
        assert_eq!(s(b"d").as_deref(), Some("d-log"));
        assert_eq!(s(b"secret"), None, "keys that aren't wanted are never returned");

        // Damaged files are skipped, not a panic.
        std::fs::write(dir.join("000011.ldb"), b"short").unwrap();
        std::fs::write(dir.join("000012.log"), [9u8; 100]).unwrap();
        assert_eq!(read(&dir, &wanted).unwrap().len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
