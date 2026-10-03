//! Read-only directory sizes. This module never supplies cleanup candidates.
//! Allocated blocks include directory/link storage; hard-linked files are counted
//! once per directory entry, and APFS shared extents cannot be deduplicated here.

use serde::Serialize;
use std::cmp::Ordering as CmpOrdering;
use std::collections::BinaryHeap;
use std::ffi::CString;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DISPLAY_DEPTH: usize = 3;
const DISPLAY_CHILDREN: usize = 300;
const DISPLAY_NODES: u64 = 15_000;
const MAX_DEPTH: usize = 256;
const MAX_VISITS: u64 = 2_000_000;
const MAX_TIME: Duration = Duration::from_secs(300);
static NEXT_ANALYSIS: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryNode {
    pub id: String,
    pub path: String,
    pub name: String,
    pub kind: String,
    pub bytes: u64,
    pub files: u64,
    pub dirs: u64,
    pub children: Vec<DirectoryNode>,
    pub has_children: bool,
    pub partial: bool,
    pub omitted_children: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisReport {
    pub analysis_id: String,
    pub root: DirectoryNode,
    pub duration_ms: u64,
    pub scanned_files: u64,
    pub warnings: Vec<String>,
    pub cancelled: bool,
    /// Capacity of the filesystem containing the chosen root, not directory size.
    pub total_bytes: u64,
    pub available_bytes: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisProgress {
    pub current_path: String,
    pub scanned_files: u64,
    pub bytes_found: u64,
}

#[derive(Default)]
struct Totals {
    bytes: u64,
    files: u64,
    dirs: u64,
    partial: bool,
    children: u64,
}

struct Measured {
    totals: Totals,
    displayed: Option<Displayed>,
}

struct Displayed {
    node: DirectoryNode,
    nodes: u64,
}

// A bounded heap with the smallest child at the top, retaining the largest rows.
impl PartialEq for Displayed {
    fn eq(&self, other: &Self) -> bool {
        self.node.bytes == other.node.bytes && self.node.path == other.node.path
    }
}
impl Eq for Displayed {}
impl PartialOrd for Displayed {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}
impl Ord for Displayed {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        other
            .node
            .bytes
            .cmp(&self.node.bytes)
            .then_with(|| self.node.path.cmp(&other.node.path))
    }
}

struct Context<'a, F: Fn(AnalysisProgress)> {
    cancel: &'a AtomicBool,
    progress: F,
    started: Instant,
    last_progress: Option<Instant>,
    analysis_id: String,
    device: u64,
    visits: u64,
    files: u64,
    bytes: u64,
    displayed_nodes: u64,
    child_cap: usize,
    stopped: bool,
    warnings: Vec<String>,
}

impl<F: Fn(AnalysisProgress)> Context<'_, F> {
    fn warn(&mut self, warning: String) {
        if self.warnings.contains(&warning) {
            return;
        }
        if self.warnings.len() < 99 {
            self.warnings.push(warning);
        } else if self.warnings.len() == 99 {
            self.warnings.push(
                "另有无法读取或省略的目录，警告已达到显示上限。未知空间未计入已知占用。".into(),
            );
        }
    }

    fn check(&mut self) -> bool {
        if self.stopped {
            return false;
        }
        if self.cancel.load(Ordering::Relaxed) {
            self.stopped = true;
            return false;
        }
        if self.visits >= MAX_VISITS || self.started.elapsed() >= MAX_TIME {
            self.stopped = true;
            self.warn(
                "分析达到 300 秒或 200 万项目上限，结果不完整；请选择较小目录继续分析。".into(),
            );
            return false;
        }
        true
    }

    fn emit(&mut self, path: &Path) {
        let now = Instant::now();
        if self
            .last_progress
            .is_some_and(|last| now.duration_since(last) < Duration::from_millis(200))
        {
            return;
        }
        self.last_progress = Some(now);
        (self.progress)(AnalysisProgress {
            current_path: path.to_string_lossy().into_owned(),
            scanned_files: self.files,
            bytes_found: self.bytes,
        });
    }
}

fn empty_partial() -> Measured {
    Measured {
        totals: Totals {
            partial: true,
            ..Totals::default()
        },
        displayed: None,
    }
}

fn measure<F: Fn(AnalysisProgress)>(
    path: &Path,
    depth: usize,
    retain: bool,
    ctx: &mut Context<'_, F>,
) -> Measured {
    if !ctx.check() {
        return empty_partial();
    }
    ctx.visits += 1;
    let node_id = format!("{}-{}", ctx.analysis_id, ctx.visits);
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => {
            ctx.warn(format!(
                "无法读取 {}：{}。此处占用未知，结果为部分统计。",
                path.display(),
                error
            ));
            return empty_partial();
        }
    };
    let symlink = metadata.file_type().is_symlink();
    let directory = metadata.is_dir() && !symlink;
    let cross_device = directory && metadata.dev() != ctx.device;
    let mut totals = Totals {
        bytes: if cross_device {
            0
        } else {
            metadata.blocks().saturating_mul(512)
        },
        files: u64::from(metadata.is_file()),
        dirs: u64::from(directory),
        partial: cross_device,
        children: 0,
    };
    ctx.files = ctx.files.saturating_add(totals.files);
    ctx.bytes = ctx.bytes.saturating_add(totals.bytes);
    ctx.emit(path);
    let show_children = retain && depth < DISPLAY_DEPTH && ctx.displayed_nodes < DISPLAY_NODES;
    if retain && depth < DISPLAY_DEPTH && !show_children {
        ctx.warn(
            "为控制内存，部分深层目录没有展开；完整统计仍包含这些项目，可选择该目录重新分析。"
                .into(),
        );
    }
    let mut largest = BinaryHeap::<Displayed>::new();
    if cross_device {
        ctx.warn(format!(
            "{} 位于另一文件系统或挂载卷，已跳过其内容；占用未知。",
            path.display()
        ));
    } else if directory && depth >= MAX_DEPTH {
        totals.partial = true;
        ctx.warn(format!(
            "{} 超过安全遍历深度，已跳过其内容；占用未知。",
            path.display()
        ));
    } else if directory {
        match fs::read_dir(path) {
            Err(error) => {
                totals.partial = true;
                ctx.warn(format!(
                    "无法列出 {}：{}。此目录内容占用未知，结果为部分统计。",
                    path.display(),
                    error
                ));
            }
            Ok(mut entries) => loop {
                if !ctx.check() {
                    totals.partial = true;
                    break;
                }
                let Some(entry) = entries.next() else {
                    break;
                };
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        totals.partial = true;
                        ctx.warn(format!(
                            "无法读取 {} 的某个子项：{}。部分内容占用未知。",
                            path.display(),
                            error
                        ));
                        continue;
                    }
                };
                totals.children += 1;
                let child = measure(&entry.path(), depth + 1, show_children, ctx);
                totals.bytes = totals.bytes.saturating_add(child.totals.bytes);
                totals.files = totals.files.saturating_add(child.totals.files);
                totals.dirs = totals.dirs.saturating_add(child.totals.dirs);
                totals.partial |= child.totals.partial;
                if let Some(displayed) = child.displayed {
                    largest.push(displayed);
                    if largest.len() > ctx.child_cap {
                        if let Some(omitted) = largest.pop() {
                            ctx.displayed_nodes = ctx.displayed_nodes.saturating_sub(omitted.nodes);
                        }
                    }
                }
            },
        }
        // The walk is not a filesystem snapshot; flag directories changing while measured.
        match fs::symlink_metadata(path) {
            Ok(after)
                if after.dev() == metadata.dev()
                    && after.ino() == metadata.ino()
                    && after.mtime() == metadata.mtime()
                    && after.mtime_nsec() == metadata.mtime_nsec()
                    && after.ctime() == metadata.ctime()
                    && after.ctime_nsec() == metadata.ctime_nsec() => {}
            _ => {
                totals.partial = true;
                ctx.warn(format!(
                    "{} 在分析期间发生变化；其占用仅为部分时间点的统计。",
                    path.display()
                ));
            }
        }
    }
    let displayed = if retain {
        if path.to_str().is_none() {
            ctx.warn(format!(
                "{} 的路径不是有效 UTF-8；占用仍已统计，但无法从应用打开或继续分析此路径。",
                path.display()
            ));
        }
        let mut children = largest.into_vec();
        children.sort_by(|left, right| {
            right
                .node
                .bytes
                .cmp(&left.node.bytes)
                .then_with(|| left.node.path.cmp(&right.node.path))
        });
        let nodes = 1 + children.iter().map(|child| child.nodes).sum::<u64>();
        let omitted_children = totals.children.saturating_sub(children.len() as u64);
        ctx.displayed_nodes += 1;
        Some(Displayed {
            nodes,
            node: DirectoryNode {
                id: node_id,
                path: path.to_str().unwrap_or_default().to_string(),
                name: path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "/".into()),
                kind: if symlink {
                    "symlink"
                } else if directory {
                    "directory"
                } else {
                    "file"
                }
                .into(),
                bytes: totals.bytes,
                files: totals.files,
                dirs: totals.dirs,
                children: children.into_iter().map(|child| child.node).collect(),
                has_children: directory && (totals.children > 0 || totals.partial),
                partial: totals.partial,
                omitted_children,
            },
        })
    } else {
        None
    };
    Measured { totals, displayed }
}

pub(crate) fn filesystem_capacity(path: &Path) -> Result<(u64, u64), String> {
    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| "目录路径无效。")?;
    let mut status = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: CString is terminated, and statvfs initializes the supplied valid struct.
    if unsafe { libc::statvfs(path.as_ptr(), status.as_mut_ptr()) } != 0 {
        return Err(format!(
            "无法读取文件系统容量：{}",
            io::Error::last_os_error()
        ));
    }
    // SAFETY: statvfs succeeded.
    let status = unsafe { status.assume_init() };
    let block = if status.f_frsize == 0 {
        status.f_bsize
    } else {
        status.f_frsize
    };
    Ok((
        (status.f_blocks as u64).saturating_mul(block),
        (status.f_bavail as u64).saturating_mul(block),
    ))
}

pub fn analyze(
    path: &Path,
    cancel: &AtomicBool,
    progress: impl Fn(AnalysisProgress),
) -> Result<AnalysisReport, String> {
    if !path.is_absolute() {
        return Err("请选择绝对路径的本地目录。".into());
    }
    let path = path
        .canonicalize()
        .map_err(|error| format!("无法打开选定目录：{error}"))?;
    let metadata =
        fs::symlink_metadata(&path).map_err(|error| format!("无法读取选定目录：{error}"))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("请选择真实目录。文件和符号链接不能作为分析根目录。".into());
    }
    let (total_bytes, available_bytes) = filesystem_capacity(&path)?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let analysis_id = format!(
        "analysis-{timestamp}-{}-{}",
        std::process::id(),
        NEXT_ANALYSIS.fetch_add(1, Ordering::Relaxed)
    );
    let mut ctx = Context {
        cancel,
        progress,
        started: Instant::now(),
        last_progress: None,
        analysis_id: analysis_id.clone(),
        device: metadata.dev(),
        visits: 0,
        files: 0,
        bytes: 0,
        displayed_nodes: 0,
        child_cap: DISPLAY_CHILDREN,
        stopped: false,
        warnings: Vec::new(),
    };
    let measured = measure(&path, 0, true, &mut ctx);
    let cancelled = cancel.load(Ordering::Relaxed);
    let root = measured
        .displayed
        .map(|displayed| displayed.node)
        .unwrap_or_else(|| DirectoryNode {
            id: format!("{analysis_id}-root"),
            path: path.to_string_lossy().into_owned(),
            name: path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "/".into()),
            kind: "directory".into(),
            bytes: 0,
            files: 0,
            dirs: 0,
            children: Vec::new(),
            has_children: true,
            partial: true,
            omitted_children: 0,
        });
    Ok(AnalysisReport {
        analysis_id,
        root,
        duration_ms: ctx.started.elapsed().as_millis() as u64,
        scanned_files: ctx.files,
        warnings: ctx.warnings,
        cancelled,
        total_bytes,
        available_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::path::PathBuf;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "mac-sweep-analysis-{}-{}",
                std::process::id(),
                NEXT_ANALYSIS.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }
        fn file(&self, relative: &str, size: usize) -> PathBuf {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, vec![1_u8; size]).unwrap();
            path
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn blocks(path: &Path) -> u64 {
        fs::symlink_metadata(path).unwrap().blocks() * 512
    }

    // APFS rejects invalid UTF-8 filenames; this filesystem fixture applies to Unix
    // filesystems that permit arbitrary byte names.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn non_utf8_names_are_counted_without_a_reconstructed_action_path() {
        use std::os::unix::ffi::OsStringExt;
        let fixture = Fixture::new();
        let path = fixture
            .0
            .join(std::ffi::OsString::from_vec(vec![b'x', 0xff]));
        fs::write(&path, vec![1_u8; 4096]).unwrap();
        let report = analyze(&fixture.0, &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!(report.root.files, 1);
        assert_eq!(report.root.children[0].bytes, blocks(&path));
        assert!(report.root.children[0].path.is_empty());
        assert!(report
            .warnings
            .iter()
            .any(|warning| warning.contains("UTF-8")));
    }

    #[test]
    fn full_totals_include_hidden_projects_models_and_deep_files() {
        let fixture = Fixture::new();
        let files = [
            fixture.file(".git/objects/fixture", 4_096),
            fixture.file("node_modules/pkg/model.safetensors", 32_768),
            fixture.file("a/b/c/d/large.bin", 65_536),
        ];
        let expected = blocks(&fixture.0)
            + files.iter().map(|path| blocks(path)).sum::<u64>()
            + [
                ".git",
                ".git/objects",
                "node_modules",
                "node_modules/pkg",
                "a",
                "a/b",
                "a/b/c",
                "a/b/c/d",
            ]
            .iter()
            .map(|relative| blocks(&fixture.0.join(relative)))
            .sum::<u64>();
        let report = analyze(&fixture.0, &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!(report.root.bytes, expected);
        assert_eq!(report.scanned_files, 3);
        assert_eq!(report.root.files, 3);
        assert_eq!(report.root.dirs, 9);
        assert!(!report.root.partial);
        assert!(report
            .root
            .children
            .windows(2)
            .all(|rows| rows[0].bytes >= rows[1].bytes));
        let a = report
            .root
            .children
            .iter()
            .find(|node| node.name == "a")
            .unwrap();
        let c = &a.children[0].children[0];
        assert!(c.children.is_empty());
        assert!(c.has_children);
        assert_eq!(c.omitted_children, 1);
        assert_eq!(c.files, 1);
    }

    #[test]
    fn omission_preserves_full_totals_and_symlinks_do_not_follow_targets() {
        let fixture = Fixture::new();
        let small = fixture.file("small.bin", 4_096);
        let large = fixture.file("large.bin", 32_768);
        symlink(&fixture.0, fixture.0.join("loop-link")).unwrap();
        let cancel = AtomicBool::new(false);
        let mut ctx = Context {
            cancel: &cancel,
            progress: |_| {},
            started: Instant::now(),
            last_progress: None,
            analysis_id: "fixture".into(),
            device: fs::metadata(&fixture.0).unwrap().dev(),
            visits: 0,
            files: 0,
            bytes: 0,
            displayed_nodes: 0,
            child_cap: 1,
            stopped: false,
            warnings: Vec::new(),
        };
        let measured = measure(&fixture.0, 0, true, &mut ctx);
        let root = measured.displayed.unwrap().node;
        assert_eq!(
            root.bytes,
            blocks(&fixture.0)
                + blocks(&small)
                + blocks(&large)
                + blocks(&fixture.0.join("loop-link"))
        );
        assert_eq!(root.files, 2);
        assert_eq!(root.children.len(), 1);
        assert_eq!(root.children[0].name, "large.bin");
        assert_eq!(root.omitted_children, 2);
        assert!(!root.partial);
        let report = analyze(&fixture.0, &cancel, |_| {}).unwrap();
        let link = report
            .root
            .children
            .iter()
            .find(|node| node.kind == "symlink")
            .unwrap();
        assert!(!link.has_children);
        assert!(link.children.is_empty());
    }

    #[test]
    fn cancellation_and_permission_failure_are_explicitly_partial() {
        let fixture = Fixture::new();
        fixture.file("restricted/private.bin", 4_096);
        let report = analyze(&fixture.0, &AtomicBool::new(true), |_| {}).unwrap();
        assert!(report.cancelled);
        assert!(report.root.partial);
        assert_eq!(report.scanned_files, 0);
        let restricted = fixture.0.join("restricted");
        fs::set_permissions(&restricted, fs::Permissions::from_mode(0o0)).unwrap();
        if fs::read_dir(&restricted).is_err() {
            let report = analyze(&fixture.0, &AtomicBool::new(false), |_| {}).unwrap();
            assert!(report.root.partial);
            assert!(report
                .warnings
                .iter()
                .any(|warning| warning.contains("未知")));
            assert_eq!(report.scanned_files, 0);
        }
        fs::set_permissions(&restricted, fs::Permissions::from_mode(0o700)).unwrap();
    }
}
