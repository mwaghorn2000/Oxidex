struct Index {
    // Lowercased names, used for matching.
    names: String,
    // Original-case names, used for showing results. Kept separate because
    // lowercasing can change byte length.
    display_names: String,
    entries: Vec<Entry>,
}

pub struct Entry {
    name_start: u32,
    name_len: u16,
    display_start: u32,
    display_len: u16,
    parent: u32,
    is_dir: bool,
    size: u64,
    modifies: u64,
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
        }
    }

    // Appends an entry and returns its ID. `parent` is `u32::MAX` for a root.
    pub fn push(
        &mut self,
        name: &str,
        parent: u32,
        is_dir: bool,
        size: u64,
        modified: u64,
    ) -> Result<u32, NameTooLong> {
        let lowercase = name.to_lowercase();
        if lowercase.len() > u16::MAX as usize || name.len() > u16::MAX as usize {
            return Err(NameTooLong);
        }
        let id = self.entries.len() as u32;
        self.entries.push(Entry {
            name_start: self.names.len() as u32,
            name_len: lowercase.len() as u16,
            display_start: self.display_names.len() as u32,
            display_len: name.len() as u16,
            parent,
            is_dir,
            size,
            modifies: modified,
        });
        self.names.push_str(&lowercase);
        self.display_names.push_str(name);
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
        while curr != u32::MAX {
            parts.push(self.name(curr)?);
            curr = self.entries[curr as usize].parent;
        }

        Some(parts.iter().rev().collect())
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
    fn push_assigns_sequential_ids() {
        let mut index = Index::new();
        assert_eq!(index.push("c", u32::MAX, true, 0, 0), Ok(0));
        assert_eq!(index.push("docs", 0, true, 0, 0), Ok(1));
        assert_eq!(index.push("a.txt", 1, false, 10, 0), Ok(2));
        assert_eq!(
            index.full_path(2),
            Some(Path::new("c").join("docs").join("a.txt"))
        );
    }

    #[test]
    fn name_is_lowercased() {
        let mut index = Index::new();
        let id = index.push("ReadMe.MD", u32::MAX, false, 0, 0).unwrap();
        assert_eq!(index.name(id), Some("readme.md"));
    }

    #[test]
    fn display_name_keeps_case() {
        let mut index = Index::new();
        let id = index.push("ReadMe.MD", u32::MAX, false, 0, 0).unwrap();
        assert_eq!(index.display_name(id), Some("ReadMe.MD"));
    }

    // 'ẞ' is 3 bytes but lowercases to 2-byte 'ß'; 'İ' is 2 bytes but lowercases
    // to 3 bytes ("i" + combining dot). Both buffers must stay in sync, and the
    // entry pushed afterwards must not be shifted.
    #[test]
    fn lowercasing_that_changes_length() {
        let mut index = Index::new();
        let a = index.push("ẞig", u32::MAX, false, 0, 0).unwrap();
        let b = index.push("İx", u32::MAX, false, 0, 0).unwrap();
        let c = index.push("plain", u32::MAX, false, 0, 0).unwrap();
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
        index.push("ok", u32::MAX, false, 0, 0).unwrap();
        let long = "x".repeat(u16::MAX as usize + 1);
        assert_eq!(index.push(&long, u32::MAX, false, 0, 0), Err(NameTooLong));
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
        assert_eq!(index.push(&long, u32::MAX, false, 0, 0), Err(NameTooLong));
    }

    #[test]
    fn name_returns_each_entry() {
        let mut index = Index::new();
        let a = index.push("ab", u32::MAX, false, 0, 0).unwrap();
        let b = index.push("c", u32::MAX, false, 0, 0).unwrap();
        let c = index.push("def", u32::MAX, false, 0, 0).unwrap();
        assert_eq!(index.name(a), Some("ab"));
        assert_eq!(index.name(b), Some("c"));
        assert_eq!(index.name(c), Some("def"));
    }

    #[test]
    fn name_out_of_range_is_none() {
        let mut index = Index::new();
        index.push("ab", u32::MAX, false, 0, 0).unwrap();
        assert_eq!(index.name(1), None);
        assert_eq!(index.name(u32::MAX - 1), None);
    }

    // Offsets are byte offsets, so multi-byte characters must not shift later names.
    #[test]
    fn name_handles_multibyte() {
        let mut index = Index::new();
        let a = index.push("café.txt", u32::MAX, false, 0, 0).unwrap();
        let b = index.push("next", u32::MAX, false, 0, 0).unwrap();
        assert_eq!(index.name(a), Some("café.txt"));
        assert_eq!(index.name(b), Some("next"));
    }

    #[test]
    fn full_path_of_root_is_its_name() {
        let mut index = Index::new();
        let root = index.push("c", u32::MAX, true, 0, 0).unwrap();
        assert_eq!(index.full_path(root), Some(Path::new("c").to_path_buf()));
    }

    #[test]
    fn full_path_out_of_range_is_none() {
        let mut index = Index::new();
        index.push("c", u32::MAX, true, 0, 0).unwrap();
        assert_eq!(index.full_path(1), None);
    }

    #[test]
    fn full_path_with_missing_parent_is_none() {
        let mut index = Index::new();
        index.push("c", u32::MAX, true, 0, 0).unwrap();
        let orphan = index.push("a.txt", 7, false, 0, 0).unwrap();
        assert_eq!(index.full_path(orphan), None);
    }

    #[test]
    fn full_path_walks_parents() {
        let mut index = Index::new();
        let root = index.push("c", u32::MAX, true, 0, 0).unwrap();
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
        let root = index.push("c", u32::MAX, true, 0, 0).unwrap();
        let a = index.push("a.txt", root, false, 0, 0).unwrap();
        let b = index.push("b.txt", root, false, 0, 0).unwrap();
        assert_eq!(index.full_path(a), Some(Path::new("c").join("a.txt")));
        assert_eq!(index.full_path(b), Some(Path::new("c").join("b.txt")));
    }
}
