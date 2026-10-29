// The parent of a root entry, which has none.
pub const NO_PARENT: u32 = u32::MAX;

pub struct Index {
    // Lowercased names, used for matching.
    names: String,
    // Original-case names, used for showing results. Kept separate because
    // lowercasing can change byte length.
    display_names: String,
    entries: Vec<Entry>,
    // `char_mask` of each lowercased name, by entry ID. Kept apart from
    // `entries` so search can scan them as one tight array and skip most
    // names without reading them.
    masks: Vec<u64>,
}

#[allow(dead_code)]
pub struct Entry {
    name_start: u32,
    name_len: u16,
    display_start: u32,
    display_len: u16,
    parent: u32,
    is_dir: bool,
    size: u64,
    modified: u64,
}

// Which characters a lowercased name contains, one bit per character. Letters
// and digits get a bit each; other ASCII characters share the remaining bits.
// Non-ASCII characters aren't tracked. Search uses this as a quick filter:
// a name missing a query character can't match it exactly, and needs at
// least one edit to match it as a typo. Shared bits and untracked characters
// only make the filter let more names through, never fewer.
pub fn char_mask(lowercase: &str) -> u64 {
    let mut mask = 0;
    for b in lowercase.bytes() {
        let bit = match b {
            b'a'..=b'z' => b - b'a',
            b'0'..=b'9' => 26 + (b - b'0'),
            0..=127 => 36 + b % 28,
            _ => continue,
        };
        mask |= 1 << bit;
    }
    mask
}

// Lowercases `text` the way names and queries are both matched. Plain
// `to_lowercase` turns a Greek capital sigma at the end of a word into final
// 'ς' and anywhere else into 'σ', so the query "ΟΔΟΣ" wouldn't be a prefix of
// "ΟΔΟΣΤΡΩΜΑ". Both forms are 2 bytes, so folding keeps byte offsets.
pub(crate) fn fold_case(text: &str) -> String {
    let lowercase = text.to_lowercase();
    if lowercase.contains('ς') {
        return lowercase.replace('ς', "σ");
    }
    lowercase
}

// Returned by `push` when a name is longer than `u16::MAX` bytes.
#[derive(Debug, PartialEq, Eq)]
pub struct NameTooLong;

impl Index {
    pub fn new() -> Index {
        Index {
            names: String::new(),
            display_names: String::new(),
            entries: Vec::new(),
            masks: Vec::new(),
        }
    }

    // Appends an entry and returns its ID. `parent` is `NO_PARENT` for a root.
    pub fn push(
        &mut self,
        name: &str,
        parent: u32,
        is_dir: bool,
        size: u64,
        modified: u64,
    ) -> Result<u32, NameTooLong> {
        self.push_entry(name, name, parent, is_dir, size, modified)
    }

    // Appends a root and returns its ID. `path` is its full path, shown at the
    // start of every result path under it. Only its last part is matched,
    // like any other folder's name, so a root can be found by its own name.
    // A path with no last part ("/", "C:\") is matched whole.
    pub fn push_root(
        &mut self,
        path: &str,
        is_dir: bool,
        size: u64,
        modified: u64,
    ) -> Result<u32, NameTooLong> {
        let last = std::path::Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(path);
        self.push_entry(last, path, NO_PARENT, is_dir, size, modified)
    }

    // `name` is what search matches; `display` is what results show.
    fn push_entry(
        &mut self,
        name: &str,
        display: &str,
        parent: u32,
        is_dir: bool,
        size: u64,
        modified: u64,
    ) -> Result<u32, NameTooLong> {
        let lowercase = fold_case(name);
        if lowercase.len() > u16::MAX as usize || display.len() > u16::MAX as usize {
            return Err(NameTooLong);
        }
        let id = self.entries.len() as u32;
        self.entries.push(Entry {
            name_start: self.names.len() as u32,
            name_len: lowercase.len() as u16,
            display_start: self.display_names.len() as u32,
            display_len: display.len() as u16,
            parent,
            is_dir,
            size,
            modified,
        });
        self.masks.push(char_mask(&lowercase));
        self.names.push_str(&lowercase);
        self.display_names.push_str(display);
        Ok(id)
    }

    pub fn display_name(&self, id: u32) -> Option<&str> {
        let e = self.entries.get(id as usize)?;
        let start = e.display_start as usize;
        self.display_names
            .get(start..(start + e.display_len as usize))
    }

    pub fn name(&self, id: u32) -> Option<&str> {
        let e = self.entries.get(id as usize)?;
        let start = e.name_start as usize;
        self.names.get(start..(start + e.name_len as usize))
    }

    pub fn full_path(&self, id: u32) -> Option<std::path::PathBuf> {
        let mut parts = Vec::new();
        let mut curr = id;
        while curr != NO_PARENT {
            // A valid chain visits each entry at most once, so a longer one
            // means the parents form a cycle.
            if parts.len() >= self.entries.len() {
                return None;
            }
            parts.push(self.display_name(curr)?);
            curr = self.entries[curr as usize].parent;
        }

        Some(parts.iter().rev().collect())
    }

    // The folder `id` is in, or `None` for a root.
    pub fn parent(&self, id: u32) -> Option<u32> {
        let parent = self.entries.get(id as usize)?.parent;
        (parent != NO_PARENT).then_some(parent)
    }

    // Seconds since 1970, or 0 if the OS couldn't say.
    pub fn modified(&self, id: u32) -> Option<u64> {
        Some(self.entries.get(id as usize)?.modified)
    }

    pub fn masks(&self) -> &[u64] {
        &self.masks
    }

    // Number of entries. IDs run from 0 to `len() - 1`.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    // Number of folders above `id`, so a root is 0.
    pub fn depth(&self, id: u32) -> Option<u32> {
        let mut depth = 0;
        let mut curr = self.entries.get(id as usize)?.parent;
        while curr != NO_PARENT {
            // As in `full_path`, a chain longer than the index is a cycle.
            if depth as usize >= self.entries.len() {
                return None;
            }
            curr = self.entries.get(curr as usize)?.parent;
            depth += 1;
        }
        Some(depth)
    }
}

impl Default for Index {
    fn default() -> Self {
        Self::new()
    }
}

// Bytes per entry in a saved index (see `store`).
pub(crate) const ENTRY_BYTES: usize = 33;

// Raw access for `store`, which saves the parts as they are and checks them
// on the way back in.
impl Index {
    pub(crate) fn parts(&self) -> (&str, &str, &[Entry]) {
        (&self.names, &self.display_names, &self.entries)
    }

    // Builds an index from loaded parts, checking everything the accessors
    // rely on, so a corrupt file is an error here rather than a panic or a
    // wrong slice later. Masks aren't saved but rebuilt: see BENCHMARKS.md,
    // "store".
    pub(crate) fn from_parts(
        names: String,
        display_names: String,
        entries: Vec<Entry>,
    ) -> Result<Index, &'static str> {
        // `NO_PARENT` must never be a real ID.
        if entries.len() >= NO_PARENT as usize {
            return Err("too many entries");
        }
        let mut masks = Vec::with_capacity(entries.len());
        for e in &entries {
            // `get` is `None` when a range is out of bounds or splits a
            // character.
            let Some(name) = names.get(range(e.name_start, e.name_len)) else {
                return Err("name out of bounds or not on a character boundary");
            };
            if display_names
                .get(range(e.display_start, e.display_len))
                .is_none()
            {
                return Err("display name out of bounds or not on a character boundary");
            }
            if e.parent != NO_PARENT && e.parent as usize >= entries.len() {
                return Err("parent out of range");
            }
            masks.push(char_mask(name));
        }
        Ok(Index {
            names,
            display_names,
            entries,
            masks,
        })
    }
}

// Saturating, so a huge start can't overflow on 32-bit targets; the range is
// then just out of bounds.
fn range(start: u32, len: u16) -> std::ops::Range<usize> {
    let start = start as usize;
    start..start.saturating_add(len as usize)
}

impl Entry {
    // The saved form: every field in declaration order, little-endian, with
    // `is_dir` as one byte.
    pub(crate) fn to_bytes(&self) -> [u8; ENTRY_BYTES] {
        let mut b = [0; ENTRY_BYTES];
        b[0..4].copy_from_slice(&self.name_start.to_le_bytes());
        b[4..6].copy_from_slice(&self.name_len.to_le_bytes());
        b[6..10].copy_from_slice(&self.display_start.to_le_bytes());
        b[10..12].copy_from_slice(&self.display_len.to_le_bytes());
        b[12..16].copy_from_slice(&self.parent.to_le_bytes());
        b[16] = self.is_dir as u8;
        b[17..25].copy_from_slice(&self.size.to_le_bytes());
        b[25..33].copy_from_slice(&self.modified.to_le_bytes());
        b
    }

    // Reads what `to_bytes` wrote. `None` if `is_dir` is neither 0 nor 1.
    pub(crate) fn from_bytes(b: &[u8; ENTRY_BYTES]) -> Option<Entry> {
        let u16_at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
        let u32_at = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        let u64_at = |i: usize| (u32_at(i) as u64) | ((u32_at(i + 4) as u64) << 32);
        let is_dir = match b[16] {
            0 => false,
            1 => true,
            _ => return None,
        };
        Some(Entry {
            name_start: u32_at(0),
            name_len: u16_at(4),
            display_start: u32_at(6),
            display_len: u16_at(10),
            parent: u32_at(12),
            is_dir,
            size: u64_at(17),
            modified: u64_at(25),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn new_index_is_empty() {
        let index = Index::new();
        assert_eq!(index.name(0), None);
        assert_eq!(index.display_name(0), None);
        assert_eq!(index.full_path(0), None);
    }

    #[test]
    fn len_counts_entries() {
        let mut index = Index::new();
        assert!(index.is_empty());
        index.push("c", u32::MAX, true, 0, 0).unwrap();
        index.push("a.txt", 0, false, 0, 0).unwrap();
        assert_eq!(index.len(), 2);
        assert!(!index.is_empty());
    }

    #[test]
    fn push_assigns_sequential_ids() {
        let mut index = Index::new();
        assert_eq!(index.push("c", NO_PARENT, true, 0, 0), Ok(0));
        assert_eq!(index.push("docs", 0, true, 0, 0), Ok(1));
        assert_eq!(index.push("a.txt", 1, false, 10, 0), Ok(2));
        assert_eq!(
            index.full_path(2),
            Some(Path::new("c").join("docs").join("a.txt"))
        );
    }

    #[test]
    fn root_matches_by_last_part_and_shows_full_path() {
        let mut index = Index::new();
        let root = index.push_root("/home/Me/Projects", true, 0, 0).unwrap();
        let file = index.push("a.txt", root, false, 0, 0).unwrap();
        assert_eq!(index.name(root), Some("projects"));
        assert_eq!(index.masks()[root as usize], char_mask("projects"));
        assert_eq!(index.display_name(root), Some("/home/Me/Projects"));
        assert_eq!(
            index.full_path(file),
            Some(Path::new("/home/Me/Projects").join("a.txt"))
        );
    }

    #[test]
    fn root_with_no_last_part_matches_whole() {
        let mut index = Index::new();
        let root = index.push_root("/", true, 0, 0).unwrap();
        assert_eq!(index.name(root), Some("/"));
    }

    #[test]
    fn name_is_lowercased() {
        let mut index = Index::new();
        let id = index.push("ReadMe.MD", NO_PARENT, false, 0, 0).unwrap();
        assert_eq!(index.name(id), Some("readme.md"));
    }

    #[test]
    fn final_sigma_folds_like_any_sigma() {
        let mut index = Index::new();
        let id = index.push("ΟΔΟΣ.txt", NO_PARENT, false, 0, 0).unwrap();
        assert_eq!(index.name(id), Some("οδοσ.txt"));
        assert_eq!(index.display_name(id), Some("ΟΔΟΣ.txt"));
        assert_eq!(fold_case("ΟΔΟΣ"), fold_case("οδος"));
    }

    #[test]
    fn display_name_keeps_case() {
        let mut index = Index::new();
        let id = index.push("ReadMe.MD", NO_PARENT, false, 0, 0).unwrap();
        assert_eq!(index.display_name(id), Some("ReadMe.MD"));
    }

    // 'ẞ' is 3 bytes but lowercases to 2-byte 'ß'; 'İ' is 2 bytes but lowercases
    // to 3 bytes ("i" + combining dot). Both buffers must stay in sync, and the
    // entry pushed afterwards must not be shifted.
    #[test]
    fn lowercasing_that_changes_length() {
        let mut index = Index::new();
        let a = index.push("ẞig", NO_PARENT, false, 0, 0).unwrap();
        let b = index.push("İx", NO_PARENT, false, 0, 0).unwrap();
        let c = index.push("plain", NO_PARENT, false, 0, 0).unwrap();
        assert_eq!(index.name(a), Some("ßig"));
        assert_eq!(index.display_name(a), Some("ẞig"));
        assert_eq!(index.name(b), Some("i\u{307}x"));
        assert_eq!(index.display_name(b), Some("İx"));
        assert_eq!(index.name(c), Some("plain"));
        assert_eq!(index.display_name(c), Some("plain"));
    }

    #[test]
    fn push_rejects_name_over_u16_max() {
        let mut index = Index::new();
        index.push("ok", NO_PARENT, false, 0, 0).unwrap();
        let long = "x".repeat(u16::MAX as usize + 1);
        assert_eq!(index.push(&long, NO_PARENT, false, 0, 0), Err(NameTooLong));
        // The failed push must not leave a partial entry or name behind.
        assert_eq!(index.name(1), None);
        assert_eq!(index.names, "ok");
        assert_eq!(index.display_names, "ok");
    }

    // 'ẞ' is 3 bytes but lowercases to 2-byte 'ß', so this name is too long
    // only in its original form. Checking just the lowercase length would let
    // `display_len` silently truncate.
    #[test]
    fn push_rejects_long_display_name_that_shrinks() {
        let mut index = Index::new();
        let long = "ẞ".repeat(21_846);
        assert!(long.len() > u16::MAX as usize);
        assert!(long.to_lowercase().len() <= u16::MAX as usize);
        assert_eq!(index.push(&long, NO_PARENT, false, 0, 0), Err(NameTooLong));
    }

    #[test]
    fn name_returns_each_entry() {
        let mut index = Index::new();
        let a = index.push("ab", NO_PARENT, false, 0, 0).unwrap();
        let b = index.push("c", NO_PARENT, false, 0, 0).unwrap();
        let c = index.push("def", NO_PARENT, false, 0, 0).unwrap();
        assert_eq!(index.name(a), Some("ab"));
        assert_eq!(index.name(b), Some("c"));
        assert_eq!(index.name(c), Some("def"));
    }

    #[test]
    fn name_out_of_range_is_none() {
        let mut index = Index::new();
        index.push("ab", NO_PARENT, false, 0, 0).unwrap();
        assert_eq!(index.name(1), None);
        assert_eq!(index.name(u32::MAX - 1), None);
    }

    // Offsets are byte offsets, so multi-byte characters must not shift later names.
    #[test]
    fn name_handles_multibyte() {
        let mut index = Index::new();
        let a = index.push("café.txt", NO_PARENT, false, 0, 0).unwrap();
        let b = index.push("next", NO_PARENT, false, 0, 0).unwrap();
        assert_eq!(index.name(a), Some("café.txt"));
        assert_eq!(index.name(b), Some("next"));
    }

    #[test]
    fn full_path_of_root_is_its_name() {
        let mut index = Index::new();
        let root = index.push("c", NO_PARENT, true, 0, 0).unwrap();
        assert_eq!(index.full_path(root), Some(Path::new("c").to_path_buf()));
    }

    #[test]
    fn full_path_out_of_range_is_none() {
        let mut index = Index::new();
        index.push("c", NO_PARENT, true, 0, 0).unwrap();
        assert_eq!(index.full_path(1), None);
    }

    #[test]
    fn full_path_with_missing_parent_is_none() {
        let mut index = Index::new();
        index.push("c", NO_PARENT, true, 0, 0).unwrap();
        let orphan = index.push("a.txt", 7, false, 0, 0).unwrap();
        assert_eq!(index.full_path(orphan), None);
    }

    #[test]
    fn full_path_with_parent_cycle_is_none() {
        let mut index = Index::new();
        let a = index.push("a", 1, true, 0, 0).unwrap();
        let b = index.push("b", 0, true, 0, 0).unwrap();
        assert_eq!(index.full_path(a), None);
        assert_eq!(index.full_path(b), None);
    }

    #[test]
    fn full_path_walks_parents() {
        let mut index = Index::new();
        let root = index.push("c", NO_PARENT, true, 0, 0).unwrap();
        let docs = index.push("docs", root, true, 0, 0).unwrap();
        let file = index.push("a.txt", docs, false, 0, 0).unwrap();
        assert_eq!(
            index.full_path(file),
            Some(Path::new("c").join("docs").join("a.txt"))
        );
    }

    #[test]
    fn siblings_share_parent() {
        let mut index = Index::new();
        let root = index.push("c", NO_PARENT, true, 0, 0).unwrap();
        let a = index.push("a.txt", root, false, 0, 0).unwrap();
        let b = index.push("b.txt", root, false, 0, 0).unwrap();
        assert_eq!(index.full_path(a), Some(Path::new("c").join("a.txt")));
        assert_eq!(index.full_path(b), Some(Path::new("c").join("b.txt")));
    }

    #[test]
    fn depth_counts_parents() {
        let mut index = Index::new();
        let root = index.push("c", NO_PARENT, true, 0, 0).unwrap();
        let docs = index.push("docs", root, true, 0, 0).unwrap();
        let file = index.push("a.txt", docs, false, 0, 0).unwrap();
        assert_eq!(index.depth(root), Some(0));
        assert_eq!(index.depth(file), Some(2));
        assert_eq!(index.depth(file + 1), None);
    }

    #[test]
    fn depth_with_parent_cycle_is_none() {
        let mut index = Index::new();
        let a = index.push("a", 1, true, 0, 0).unwrap();
        index.push("b", 0, true, 0, 0).unwrap();
        assert_eq!(index.depth(a), None);
    }

    #[test]
    fn parent_returns_folder_above() {
        let mut index = Index::new();
        let root = index.push("c", NO_PARENT, true, 0, 0).unwrap();
        let file = index.push("a.txt", root, false, 0, 0).unwrap();
        assert_eq!(index.parent(file), Some(root));
        assert_eq!(index.parent(root), None);
        assert_eq!(index.parent(file + 1), None);
    }

    #[test]
    fn modified_returns_each_entry() {
        let mut index = Index::new();
        let a = index.push("a", NO_PARENT, false, 0, 100).unwrap();
        let b = index.push("b", NO_PARENT, false, 0, 200).unwrap();
        assert_eq!(index.modified(a), Some(100));
        assert_eq!(index.modified(b), Some(200));
        assert_eq!(index.modified(b + 1), None);
    }

    #[test]
    fn char_mask_sets_one_bit_per_letter_and_digit() {
        assert_eq!(char_mask("a"), 1);
        assert_eq!(char_mask("b"), 1 << 1);
        assert_eq!(char_mask("abba"), 0b11);
        assert_eq!(char_mask("0"), 1 << 26);
        assert_ne!(char_mask("a"), char_mask("0"));
    }

    #[test]
    fn char_mask_tracks_ascii_punctuation() {
        assert_ne!(char_mask("."), 0);
        assert_eq!(char_mask("a.b") & !char_mask("ab"), char_mask("."));
    }

    // Non-ASCII characters aren't tracked, so they can never make a name look
    // like it's missing a query character it actually has.
    #[test]
    fn char_mask_ignores_non_ascii() {
        assert_eq!(char_mask("é"), 0);
        assert_eq!(char_mask("café"), char_mask("caf"));
    }

    #[test]
    fn masks_hold_each_lowercased_name() {
        let mut index = Index::new();
        index.push("AB", NO_PARENT, false, 0, 0).unwrap();
        index.push("c1", NO_PARENT, false, 0, 0).unwrap();
        assert_eq!(index.masks(), [char_mask("ab"), char_mask("c1")]);
    }

    #[test]
    fn failed_push_adds_no_mask() {
        let mut index = Index::new();
        let long = "x".repeat(u16::MAX as usize + 1);
        assert_eq!(index.push(&long, NO_PARENT, false, 0, 0), Err(NameTooLong));
        assert!(index.masks().is_empty());
    }
}
