struct Index {
    names: String,
    entries: Vec<Entry>,
}

pub struct Entry {
    name_start: u32,
    name_len: u16,
    parent: u32,
    is_dir: bool,
    size: u64,
    modifies: u64,
}

impl Index {
    pub fn name(&self, id: u32) -> &str {
        let e = &self.entries[id as usize];
        let start = e.name_start as usize;
        &self.names[start..(start + e.name_len as usize)]
    }

    pub fn full_path(&self, id: u32) -> std::path::PathBuf {
        let mut parts = Vec::new();
        let mut curr = id;
        while curr != u32::MAX {
            parts.push(self.name(curr));
            curr = self.entries[curr as usize].parent;
        }

        parts. iter().rev().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn build(items: &[(&str, u32)]) -> Index {
        let mut index = Index {
            names: String::new(),
            entries: Vec::new(),
        };
        for &(name, parent) in items {
            index.entries.push(Entry {
                name_start: index.names.len() as u32,
                name_len: name.len() as u16,
                parent,
                is_dir: false,
                size: 0,
                modifies: 0,
            });
            index.names.push_str(name);
        }
        index
    }

    #[test]
    fn name_returns_each_entry() {
        let index = build(&[("ab", u32::MAX), ("c", u32::MAX), ("def", u32::MAX)]);
        assert_eq!(index.name(0), "ab");
        assert_eq!(index.name(1), "c");
        assert_eq!(index.name(2), "def");
    }

    // Offsets are byte offsets, so multi-byte characters must not shift later names.
    #[test]
    fn name_handles_multibyte() {
        let index = build(&[("café.txt", u32::MAX), ("next", u32::MAX)]);
        assert_eq!(index.name(0), "café.txt");
        assert_eq!(index.name(1), "next");
    }

    #[test]
    fn full_path_of_root_is_its_name() {
        let index = build(&[("c", u32::MAX)]);
        assert_eq!(index.full_path(0), Path::new("c"));
    }

    #[test]
    fn full_path_walks_parents() {
        let index = build(&[("c", u32::MAX), ("docs", 0), ("a.txt", 1)]);
        assert_eq!(
            index.full_path(2),
            Path::new("c").join("docs").join("a.txt")
        );
    }

    #[test]
    fn siblings_share_parent() {
        let index = build(&[("c", u32::MAX), ("a.txt", 0), ("b.txt", 0)]);
        assert_eq!(index.full_path(1), Path::new("c").join("a.txt"));
        assert_eq!(index.full_path(2), Path::new("c").join("b.txt"));
    }
}
