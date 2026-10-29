// Saves an index to disk and loads it back, so a frontend can show results
// at startup while a fresh walk runs: a cold walk of a large tree takes
// seconds. Core never picks a file location; the frontend does.
//
// The format, all integers little-endian:
//
// | Bytes            | Field                                 |
// |------------------|---------------------------------------|
// | 8                | magic, `OXIDEXIX`                     |
// | 4                | format version, `VERSION`             |
// | 4                | entry count                           |
// | 4                | lowercased names length, in bytes     |
// | 4                | display names length, in bytes        |
// | count × 33       | entries, as `Entry::to_bytes`         |
// | names length     | lowercased names, UTF-8               |
// | display length   | display names, UTF-8                  |
//
// The buffers are written as they sit in memory, so saving and loading are
// close to a copy. The masks aren't saved: `Index::from_parts` rebuilds them.
use std::io::{self, BufReader, BufWriter, ErrorKind, Read, Write};

use crate::index::{ENTRY_BYTES, Entry, Index};

const MAGIC: &[u8; 8] = b"OXIDEXIX";
// Bump on any layout change. Older files are then rejected, and the frontend
// falls back to a walk.
const VERSION: u32 = 1;
const HEADER_BYTES: usize = 24;

// Writes `index` in the format above. An index too large for the format's
// 32-bit lengths is an `InvalidInput` error.
pub fn write_to(index: &Index, out: impl Write) -> io::Result<()> {
    let (names, display_names, entries) = index.parts();
    let mut out = BufWriter::new(out);
    out.write_all(MAGIC)?;
    out.write_all(&VERSION.to_le_bytes())?;
    for len in [entries.len(), names.len(), display_names.len()] {
        let Ok(len) = u32::try_from(len) else {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "index too large to save",
            ));
        };
        out.write_all(&len.to_le_bytes())?;
    }
    for e in entries {
        out.write_all(&e.to_bytes())?;
    }
    out.write_all(names.as_bytes())?;
    out.write_all(display_names.as_bytes())?;
    out.flush()
}

// Reads what `write_to` wrote. A file cut short is an `UnexpectedEof` error;
// a wrong magic or version, or any part that doesn't check out, is
// `InvalidData`.
pub fn read_from(input: impl Read) -> io::Result<Index> {
    let mut input = BufReader::new(input);
    let mut header = [0; HEADER_BYTES];
    input.read_exact(&mut header)?;
    if header[..8] != MAGIC[..] {
        return Err(invalid("not an Oxidex index"));
    }
    let u32_at = |i: usize| {
        u32::from_le_bytes([header[i], header[i + 1], header[i + 2], header[i + 3]]) as u64
    };
    if u32_at(8) != VERSION as u64 {
        return Err(invalid("unsupported index version"));
    }
    let count = u32_at(12);

    let entry_bytes = read_bytes(&mut input, count * ENTRY_BYTES as u64)?;
    let names = read_string(&mut input, u32_at(16))?;
    let display_names = read_string(&mut input, u32_at(20))?;
    if input.read(&mut [0])? != 0 {
        return Err(invalid("trailing bytes after index"));
    }

    let (chunks, _) = entry_bytes.as_chunks::<ENTRY_BYTES>();
    let mut entries = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        let Some(e) = Entry::from_bytes(chunk) else {
            return Err(invalid("bad entry"));
        };
        entries.push(e);
    }
    Index::from_parts(names, display_names, entries).map_err(invalid)
}

// Reads exactly `len` bytes. The buffer grows as bytes arrive instead of
// being sized from `len`, so a corrupt length can't allocate more than the
// file actually holds.
fn read_bytes(input: &mut impl Read, len: u64) -> io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    input.by_ref().take(len).read_to_end(&mut buf)?;
    if buf.len() as u64 != len {
        return Err(ErrorKind::UnexpectedEof.into());
    }
    Ok(buf)
}

fn read_string(input: &mut impl Read, len: u64) -> io::Result<String> {
    String::from_utf8(read_bytes(input, len)?).map_err(|_| invalid("name buffer is not UTF-8"))
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::History;
    use crate::index::NO_PARENT;
    use crate::search::{Cancel, Hit, Sources, search};
    use std::fs;
    use std::num::NonZeroUsize;
    use std::sync::Arc;

    // Where entry `id` starts in a saved file.
    fn entry_at(id: usize) -> usize {
        HEADER_BYTES + id * ENTRY_BYTES
    }

    fn saved(index: &Index) -> Vec<u8> {
        let mut bytes = Vec::new();
        write_to(index, &mut bytes).unwrap();
        bytes
    }

    fn load(bytes: &[u8]) -> io::Result<Index> {
        read_from(bytes)
    }

    fn kind(bytes: &[u8]) -> ErrorKind {
        load(bytes).err().map(|e| e.kind()).unwrap()
    }

    fn set_u32(bytes: &mut [u8], at: usize, value: u32) {
        bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn set_u16(bytes: &mut [u8], at: usize, value: u16) {
        bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
    }

    // root, root/é.txt and root/Docs, with é a 2-byte character.
    fn small() -> Index {
        let mut index = Index::new();
        let root = index.push_root("/r", true, 0, 7).unwrap();
        index.push("é.txt", root, false, 3, 8).unwrap();
        index.push("Docs", root, true, 0, 9).unwrap();
        index
    }

    fn hits(index: &Arc<Index>, query: &str) -> Vec<Hit> {
        let history = History::new();
        let sources = Sources {
            indexes: std::slice::from_ref(index),
            history: &history,
        };
        let scorers = crate::score::default_scorers();
        let mut out = Vec::new();
        let _ = search(&sources, query, 50, &scorers, 0, &Cancel::never(), &mut out);
        out
    }

    #[test]
    fn walked_dir_round_trips() {
        let dir = std::env::temp_dir().join(format!("oxidex-store-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("Docs/ẞig")).unwrap();
        fs::write(dir.join("Docs/ReadMe.md"), "hi").unwrap();
        fs::write(dir.join("Docs/ẞig/İx.txt"), "").unwrap();
        fs::write(dir.join("ΟΔΟΣ.txt"), "").unwrap();
        let workers = NonZeroUsize::new(2).unwrap();
        let before = Arc::new(crate::walk::walk(&dir, workers).unwrap().index);
        fs::remove_dir_all(&dir).unwrap();

        let after = Arc::new(load(&saved(&before)).unwrap());
        assert_eq!(after.len(), before.len());
        let (a, b) = (after.parts().2, before.parts().2);
        for id in 0..before.len() as u32 {
            assert_eq!(after.name(id), before.name(id));
            assert_eq!(after.display_name(id), before.display_name(id));
            assert_eq!(after.full_path(id), before.full_path(id));
            assert_eq!(after.parent(id), before.parent(id));
            assert_eq!(after.modified(id), before.modified(id));
            // Covers the fields with no public accessor, `is_dir` and `size`.
            assert_eq!(a[id as usize].to_bytes(), b[id as usize].to_bytes());
        }
        assert_eq!(after.masks(), before.masks());
        for query in ["readme", "ßig", "i\u{307}x", "οδος", "docs/", "rdme", "x"] {
            assert_eq!(hits(&after, query), hits(&before, query), "{query}");
        }
        assert!(!hits(&after, "ßig").is_empty());
    }

    #[test]
    fn empty_index_round_trips() {
        let bytes = saved(&Index::new());
        assert_eq!(bytes.len(), HEADER_BYTES);
        assert!(load(&bytes).unwrap().is_empty());
    }

    #[test]
    fn bad_magic_is_invalid() {
        let mut bytes = saved(&small());
        bytes[0] ^= 1;
        assert_eq!(kind(&bytes), ErrorKind::InvalidData);
    }

    #[test]
    fn wrong_version_is_invalid() {
        let mut bytes = saved(&small());
        set_u32(&mut bytes, 8, VERSION + 1);
        assert_eq!(kind(&bytes), ErrorKind::InvalidData);
    }

    #[test]
    fn every_truncation_is_an_error() {
        let bytes = saved(&small());
        for len in 0..bytes.len() {
            let kind = kind(&bytes[..len]);
            assert!(
                kind == ErrorKind::UnexpectedEof || kind == ErrorKind::InvalidData,
                "{len}: {kind:?}"
            );
        }
    }

    #[test]
    fn trailing_bytes_are_invalid() {
        let mut bytes = saved(&small());
        bytes.push(0);
        assert_eq!(kind(&bytes), ErrorKind::InvalidData);
    }

    // A huge length must fail on the missing bytes, not allocate for them.
    #[test]
    fn corrupt_lengths_are_errors() {
        let bytes = saved(&small());
        for at in [12, 16, 20] {
            let mut huge = bytes.clone();
            set_u32(&mut huge, at, u32::MAX);
            assert_eq!(kind(&huge), ErrorKind::UnexpectedEof, "{at}");
        }
        // One byte short shifts where the display buffer starts.
        let mut short = bytes.clone();
        let names_len = load(&bytes).unwrap().parts().0.len() as u32;
        set_u32(&mut short, 16, names_len - 1);
        assert!(load(&short).is_err());
    }

    #[test]
    fn out_of_range_parent_is_invalid() {
        let mut bytes = saved(&small());
        set_u32(&mut bytes, entry_at(1) + 12, 3);
        assert_eq!(kind(&bytes), ErrorKind::InvalidData);
        set_u32(&mut bytes, entry_at(1) + 12, NO_PARENT);
        assert!(load(&bytes).is_ok());
    }

    #[test]
    fn name_range_out_of_bounds_is_invalid() {
        let mut bytes = saved(&small());
        set_u32(&mut bytes, entry_at(2), 1_000);
        assert_eq!(kind(&bytes), ErrorKind::InvalidData);
    }

    // Entry 1's names are "é.txt"; cutting either to 1 byte splits 'é'.
    #[test]
    fn name_range_ending_mid_character_is_invalid() {
        for at in [4, 10] {
            let mut bytes = saved(&small());
            set_u16(&mut bytes, entry_at(1) + at, 1);
            assert_eq!(kind(&bytes), ErrorKind::InvalidData, "{at}");
        }
    }

    #[test]
    fn non_utf8_buffer_is_invalid() {
        let mut bytes = saved(&small());
        let last = bytes.len() - 1;
        bytes[last] = 0xff;
        assert_eq!(kind(&bytes), ErrorKind::InvalidData);
    }

    #[test]
    fn bad_is_dir_byte_is_invalid() {
        let mut bytes = saved(&small());
        bytes[entry_at(0) + 16] = 2;
        assert_eq!(kind(&bytes), ErrorKind::InvalidData);
    }

    // Flips random bits and checks that loading, and using whatever loads,
    // never panics. A fixed seed keeps failures reproducible.
    #[test]
    fn random_corruption_never_panics() {
        let good = saved(&small());
        let mut state: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..20_000 {
            let mut bytes = good.clone();
            for _ in 0..1 + next() % 4 {
                let at = (next() % bytes.len() as u64) as usize;
                bytes[at] ^= 1 << (next() % 8);
            }
            let Ok(index) = load(&bytes) else { continue };
            let index = Arc::new(index);
            for id in 0..index.len() as u32 {
                let _ = (index.name(id), index.display_name(id));
                let _ = (index.full_path(id), index.depth(id));
            }
            let _ = (hits(&index, "doc"), hits(&index, "r/d"));
        }
    }
}
