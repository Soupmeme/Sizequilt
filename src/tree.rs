//! The scanned tree: columns indexed by node id, with the children of a folder
//! held as a contiguous sorted run, so a folder of any depth costs no recursion
//! and no per-node allocation (about 75-90 bytes an entry, names included).

use std::collections::HashMap;
use std::path::PathBuf;

use rayon::prelude::*;
use tm_walk::{FLAG_REFUSED_DIR, KIND_DIR, KIND_SYMLINK, WalkOutput};

use crate::category::{self, CATEGORIES, Category};

/// Which size the map and the lists rank by.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Metric {
    /// Bytes actually taken on disk: what freeing space frees.
    Allocated,
    /// The file's length: what Explorer calls "Size".
    Logical,
}

/// How much one extension holds.
#[derive(Clone, Debug)]
pub struct ExtStat {
    pub ext: String,
    pub category: Category,
    pub logical: u64,
    pub allocated: u64,
    pub files: u64,
}

/// Total held by one category.
#[derive(Clone, Copy, Debug, Default)]
pub struct CategoryStat {
    pub logical: u64,
    pub allocated: u64,
    pub files: u64,
}

/// The scan's facts that never change after it is built.
#[derive(Clone, Debug)]
pub struct ScanFacts {
    pub engine: String,
    pub note: Option<String>,
    pub wall_ms: f64,
    pub build_ms: f64,
    pub entries: u64,
    pub dirs: u64,
    pub denied_entries: u64,
    pub refused_dirs: u64,
}

/// The scan. Node 0 is the root; `parent[i] < i` for every other node.
pub struct Tree {
    pub root_path: PathBuf,
    parent: Vec<u32>,
    name_off: Vec<u32>,
    names: Vec<u8>,
    kind: Vec<u8>,
    flags: Vec<u8>,
    category: Vec<u8>,
    /// A file's own bytes, or a folder's cumulative bytes.
    logical: Vec<u64>,
    allocated: Vec<u64>,
    /// Files at or under a node.
    files: Vec<u32>,
    mtime_ms: Vec<f64>,
    /// Where a folder's children start in `child`.
    child_start: Vec<u32>,
    child_len: Vec<u32>,
    child: Vec<u32>,
    metric: Metric,
    pub facts: ScanFacts,
    pub extensions: Vec<ExtStat>,
    pub categories: [CategoryStat; CATEGORIES.len()],
}

impl Tree {
    /// Builds the tree from a finished walk or MFT read.
    pub fn build(
        root_path: PathBuf,
        out: WalkOutput,
        engine: String,
        note: Option<String>,
        metric: Metric,
    ) -> Self {
        let started = std::time::Instant::now();
        let n = out.len();
        let mut logical: Vec<u64> = out.size.iter().map(|v| *v as u64).collect();
        let mut allocated: Vec<u64> = out.alloc_bytes.iter().map(|v| *v as u64).collect();

        // A hard-linked file's bytes count once, at its first name.
        let mut seen = std::collections::HashSet::new();
        for link in &out.hardlinks {
            if !seen.insert(link.family) {
                logical[link.node as usize] = 0;
                allocated[link.node as usize] = 0;
            }
        }

        let category: Vec<u8> = (0..n)
            .into_par_iter()
            .map(|i| {
                if out.kind[i] == KIND_DIR {
                    Category::Other as u8
                } else {
                    category::of_name(out.name(i).unwrap_or(&[])) as u8
                }
            })
            .collect();

        let mut files: Vec<u32> = out.kind.iter().map(|k| u32::from(*k != KIND_DIR)).collect();

        // Children in contiguous runs, by counting.
        let mut child_len = vec![0_u32; n];
        for &p in out.parent.iter().skip(1) {
            child_len[p as usize] += 1;
        }
        let mut child_start = vec![0_u32; n];
        let mut next = 0_u32;
        for i in 0..n {
            child_start[i] = next;
            next += child_len[i];
        }
        let mut cursor = child_start.clone();
        let mut child = vec![0_u32; n.saturating_sub(1)];
        for (i, &p) in out.parent.iter().enumerate().skip(1) {
            let slot = &mut cursor[p as usize];
            child[*slot as usize] = i as u32;
            *slot += 1;
        }

        // One reverse pass folds every size into its folder: parents come first.
        for i in (1..n).rev() {
            let p = out.parent[i] as usize;
            logical[p] += logical[i];
            allocated[p] += allocated[i];
            files[p] += files[i];
        }

        let dirs = out.kind.iter().filter(|k| **k == KIND_DIR).count() as u64;
        let refused_dirs = out
            .flags
            .iter()
            .filter(|f| **f & FLAG_REFUSED_DIR != 0)
            .count() as u64;

        let WalkOutput {
            parent,
            name_off,
            names,
            kind,
            flags,
            mtime_ms,
            stats,
            ..
        } = out;

        let mut tree = Self {
            root_path,
            parent,
            name_off,
            names,
            kind,
            flags,
            category,
            logical,
            allocated,
            files,
            mtime_ms,
            child_start,
            child_len,
            child,
            metric,
            facts: ScanFacts {
                engine,
                note,
                wall_ms: stats.wall_ms,
                build_ms: 0.0,
                entries: stats.entries,
                dirs,
                denied_entries: stats.denied_entries,
                refused_dirs,
            },
            extensions: Vec::new(),
            categories: [CategoryStat::default(); CATEGORIES.len()],
        };
        tree.sort_children();
        tree.recount_types();
        tree.facts.build_ms = started.elapsed().as_secs_f64() * 1000.0;
        tree
    }

    pub fn len(&self) -> usize {
        self.parent.len()
    }

    pub fn is_empty(&self) -> bool {
        self.parent.is_empty()
    }

    pub fn metric(&self) -> Metric {
        self.metric
    }

    /// Changes the metric, re-sorting every folder's children for it.
    pub fn set_metric(&mut self, metric: Metric) {
        if self.metric != metric {
            self.metric = metric;
            self.sort_children();
        }
    }

    /// A node's size under the current metric.
    #[inline]
    pub fn size(&self, node: u32) -> u64 {
        match self.metric {
            Metric::Allocated => self.allocated[node as usize],
            Metric::Logical => self.logical[node as usize],
        }
    }

    pub fn logical(&self, node: u32) -> u64 {
        self.logical[node as usize]
    }

    pub fn allocated(&self, node: u32) -> u64 {
        self.allocated[node as usize]
    }

    pub fn files_under(&self, node: u32) -> u32 {
        self.files[node as usize]
    }

    #[inline]
    pub fn is_dir(&self, node: u32) -> bool {
        self.kind[node as usize] == KIND_DIR
    }

    /// A symbolic link or junction: the walk recorded it and never followed it.
    pub fn is_link(&self, node: u32) -> bool {
        self.kind[node as usize] == KIND_SYMLINK
    }

    pub fn is_refused(&self, node: u32) -> bool {
        self.flags[node as usize] & FLAG_REFUSED_DIR != 0
    }

    #[inline]
    pub fn category(&self, node: u32) -> u8 {
        self.category[node as usize]
    }

    /// The category of the largest file at or under `node`: children come
    /// biggest first, so following the first child finds it without a scan.
    pub fn dominant_category(&self, node: u32) -> Option<u8> {
        let mut at = node;
        while self.is_dir(at) {
            at = *self.children(at).first()?;
        }
        Some(self.category(at))
    }

    pub fn parent(&self, node: u32) -> Option<u32> {
        (node != 0).then(|| self.parent[node as usize])
    }

    pub fn mtime_ms(&self, node: u32) -> f64 {
        self.mtime_ms[node as usize]
    }

    #[inline]
    pub fn name_bytes(&self, node: u32) -> &[u8] {
        let i = node as usize;
        &self.names[self.name_off[i] as usize..self.name_off[i + 1] as usize]
    }

    /// A node's name; the root's is its whole path.
    pub fn name(&self, node: u32) -> String {
        if node == 0 {
            return self.root_path.to_string_lossy().into_owned();
        }
        String::from_utf8_lossy(self.name_bytes(node)).into_owned()
    }

    /// A folder's children, biggest first under the current metric.
    #[inline]
    pub fn children(&self, node: u32) -> &[u32] {
        let start = self.child_start[node as usize] as usize;
        &self.child[start..start + self.child_len[node as usize] as usize]
    }

    /// The chain from the root down to `node`, both included.
    pub fn chain(&self, node: u32) -> Vec<u32> {
        let mut chain = vec![node];
        let mut at = node;
        while let Some(p) = self.parent(at) {
            chain.push(p);
            at = p;
        }
        chain.reverse();
        chain
    }

    /// Whether `node` is `ancestor` or below it.
    pub fn is_within(&self, node: u32, ancestor: u32) -> bool {
        let mut at = node;
        loop {
            if at == ancestor {
                return true;
            }
            match self.parent(at) {
                // Ids only rise going down, so climbing past `ancestor` ends it.
                Some(p) if p >= ancestor => at = p,
                _ => return false,
            }
        }
    }

    /// The full path of a node, rebuilt on demand.
    pub fn path(&self, node: u32) -> PathBuf {
        let mut path = self.root_path.clone();
        for link in self.chain(node).into_iter().skip(1) {
            path.push(String::from_utf8_lossy(self.name_bytes(link)).as_ref());
        }
        path
    }

    /// The `limit` largest files under the current metric, biggest first.
    pub fn largest_files(&self, limit: usize) -> Vec<u32> {
        let mut files: Vec<u32> = (1..self.len() as u32)
            .into_par_iter()
            .filter(|&i| !self.is_dir(i) && self.size(i) > 0 && !self.is_removed(i))
            .collect();
        if files.len() > limit {
            files.select_nth_unstable_by(limit - 1, |a, b| self.size(*b).cmp(&self.size(*a)));
            files.truncate(limit);
        }
        files.par_sort_unstable_by(|a, b| self.size(*b).cmp(&self.size(*a)));
        files
    }

    /// Whether a node was taken out of the tree by [`Tree::remove`].
    pub fn is_removed(&self, node: u32) -> bool {
        node != 0 && self.flags[node as usize] & FLAG_REMOVED != 0
    }

    /// Takes a node that has left the disk out of the tree: every ancestor
    /// loses its sizes and the folder stays sorted.
    pub fn remove(&mut self, node: u32) {
        let Some(parent) = self.parent(node) else {
            return;
        };
        let logical = self.logical[node as usize];
        let allocated = self.allocated[node as usize];
        let files = self.files[node as usize];

        // Everything below goes too, for the lists that scan all nodes.
        let mut stack = vec![node];
        while let Some(at) = stack.pop() {
            self.flags[at as usize] |= FLAG_REMOVED;
            stack.extend_from_slice(self.children(at));
        }

        let start = self.child_start[parent as usize] as usize;
        let len = self.child_len[parent as usize] as usize;
        let run = &mut self.child[start..start + len];
        if let Some(at) = run.iter().position(|c| *c == node) {
            run.copy_within(at + 1.., at);
            self.child_len[parent as usize] -= 1;
        }

        let mut at = Some(parent);
        while let Some(link) = at {
            let i = link as usize;
            self.logical[i] -= logical;
            self.allocated[i] -= allocated;
            self.files[i] -= files;
            at = self.parent(link);
        }
        // Ancestors shrank, so each one's place among its siblings may have moved.
        let mut at = self.parent(parent);
        let mut below = parent;
        while let Some(link) = at {
            self.resort_one(link, below);
            below = link;
            at = self.parent(link);
        }
        self.recount_types();
    }

    /// Moves `moved` to its right place in the sorted run of `folder`.
    fn resort_one(&mut self, folder: u32, moved: u32) {
        let start = self.child_start[folder as usize] as usize;
        let len = self.child_len[folder as usize] as usize;
        let mut run = std::mem::take(&mut self.child);
        {
            let slice = &mut run[start..start + len];
            if let Some(mut at) = slice.iter().position(|c| *c == moved) {
                let size = self.size(moved);
                while at + 1 < slice.len() && self.size(slice[at + 1]) > size {
                    slice.swap(at, at + 1);
                    at += 1;
                }
            }
        }
        self.child = run;
    }

    fn sort_children(&mut self) {
        // Hand each folder's run to rayon as its own slice.
        let mut runs: Vec<&mut [u32]> = Vec::new();
        let mut rest = self.child.as_mut_slice();
        let mut consumed = 0_usize;
        for i in 0..self.child_start.len() {
            let start = self.child_start[i] as usize;
            let len = self.child_len[i] as usize;
            let (_, tail) = rest.split_at_mut(start - consumed);
            let (run, tail) = tail.split_at_mut(len);
            rest = tail;
            consumed = start + len;
            if len > 1 {
                runs.push(run);
            }
        }
        let sizes = match self.metric {
            Metric::Allocated => &self.allocated,
            Metric::Logical => &self.logical,
        };
        runs.par_iter_mut().for_each(|run| {
            run.sort_unstable_by(|a, b| sizes[*b as usize].cmp(&sizes[*a as usize]));
        });
    }

    fn recount_types(&mut self) {
        let table = (1..self.len() as u32)
            .into_par_iter()
            .filter(|&i| !self.is_dir(i) && !self.is_removed(i))
            .fold(HashMap::<Box<[u8]>, ExtStat>::new, |mut map, i| {
                let name = self.name_bytes(i);
                let ext = category::extension(name).unwrap_or(&[]);
                let key = ext.to_ascii_lowercase().into_boxed_slice();
                let entry = map.entry(key).or_insert_with(|| ExtStat {
                    ext: String::from_utf8_lossy(&ext.to_ascii_lowercase()).into_owned(),
                    category: category::of_name(name),
                    logical: 0,
                    allocated: 0,
                    files: 0,
                });
                entry.logical += self.logical[i as usize];
                entry.allocated += self.allocated[i as usize];
                entry.files += 1;
                map
            })
            .reduce(HashMap::new, |mut a, b| {
                for (key, stat) in b {
                    a.entry(key)
                        .and_modify(|e| {
                            e.logical += stat.logical;
                            e.allocated += stat.allocated;
                            e.files += stat.files;
                        })
                        .or_insert(stat);
                }
                a
            });
        let mut categories = [CategoryStat::default(); CATEGORIES.len()];
        for stat in table.values() {
            let slot = &mut categories[stat.category as usize];
            slot.logical += stat.logical;
            slot.allocated += stat.allocated;
            slot.files += stat.files;
        }
        self.categories = categories;
        self.extensions = table.into_values().collect();
        self.sort_extensions();
    }

    /// Orders the type table by the current metric.
    pub fn sort_extensions(&mut self) {
        let metric = self.metric;
        self.extensions.sort_unstable_by_key(|e| {
            std::cmp::Reverse(match metric {
                Metric::Allocated => e.allocated,
                Metric::Logical => e.logical,
            })
        });
    }

    /// The heap this tree holds, for the status bar.
    pub fn heap_bytes(&self) -> u64 {
        let n = self.len() as u64;
        n * (4 + 4 + 1 + 1 + 1 + 8 + 8 + 4 + 8 + 4 + 4)
            + self.names.len() as u64
            + self.child.len() as u64 * 4
    }
}

/// Set on a node that has left the disk; clear of the walk's own flag bits.
const FLAG_REMOVED: u8 = 0x80;

#[cfg(test)]
pub(crate) mod test_tree {
    use tm_walk::{FastPath, WalkOutput, WalkStats};

    use super::*;

    /// `(parent, name, is_dir, size)` rows, the root first.
    pub(crate) fn output(rows: &[(u32, &str, bool, u64)]) -> WalkOutput {
        let mut names = Vec::new();
        let mut name_off = vec![0_u32];
        for row in rows {
            names.extend_from_slice(row.1.as_bytes());
            name_off.push(names.len() as u32);
        }
        WalkOutput {
            parent: rows.iter().map(|r| r.0).collect(),
            name_off,
            names,
            kind: rows.iter().map(|r| u8::from(r.2)).collect(),
            flags: vec![0; rows.len()],
            size: rows.iter().map(|r| r.3 as f64).collect(),
            alloc_bytes: rows.iter().map(|r| (r.3 / 2) as f64).collect(),
            mtime_ms: vec![f64::NAN; rows.len()],
            atime_ms: vec![f64::NAN; rows.len()],
            hardlinks: Vec::new(),
            refusals: Vec::new(),
            stats: WalkStats {
                dirs_listed: 1,
                entries: rows.len() as u64 - 1,
                wall_ms: 1.0,
                cpu_seconds: 0.0,
                fast_path: FastPath::ExtdDirInfo,
                workers_peak: 1,
                climb_steps: 0,
                denied_entries: 0,
                unreadable_entries: 0,
                dataless: 0,
            },
        }
    }

    pub(crate) fn tree(rows: &[(u32, &str, bool, u64)]) -> Tree {
        Tree::build(
            PathBuf::from("X:\\"),
            output(rows),
            "test".to_owned(),
            None,
            Metric::Logical,
        )
    }

    pub(crate) fn sample() -> Tree {
        tree(&[
            (0, "root", true, 0),
            (0, "small", true, 0),
            (0, "big", true, 0),
            (0, "loose.txt", false, 5),
            (1, "a.mp4", false, 10),
            (2, "b.mkv", false, 100),
            (2, "deep", true, 0),
            (6, "c.zip", false, 50),
        ])
    }

    #[test]
    fn folders_hold_the_sum_of_what_is_below_them() {
        let t = sample();
        assert_eq!(t.logical(0), 165);
        assert_eq!(t.logical(2), 150);
        assert_eq!(t.files_under(0), 4);
        assert_eq!(t.allocated(2), 75);
    }

    #[test]
    fn children_come_biggest_first() {
        let t = sample();
        assert_eq!(t.children(0), &[2, 1, 3]);
    }

    #[test]
    fn the_metric_reorders_children() {
        let mut t = tree(&[
            (0, "root", true, 0),
            (0, "one", false, 3),
            (0, "two", false, 2),
        ]);
        assert_eq!(t.children(0), &[1, 2]);
        t.alloc_swap_for_test();
        t.set_metric(Metric::Allocated);
        assert_eq!(t.children(0), &[2, 1]);
    }

    #[test]
    fn a_folder_is_dominated_by_its_largest_file() {
        let t = sample();
        assert_eq!(t.dominant_category(2), Some(Category::Video as u8));
        assert_eq!(t.dominant_category(6), Some(Category::Archive as u8));
        let empty = tree(&[(0, "root", true, 0), (0, "empty", true, 0)]);
        assert_eq!(empty.dominant_category(1), None);
    }

    #[test]
    fn a_hard_link_family_counts_once() {
        let mut out = output(&[
            (0, "root", true, 0),
            (0, "a", false, 100),
            (0, "b", false, 100),
        ]);
        out.hardlinks = vec![
            tm_walk::HardlinkRef { node: 1, family: 7 },
            tm_walk::HardlinkRef { node: 2, family: 7 },
        ];
        let t = Tree::build(
            PathBuf::from("X:\\"),
            out,
            "test".to_owned(),
            None,
            Metric::Logical,
        );
        assert_eq!(t.logical(0), 100);
    }

    #[test]
    fn paths_are_rebuilt_from_the_chain() {
        let t = sample();
        assert_eq!(t.path(7), PathBuf::from("X:\\big\\deep\\c.zip"));
        assert_eq!(t.path(0), PathBuf::from("X:\\"));
    }

    #[test]
    fn within_climbs_to_the_ancestor() {
        let t = sample();
        assert!(t.is_within(7, 2));
        assert!(t.is_within(7, 7));
        assert!(!t.is_within(7, 1));
        assert!(t.is_within(7, 0));
    }

    #[test]
    fn removing_a_folder_takes_its_bytes_out_of_every_ancestor() {
        let mut t = sample();
        t.remove(6);
        assert_eq!(t.logical(0), 115);
        assert_eq!(t.logical(2), 100);
        assert_eq!(t.files_under(0), 3);
        assert_eq!(t.children(2), &[5]);
        assert!(t.is_removed(7));
        assert_eq!(t.largest_files(10), vec![5, 4, 3]);
    }

    #[test]
    fn removal_keeps_ancestors_sorted() {
        let mut t = sample();
        // `big` falls below `small` once 145 of its 150 are gone.
        t.remove(5);
        t.remove(7);
        assert_eq!(t.logical(2), 0);
        assert_eq!(t.children(0), &[1, 3, 2]);
    }

    #[test]
    fn types_add_up_by_extension() {
        let t = sample();
        let mp4 = t.extensions.iter().find(|e| e.ext == "mp4").unwrap();
        assert_eq!(mp4.logical, 10);
        assert_eq!(mp4.category, Category::Video);
        assert_eq!(t.categories[Category::Video as usize].logical, 110);
    }

    impl Tree {
        fn alloc_swap_for_test(&mut self) {
            self.allocated[1] = 1;
            self.allocated[2] = 2;
        }
    }
}
