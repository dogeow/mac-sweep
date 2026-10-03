//! Bounded, in-memory directory measurements used by lazy directory browsing.
//! This module performs no filesystem reads and does not provide cleanup targets.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::path::Path;

#[derive(Clone, Debug)]
pub struct DirectorySummary {
    pub id: String,
    pub path: String,
    pub name: String,
    pub bytes: u64,
    pub files: u64,
    pub dirs: u64,
    pub has_children: bool,
    pub partial: bool,
    pub device: u64,
    pub inode: u64,
    pub size_known: bool,
}

#[derive(Clone, Debug)]
pub struct DirectorySummaryIndex {
    pub source_analysis_id: String,
    pub root: String,
    pub device: u64,
    pub measured_at: u64,
    pub entries: HashMap<String, DirectorySummary>,
}

impl DirectorySummaryIndex {
    /// Return a measurement only for an exact path retained by the collector.
    pub fn lookup(&self, path: &str) -> Option<&DirectorySummary> {
        self.entries.get(path)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Check path components, so `/root-old` is outside the scope `/root`.
    /// Paths are expected to have been validated by the caller; no filesystem
    /// access or canonicalization is performed here.
    pub fn contains_scope(&self, path: &Path) -> bool {
        path.starts_with(Path::new(&self.root))
    }
}

/// Heap ordering puts the least desirable retained entry first: smaller sizes,
/// then longer paths, then lexically later paths. This makes admission bounded
/// while preserving the same result regardless of directory traversal order.
#[derive(Debug)]
struct RetainedSummary(DirectorySummary);

impl PartialEq for RetainedSummary {
    fn eq(&self, other: &Self) -> bool {
        self.0.bytes == other.0.bytes && self.0.path == other.0.path
    }
}

impl Eq for RetainedSummary {}

impl PartialOrd for RetainedSummary {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RetainedSummary {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .0
            .bytes
            .cmp(&self.0.bytes)
            .then_with(|| self.0.path.len().cmp(&other.0.path.len()))
            .then_with(|| self.0.path.cmp(&other.0.path))
    }
}

#[derive(Debug)]
pub struct BoundedDirectorySummaries {
    capacity: usize,
    retained: BinaryHeap<RetainedSummary>,
}

impl BoundedDirectorySummaries {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            retained: BinaryHeap::new(),
        }
    }

    /// Cheap admission check before allocating a summary's strings. The final
    /// decision is repeated by `consider`, which also works without this check.
    /// For a non-UTF-8 path tied by size, conservatively allow consideration.
    pub fn should_consider(&self, bytes: u64, path: &Path) -> bool {
        if self.capacity == 0 {
            return false;
        }
        if self.retained.len() < self.capacity {
            return true;
        }
        let worst = &self.retained.peek().expect("a full heap is nonempty").0;
        match bytes.cmp(&worst.bytes) {
            Ordering::Greater => true,
            Ordering::Less => false,
            Ordering::Equal => path
                .to_str()
                .map(|path| {
                    path.len() < worst.path.len()
                        || (path.len() == worst.path.len() && path < worst.path.as_str())
                })
                .unwrap_or(true),
        }
    }

    /// Retain at most `capacity` measurements, including when larger directories
    /// are discovered late. The caller supplies one measurement per exact path.
    pub fn consider(&mut self, summary: DirectorySummary) {
        if self.capacity == 0 {
            return;
        }
        let candidate = RetainedSummary(summary);
        if self.retained.len() < self.capacity {
            self.retained.push(candidate);
        } else if candidate < *self.retained.peek().expect("a full heap is nonempty") {
            *self.retained.peek_mut().expect("a full heap is nonempty") = candidate;
        }
    }

    pub fn finish(
        self,
        source_analysis_id: String,
        root: String,
        device: u64,
        measured_at: u64,
    ) -> DirectorySummaryIndex {
        // Consume the bounded heap. The completed index has no unbounded history
        // of paths rejected earlier in the walk.
        let entries = self
            .retained
            .into_iter()
            .map(|entry| (entry.0.path.clone(), entry.0))
            .collect();
        DirectorySummaryIndex {
            source_analysis_id,
            root,
            device,
            measured_at,
            entries,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(path: &str, bytes: u64) -> DirectorySummary {
        DirectorySummary {
            id: format!("id:{path}"),
            path: path.to_owned(),
            name: Path::new(path)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            bytes,
            files: 2,
            dirs: 1,
            has_children: true,
            partial: false,
            device: 7,
            inode: 42,
            size_known: true,
        }
    }

    fn finish(collector: BoundedDirectorySummaries) -> DirectorySummaryIndex {
        collector.finish("analysis-1".to_owned(), "/root".to_owned(), 7, 1234)
    }

    fn keys(index: &DirectorySummaryIndex) -> Vec<&str> {
        let mut result: Vec<_> = index.entries.keys().map(String::as_str).collect();
        result.sort_unstable();
        result
    }

    #[test]
    fn late_large_directories_replace_early_small_ones_with_hard_capacity() {
        let mut collector = BoundedDirectorySummaries::new(3);
        for item in 0..50_000 {
            let path = format!("/root/Go/{item}");
            if collector.should_consider(1, Path::new(&path)) {
                collector.consider(summary(&path, 1));
            }
            assert!(collector.retained.len() <= 3);
        }
        for (path, bytes) in [
            ("/root/Library", 1_000_000),
            ("/root/Library/Caches", 500_000),
            ("/root", 2_000_000),
        ] {
            assert!(collector.should_consider(bytes, Path::new(path)));
            collector.consider(summary(path, bytes));
            assert_eq!(collector.retained.len(), 3);
        }
        let index = finish(collector);
        assert_eq!(index.len(), 3);
        assert_eq!(
            keys(&index),
            ["/root", "/root/Library", "/root/Library/Caches"]
        );
        assert!(index.lookup("/root/Go/0").is_none());
    }

    #[test]
    fn ties_prefer_shorter_paths_then_lexical_order_independent_of_walk_order() {
        let paths = ["/root/long-name", "/root/z", "/root/b", "/root", "/root/a"];
        let mut forwards = BoundedDirectorySummaries::new(3);
        let mut backwards = BoundedDirectorySummaries::new(3);
        for path in paths {
            forwards.consider(summary(path, 0));
        }
        for path in paths.into_iter().rev() {
            backwards.consider(summary(path, 0));
        }
        let forwards = finish(forwards);
        let backwards = finish(backwards);
        assert_eq!(keys(&forwards), ["/root", "/root/a", "/root/b"]);
        assert_eq!(keys(&forwards), keys(&backwards));
    }

    #[test]
    fn admission_check_respects_capacity_size_and_path_ties() {
        let mut collector = BoundedDirectorySummaries::new(1);
        assert!(collector.should_consider(0, Path::new("/root/long-name")));
        collector.consider(summary("/root/long-name", 10));
        assert!(!collector.should_consider(9, Path::new("/root")));
        assert!(collector.should_consider(11, Path::new("/root/even-longer-name")));
        assert!(collector.should_consider(10, Path::new("/root")));
        collector.consider(summary("/root/b", 10));
        assert!(collector.should_consider(10, Path::new("/root/a")));
        assert!(!collector.should_consider(10, Path::new("/root/c")));
        assert!(!collector.should_consider(10, Path::new("/root/b")));
        collector.consider(summary("/root/c", 10));
        assert_eq!(keys(&finish(collector)), ["/root/b"]);
    }

    #[test]
    fn zero_capacity_keeps_nothing() {
        let mut collector = BoundedDirectorySummaries::new(0);
        assert!(!collector.should_consider(u64::MAX, Path::new("/root")));
        collector.consider(summary("/root", u64::MAX));
        let index = finish(collector);
        assert_eq!(index.len(), 0);
        assert!(index.lookup("/root").is_none());
    }

    #[test]
    fn index_preserves_measurement_identity_and_scope_uses_path_components() {
        let mut collector = BoundedDirectorySummaries::new(3);
        let mut measured = summary("/root/Library", 100);
        measured.partial = true;
        measured.size_known = false;
        collector.consider(measured);
        let index = finish(collector);
        assert_eq!(index.source_analysis_id, "analysis-1");
        assert_eq!(index.root, "/root");
        assert_eq!(index.device, 7);
        assert_eq!(index.measured_at, 1234);
        let measured = index.lookup("/root/Library").unwrap();
        assert_eq!(measured.id, "id:/root/Library");
        assert_eq!(measured.name, "Library");
        assert_eq!(measured.bytes, 100);
        assert_eq!(measured.files, 2);
        assert_eq!(measured.dirs, 1);
        assert!(measured.has_children);
        assert_eq!(measured.device, 7);
        assert_eq!(measured.inode, 42);
        assert!(measured.partial);
        assert!(!measured.size_known);
        assert!(index.contains_scope(Path::new("/root")));
        assert!(index.contains_scope(Path::new("/root/Library/Caches")));
        assert!(!index.contains_scope(Path::new("/root-old")));
        assert!(!index.contains_scope(Path::new("/root-old/Library")));
        assert!(!index.contains_scope(Path::new("root/Library")));
        assert!(index.lookup("/root").is_none());
        assert!(index.lookup("/root/Library/Caches").is_none());
        assert!(index.lookup("/root/Library/").is_none());
    }
}
