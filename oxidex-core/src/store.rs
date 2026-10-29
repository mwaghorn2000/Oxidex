//! Saves an index to disk and loads it back, so a frontend can show results
//! at startup while a fresh walk runs: a cold walk of a large tree takes
//! seconds. Core never picks a file location; the frontend does.
//!
//! The format, all integers little-endian:
//!
//! | Bytes            | Field                                 |
//! |------------------|---------------------------------------|
//! | 8                | magic, `OXIDEXIX`                     |
//! | 4                | format version, `VERSION`             |
//! | 4                | entry count                           |
//! | 4                | lowercased names length, in bytes     |
//! | 4                | display names length, in bytes        |
//! | count × 33       | entries, as `Entry::to_bytes`         |
//! | names length     | lowercased names, UTF-8               |
//! | display length   | display names, UTF-8                  |
//!
//! The buffers are written as they sit in memory, so saving and loading are
//! close to a copy. The masks aren't saved: `Index::from_parts` rebuilds them.
use std::io::{self, BufReader, BufWriter, ErrorKind, Read, Write};

use crate::index::{ENTRY_BYTES, Entry, Index};

const MAGIC: &[u8; 8] = b"OXIDEXIX";
// Bump on any layout change. Older files are then rejected, and the frontend
// falls back to a walk.
const VERSION: u32 = 1;
const HEADER_BYTES: usize = 24;

/// Writes `index` in the format above. An index too large for the format's
/// 32-bit lengths is an `InvalidInput` error.
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

/// Reads what `write_to` wrote. A file cut short is an `UnexpectedEof` error;
/// a wrong magic or version, or any part that doesn't check out, is
/// `InvalidData`.
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
