//! Read-only directory sizes. This module never supplies cleanup candidates.
//! Allocated blocks include directory/link storage. Multiply-linked regular files
//! are counted once per inode; APFS shared extents cannot be deduplicated here.

use crate::analysis_cache::{BoundedDirectorySummaries, DirectorySummary, DirectorySummaryIndex};
use serde::Serialize;
use std::cmp::Ordering as CmpOrdering;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::ffi::{CString, OsString};
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DISPLAY_DEPTH: usize = 3;
const DISPLAY_CHILDREN: usize = 300;
const DISPLAY_NODES: u64 = 15_000;
const DIRECTORY_SUMMARIES: usize = 20_000;
const MAX_DEPTH: usize = 256;
const ROOT_PREFETCH_ENTRIES: usize = 100_000;
const DEEP_PROGRESS_ENTRIES: usize = 1_024;
// macOS SDK sys/stat.h: SF_DATALESS marks a File Provider object whose content
// is online. Enumerating an online directory/package can trigger hydration.
const SF_DATALESS: u32 = 0x4000_0000;
const PROTECTED_DIRECTORY_TIMEOUT: Duration = Duration::from_secs(10);
const DIRECTORY_WAIT_POLL: Duration = Duration::from_millis(100);
const PROTECTED_DIRECTORY_ENTRY_LIMIT: usize = 100_000;
const MAX_DIRECTORY_WORKERS: usize = 4;
static ACTIVE_DIRECTORY_WORKERS: AtomicUsize = AtomicUsize::new(0);
static DIRECTORY_POOL: OnceLock<Result<DirectoryWorkerPool, String>> = OnceLock::new();
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
    pub size_known: bool,
    pub size_source: String,
    #[serde(skip)]
    device: u64,
    #[serde(skip)]
    inode: u64,
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
    /// Completeness of the selected directory walk, independent of display caps.
    pub scan_complete: bool,
    pub permission_denied_count: u64,
    pub other_error_count: u64,
    pub skipped_mount_count: u64,
    pub changed_directory_count: u64,
    pub depth_limited_count: u64,
    pub cloud_placeholder_count: u64,
    /// Protected personal/app directories left unread after OS timeouts or exhausted slots.
    pub blocked_directory_count: u64,
    pub cached_browse: bool,
    pub source_analysis_id: String,
    /// Source full analysis completion time, in Unix milliseconds.
    pub measured_at: u64,
    #[serde(skip)]
    pub summary_index: Arc<DirectorySummaryIndex>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisProgress {
    pub current_path: String,
    pub scanned_files: u64,
    pub bytes_found: u64,
    pub estimated_percent: Option<f64>,
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

impl DirectoryNode {
    pub(crate) fn filesystem_identity(&self) -> (u64, u64) {
        (self.device, self.inode)
    }

    fn shallow_clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            path: self.path.clone(),
            name: self.name.clone(),
            kind: self.kind.clone(),
            bytes: self.bytes,
            files: self.files,
            dirs: self.dirs,
            children: Vec::new(),
            has_children: self.has_children,
            partial: self.partial,
            omitted_children: 0,
            size_known: self.size_known,
            size_source: self.size_source.clone(),
            device: self.device,
            inode: self.inode,
        }
    }
}

/// Only a directory already present in the registered report can be browsed.
/// The shared source index additionally bounds its filesystem/path authority.
pub fn directory_for_browse(
    report: &AnalysisReport,
    node_id: &str,
) -> Result<DirectoryNode, String> {
    let mut pending = vec![&report.root];
    while let Some(node) = pending.pop() {
        if node.id == node_id {
            if node.kind != "directory"
                || node.path.is_empty()
                || !report.summary_index.contains_scope(Path::new(&node.path))
                || node.device != report.summary_index.device
            {
                return Err("ANALYSIS_NODE_CHANGED:此目录已变化，请重新分析。".into());
            }
            return Ok(node.shallow_clone());
        }
        pending.extend(&node.children);
    }
    Err("ANALYSIS_NODE_CHANGED:此目录已变化，请重新分析。".into())
}

fn browse_path_error(error: io::Error) -> String {
    if error.kind() == io::ErrorKind::NotFound {
        "ANALYSIS_NODE_MISSING:此目录已被移动或删除。".into()
    } else if error.kind() == io::ErrorKind::NotADirectory
        || error.raw_os_error() == Some(libc::ELOOP)
    {
        "ANALYSIS_NODE_CHANGED:此目录已变化，请重新分析。".into()
    } else if error.kind() == io::ErrorKind::PermissionDenied {
        "无法读取这个目录，请检查访问权限。".into()
    } else {
        format!("无法读取目录信息，请稍后重试：{error}")
    }
}

fn directory_failure<F: Fn(AnalysisProgress)>(
    ctx: &mut Context<'_, F>,
    path: &Path,
    failure: DirectoryReadFailure,
) {
    match failure {
        DirectoryReadFailure::Io(error) => {
            ctx.record_error(&error);
            ctx.warn(format!(
                "无法列出 {}：{}。目录内容未读取。",
                path.display(),
                error
            ));
        }
        DirectoryReadFailure::TimedOut | DirectoryReadFailure::WorkerLimit => {
            ctx.blocked_directory_count += 1;
            ctx.other_error_count += 1;
            let reason = if matches!(failure, DirectoryReadFailure::TimedOut) {
                "系统未及时返回目录内容"
            } else {
                "此前受保护目录仍未返回，等待线程达到限额，此目录暂未读取"
            };
            ctx.warn(format!(
                "{}：{reason}。可以检查完整磁盘访问权限后重试。",
                path.display()
            ));
        }
        DirectoryReadFailure::Cancelled => ctx.stopped = true,
    }
}

/// Recover only directory entries already measured beneath this exact parent.
/// No filesystem calls occur here, and an absent file/summary is never invented.
fn cached_direct_subdirs(
    index: &DirectorySummaryIndex,
    parent: &Path,
    cancel: &AtomicBool,
) -> (BinaryHeap<Displayed>, u64) {
    let mut rows = BinaryHeap::new();
    let mut count = 0;
    if !index.contains_scope(parent) {
        return (rows, count);
    }
    for summary in index.entries.values() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let path = Path::new(&summary.path);
        if path.parent() != Some(parent)
            || !index.contains_scope(path)
            || summary.device != index.device
        {
            continue;
        }
        count += 1;
        rows.push(Displayed {
            node: DirectoryNode {
                id: summary.id.clone(),
                path: summary.path.clone(),
                name: summary.name.clone(),
                kind: "directory".into(),
                bytes: summary.bytes,
                files: summary.files,
                dirs: summary.dirs,
                children: Vec::new(),
                has_children: summary.has_children,
                partial: summary.partial,
                omitted_children: 0,
                size_known: summary.size_known,
                size_source: "cached".into(),
                device: summary.device,
                inode: summary.inode,
            },
            nodes: 1,
        });
        if rows.len() > DISPLAY_CHILDREN {
            rows.pop();
        }
    }
    (rows, count)
}

/// Read only immediate entries. Folder totals come from the bounded source cache;
/// a missing summary is explicitly unknown and never starts a recursive scan.
pub fn browse(
    selected: DirectoryNode,
    index: Arc<DirectorySummaryIndex>,
    total_bytes: u64,
    available_bytes: u64,
    cancel: &AtomicBool,
) -> Result<AnalysisReport, String> {
    let path = Path::new(&selected.path);
    if selected.kind != "directory"
        || !index.contains_scope(path)
        || selected.device != index.device
    {
        return Err("ANALYSIS_NODE_CHANGED:此目录已变化，请重新分析。".into());
    }
    let source_root = Path::new(&index.root);
    let current_root = guarded_io(|| source_root.canonicalize()).map_err(browse_path_error)?;
    let current_path = guarded_io(|| path.canonicalize()).map_err(browse_path_error)?;
    if current_root != source_root
        || current_path != path
        || !current_path.starts_with(&current_root)
    {
        return Err("ANALYSIS_NODE_CHANGED:此目录已变化，请重新分析。".into());
    }
    // Validate the current resolution as well as the selected inode. The path
    // can still change after this check; this is not a filesystem snapshot.
    let metadata = guarded_io(|| fs::symlink_metadata(path)).map_err(browse_path_error)?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.dev() != selected.device
        || metadata.ino() != selected.inode
    {
        return Err("ANALYSIS_NODE_CHANGED:此目录已变化，请重新分析。".into());
    }
    let analysis_id = format!(
        "browse-{}-{}",
        std::process::id(),
        NEXT_ANALYSIS.fetch_add(1, Ordering::Relaxed)
    );
    let mut ctx = Context::new(cancel, |_| {}, analysis_id.clone(), index.device);
    let mut root = selected.shallow_clone();
    if root.size_known {
        root.size_source = "cached".into();
    }
    let mut rows = BinaryHeap::new();
    let mut direct_count = 0_u64;
    let mut listing_partial = false;
    if !ctx.check() {
        listing_partial = true;
    } else if is_cloud_placeholder(&metadata) {
        ctx.record_metadata(&metadata, true);
        listing_partial = true;
        root.has_children = false;
    } else {
        match directory_entries(path, cancel) {
            Err(failure) => {
                let use_cache = matches!(
                    failure,
                    DirectoryReadFailure::TimedOut | DirectoryReadFailure::WorkerLimit
                );
                directory_failure(&mut ctx, path, failure);
                listing_partial = true;
                if use_cache {
                    (rows, direct_count) = cached_direct_subdirs(&index, path, cancel);
                    ctx.warn("实时目录未读到，仅显示已有统计中的子目录，列表可能有遗漏。".into());
                }
            }
            Ok((mut entries, truncated)) => {
                listing_partial |= truncated;
                if truncated {
                    ctx.warn("直属项目达到显示读取上限，部分项目没有列出。".into());
                }
                loop {
                    if !ctx.check() {
                        listing_partial = true;
                        break;
                    }
                    let Some(entry) = entries.next() else {
                        break;
                    };
                    direct_count += 1;
                    let entry = match entry {
                        Ok(entry) => entry,
                        Err(error) => {
                            ctx.record_error(&error);
                            ctx.warn(format!("无法读取直属项目：{error}"));
                            listing_partial = true;
                            continue;
                        }
                    };
                    let metadata = match entry.metadata() {
                        Ok(metadata) => metadata,
                        Err(error) => {
                            ctx.record_error(&error);
                            ctx.warn(format!("无法读取直属项目信息：{error}"));
                            listing_partial = true;
                            continue;
                        }
                    };
                    let child_path = entry.path();
                    let child_string = child_path.to_str().unwrap_or_default().to_owned();
                    let directory = metadata.is_dir();
                    let cloud = is_cloud_placeholder(&metadata);
                    let own = ctx.record_metadata(&metadata, cloud);
                    let cached = index.lookup(&child_string).filter(|summary| {
                        directory
                            && summary.device == metadata.dev()
                            && summary.inode == metadata.ino()
                    });
                    let mut child = DirectoryNode {
                        id: format!("{analysis_id}-{direct_count}"),
                        path: child_string,
                        name: entry.file_name().to_string_lossy().into_owned(),
                        kind: if metadata.file_type().is_symlink() {
                            "symlink"
                        } else if directory {
                            "directory"
                        } else {
                            "file"
                        }
                        .into(),
                        bytes: if directory { 0 } else { own.bytes },
                        files: own.files,
                        dirs: own.dirs,
                        children: Vec::new(),
                        has_children: directory && !cloud && metadata.dev() == index.device,
                        partial: directory,
                        omitted_children: 0,
                        size_known: !directory,
                        size_source: if directory { "unknown" } else { "stat" }.into(),
                        device: metadata.dev(),
                        inode: metadata.ino(),
                    };
                    if let Some(cached) = cached {
                        child.id = cached.id.clone();
                        child.name = cached.name.clone();
                        child.bytes = cached.bytes;
                        child.files = cached.files;
                        child.dirs = cached.dirs;
                        child.partial = cached.partial;
                        child.size_known = cached.size_known;
                        child.size_source = if cached.size_known {
                            "cached"
                        } else {
                            "unknown"
                        }
                        .into();
                        child.has_children = cached.has_children && !cloud;
                    }
                    if directory && metadata.dev() != index.device {
                        ctx.skipped_mount_count += 1;
                        child.size_known = false;
                        child.size_source = "unknown".into();
                        child.bytes = 0;
                        ctx.warn(format!(
                            "{} 属于另一挂载卷，没有继续读取。",
                            child_path.display()
                        ));
                    }
                    rows.push(Displayed {
                        node: child,
                        nodes: 1,
                    });
                    if rows.len() > DISPLAY_CHILDREN {
                        rows.pop();
                    }
                }
            }
        }
    }
    let mut children = rows.into_vec();
    children.sort_by(|left, right| {
        right
            .node
            .bytes
            .cmp(&left.node.bytes)
            .then_with(|| left.node.path.cmp(&right.node.path))
    });
    root.omitted_children = direct_count.saturating_sub(children.len() as u64);
    root.children = children.into_iter().map(|row| row.node).collect();
    root.partial |= listing_partial;
    let cancelled = cancel.load(Ordering::Relaxed);
    Ok(AnalysisReport {
        analysis_id,
        root,
        duration_ms: ctx.started.elapsed().as_millis() as u64,
        scanned_files: ctx.files,
        warnings: ctx.warnings,
        cancelled,
        total_bytes,
        available_bytes,
        scan_complete: false,
        permission_denied_count: ctx.permission_denied_count,
        other_error_count: ctx.other_error_count,
        skipped_mount_count: ctx.skipped_mount_count,
        changed_directory_count: 0,
        depth_limited_count: 0,
        cloud_placeholder_count: ctx.cloud_placeholder_count,
        blocked_directory_count: ctx.blocked_directory_count,
        cached_browse: true,
        source_analysis_id: index.source_analysis_id.clone(),
        measured_at: index.measured_at,
        summary_index: index,
    })
}

/// A persisted favorite is an authorized location, not a previously measured
/// tree. Open its immediate entries with unknown folder totals, never recurse.
pub fn browse_location(path: &Path, cancel: &AtomicBool) -> Result<AnalysisReport, String> {
    let canonical = guarded_io(|| path.canonicalize()).map_err(browse_path_error)?;
    let metadata = guarded_io(|| fs::symlink_metadata(path)).map_err(browse_path_error)?;
    if canonical != path || !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("ANALYSIS_NODE_CHANGED:此目录已变化，请重新分析。".into());
    }
    let source_id = format!(
        "location-{}-{}",
        std::process::id(),
        NEXT_ANALYSIS.fetch_add(1, Ordering::Relaxed)
    );
    let root = DirectoryNode {
        id: format!("{source_id}-root"),
        path: canonical
            .to_str()
            .ok_or("这个目录名称无法打开。")?
            .to_owned(),
        name: canonical
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/".into()),
        kind: "directory".into(),
        bytes: 0,
        files: 0,
        dirs: 1,
        children: Vec::new(),
        has_children: true,
        partial: true,
        omitted_children: 0,
        size_known: false,
        size_source: "unknown".into(),
        device: metadata.dev(),
        inode: metadata.ino(),
    };
    let index = Arc::new(DirectorySummaryIndex {
        source_analysis_id: source_id,
        root: root.path.clone(),
        device: metadata.dev(),
        measured_at: 0,
        entries: HashMap::new(),
    });
    browse(root, index, 0, 0, cancel)
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
    /// Only multiply-linked files need an inode set; ordinary files use no entry.
    hard_links: HashSet<(u64, u64)>,
    permission_denied_count: u64,
    other_error_count: u64,
    skipped_mount_count: u64,
    changed_directory_count: u64,
    depth_limited_count: u64,
    cloud_placeholder_count: u64,
    blocked_directory_count: u64,
    summaries: BoundedDirectorySummaries,
    work_budget: f64,
    work_credit: f64,
    estimate_ready: bool,
    unknown_work_streams: u64,
    #[cfg(test)]
    unthrottled_progress: bool,
}

impl<F: Fn(AnalysisProgress)> Context<'_, F> {
    fn new(cancel: &AtomicBool, progress: F, analysis_id: String, device: u64) -> Context<'_, F> {
        Context {
            cancel,
            progress,
            started: Instant::now(),
            last_progress: None,
            analysis_id,
            device,
            visits: 0,
            files: 0,
            bytes: 0,
            displayed_nodes: 0,
            child_cap: DISPLAY_CHILDREN,
            stopped: false,
            warnings: Vec::new(),
            hard_links: HashSet::new(),
            permission_denied_count: 0,
            other_error_count: 0,
            skipped_mount_count: 0,
            changed_directory_count: 0,
            depth_limited_count: 0,
            cloud_placeholder_count: 0,
            blocked_directory_count: 0,
            summaries: BoundedDirectorySummaries::new(DIRECTORY_SUMMARIES),
            work_budget: 1.0,
            work_credit: 0.0,
            estimate_ready: false,
            unknown_work_streams: 0,
            #[cfg(test)]
            unthrottled_progress: false,
        }
    }

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
        true
    }

    fn progress_due(&self) -> bool {
        #[cfg(test)]
        if self.unthrottled_progress {
            return true;
        }
        let now = Instant::now();
        !self
            .last_progress
            .is_some_and(|last| now.duration_since(last) < Duration::from_millis(200))
    }

    fn emit(&mut self, path: &Path) {
        if !self.progress_due() {
            return;
        }
        self.last_progress = Some(Instant::now());
        (self.progress)(AnalysisProgress {
            current_path: path.to_string_lossy().into_owned(),
            scanned_files: self.files,
            bytes_found: self.bytes,
            estimated_percent: self.estimated_percent(),
        });
    }

    fn estimated_percent(&self) -> Option<f64> {
        (self.estimate_ready && self.unknown_work_streams == 0)
            .then(|| (self.work_credit * 100.0).clamp(0.0, 99.0))
    }

    fn complete_work(&mut self, credit: f64) {
        self.work_credit = (self.work_credit + credit.max(0.0)).min(1.0);
    }

    fn record_error(&mut self, error: &io::Error) {
        if error.kind() == io::ErrorKind::PermissionDenied
            || matches!(error.raw_os_error(), Some(libc::EPERM) | Some(libc::EACCES))
        {
            self.permission_denied_count = self.permission_denied_count.saturating_add(1);
        } else {
            self.other_error_count = self.other_error_count.saturating_add(1);
        }
    }

    fn record_metadata(&mut self, metadata: &fs::Metadata, cloud_placeholder: bool) -> Totals {
        let directory = metadata.is_dir();
        let file = metadata.is_file();
        let cross_device = directory && metadata.dev() != self.device;
        // APFS clones have different inodes. Only real hard links can be safely
        // deduplicated using stat; shared/cloned extents remain separately measured.
        let duplicate_link = file
            && metadata.nlink() > 1
            && !self.hard_links.insert((metadata.dev(), metadata.ino()));
        if cloud_placeholder {
            self.cloud_placeholder_count = self.cloud_placeholder_count.saturating_add(1);
            self.warn("在线云盘占位内容未下载，未计入本地文件占用；只统计已有的本地数据。".into());
        }
        let totals = Totals {
            bytes: if cross_device || duplicate_link {
                0
            } else {
                metadata.blocks().saturating_mul(512)
            },
            files: u64::from(file),
            dirs: u64::from(directory),
            partial: cross_device || (directory && cloud_placeholder),
            children: 0,
        };
        self.files = self.files.saturating_add(totals.files);
        self.bytes = self.bytes.saturating_add(totals.bytes);
        totals
    }

    #[cfg(target_os = "macos")]
    fn record_bulk_file(&mut self, metadata: crate::macos_bulk::FileMetadata) -> Totals {
        let duplicate =
            metadata.link_count > 1 && !self.hard_links.insert((metadata.device, metadata.inode));
        if flags_are_cloud_placeholder(metadata.flags) {
            self.cloud_placeholder_count += 1;
            self.warn("在线云盘占位内容未下载，未计入本地文件占用；只统计已有的本地数据。".into());
        }
        let totals = Totals {
            bytes: if duplicate {
                0
            } else {
                metadata.allocated_bytes
            },
            files: 1,
            ..Totals::default()
        };
        self.files = self.files.saturating_add(1);
        self.bytes = self.bytes.saturating_add(totals.bytes);
        totals
    }
}

fn flags_are_cloud_placeholder(flags: u32) -> bool {
    flags & SF_DATALESS != 0
}

fn is_cloud_placeholder(metadata: &fs::Metadata) -> bool {
    #[cfg(target_os = "macos")]
    {
        use std::os::macos::fs::MetadataExt as MacMetadataExt;
        flags_are_cloud_placeholder(metadata.st_flags())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = metadata;
        flags_are_cloud_placeholder(0)
    }
}

/// macOS personal-data and app-data trees can block while opening or enumerating
/// directories. Bound each wait without placing a limit on ordinary folders.
fn requires_bounded_enumeration(path: &Path) -> bool {
    let mut before_previous: Option<Component<'_>> = None;
    let mut previous: Option<Component<'_>> = None;
    for component in path.components() {
        let name = component.as_os_str();
        if before_previous.is_some_and(|parent| parent.as_os_str() == "Users")
            && matches!(previous, Some(Component::Normal(_)))
            && matches!(
                name.to_str(),
                Some(
                    "Desktop"
                        | "Documents"
                        | "Downloads"
                        | "Music"
                        | "Movies"
                        | "Pictures"
                        | "Library"
                )
            )
        {
            return true;
        }
        if previous.is_some_and(|parent| parent.as_os_str() == "Library")
            && matches!(name.to_str(), Some("Group Containers" | "Containers"))
        {
            return true;
        }
        before_previous = previous;
        previous = Some(component);
    }
    false
}

struct DirectoryWorkerSlot;

impl DirectoryWorkerSlot {
    fn reserve() -> Option<Self> {
        let mut active = ACTIVE_DIRECTORY_WORKERS.load(Ordering::Acquire);
        loop {
            if active >= MAX_DIRECTORY_WORKERS {
                return None;
            }
            match ACTIVE_DIRECTORY_WORKERS.compare_exchange_weak(
                active,
                active + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(Self),
                Err(current) => active = current,
            }
        }
    }
}

impl Drop for DirectoryWorkerSlot {
    fn drop(&mut self) {
        ACTIVE_DIRECTORY_WORKERS.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Debug)]
enum DirectoryReadFailure {
    Io(io::Error),
    TimedOut,
    WorkerLimit,
    Cancelled,
}

type DirectoryJob = Box<dyn FnOnce() + Send + 'static>;

struct DirectoryWorkerPool {
    sender: mpsc::SyncSender<DirectoryJob>,
}

impl DirectoryWorkerPool {
    fn start() -> Result<Self, String> {
        let (sender, receiver) = mpsc::sync_channel::<DirectoryJob>(MAX_DIRECTORY_WORKERS);
        let receiver = Arc::new(Mutex::new(receiver));
        for worker in 0..MAX_DIRECTORY_WORKERS {
            let receiver = Arc::clone(&receiver);
            thread::Builder::new()
                .name(format!("mac-sweep-directory-{worker}"))
                .spawn(move || loop {
                    // Hold the receiver lock only while dequeuing. The native
                    // call itself runs outside it, so all four workers can run.
                    let job = {
                        let receiver = receiver.lock().unwrap_or_else(|error| error.into_inner());
                        receiver.recv()
                    };
                    let Ok(job) = job else {
                        break;
                    };
                    // A failed task releases its slot/sender but must not shrink
                    // the permanent pool or cause later tasks to queue forever.
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                })
                .map_err(|error| format!("无法启动目录读取线程：{error}"))?;
        }
        Ok(Self { sender })
    }

    fn shared() -> Result<&'static Self, DirectoryReadFailure> {
        DIRECTORY_POOL
            .get_or_init(Self::start)
            .as_ref()
            .map_err(|error| DirectoryReadFailure::Io(io::Error::other(error.clone())))
    }
}

/// A blocked system call cannot be interrupted safely. Keep its worker isolated,
/// let the scan continue, and cap outstanding workers across repeated scans.
fn run_directory_worker<T: Send + 'static>(
    cancel: &AtomicBool,
    timeout: Duration,
    operation: impl FnOnce(Arc<AtomicBool>) -> io::Result<T> + Send + 'static,
) -> Result<T, DirectoryReadFailure> {
    if cancel.load(Ordering::Relaxed) {
        return Err(DirectoryReadFailure::Cancelled);
    }
    let slot = DirectoryWorkerSlot::reserve().ok_or(DirectoryReadFailure::WorkerLimit)?;
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = Arc::clone(&stop);
    let (sender, receiver) = mpsc::sync_channel(1);
    let job: DirectoryJob = Box::new(move || {
        let _slot = slot;
        if worker_stop.load(Ordering::Relaxed) {
            return;
        }
        let result = operation(worker_stop);
        // A dropped receiver frees buffered entries when a delayed OS call
        // finally returns; no scan context or report is held by this thread.
        let _ = sender.send(result);
    });
    match DirectoryWorkerPool::shared()?.sender.try_send(job) {
        Ok(()) => {}
        Err(mpsc::TrySendError::Full(_)) => return Err(DirectoryReadFailure::WorkerLimit),
        Err(mpsc::TrySendError::Disconnected(_)) => {
            return Err(DirectoryReadFailure::Io(io::Error::other(
                "目录读取线程不可用。",
            )))
        }
    }
    let deadline = Instant::now() + timeout;
    loop {
        if cancel.load(Ordering::Relaxed) {
            stop.store(true, Ordering::Relaxed);
            return Err(DirectoryReadFailure::Cancelled);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            stop.store(true, Ordering::Relaxed);
            return Err(DirectoryReadFailure::TimedOut);
        }
        match receiver.recv_timeout(remaining.min(DIRECTORY_WAIT_POLL)) {
            Ok(result) => {
                if cancel.load(Ordering::Relaxed) {
                    stop.store(true, Ordering::Relaxed);
                    return Err(DirectoryReadFailure::Cancelled);
                }
                return result.map_err(DirectoryReadFailure::Io);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(DirectoryReadFailure::Io(io::Error::other(
                    "目录读取线程意外退出。",
                )));
            }
        }
    }
}

fn guarded_io<T>(operation: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    #[cfg(target_os = "macos")]
    {
        crate::macos_bulk::without_materialization(operation).map_err(bulk_error)
    }
    #[cfg(not(target_os = "macos"))]
    {
        operation()
    }
}

#[cfg(target_os = "macos")]
fn bulk_error(error: crate::macos_bulk::BulkError) -> io::Error {
    match error {
        crate::macos_bulk::BulkError::Io(error)
        | crate::macos_bulk::BulkError::Unsupported(error)
        | crate::macos_bulk::BulkError::PolicySetup(error) => error,
        crate::macos_bulk::BulkError::Cancelled => io::Error::from(io::ErrorKind::Interrupted),
    }
}

enum AnalysisEntry {
    Standard(Box<fs::DirEntry>),
    #[cfg(target_os = "macos")]
    Bulk {
        parent: Arc<PathBuf>,
        entry: crate::macos_bulk::BulkEntry,
    },
}

impl From<fs::DirEntry> for AnalysisEntry {
    fn from(entry: fs::DirEntry) -> Self {
        Self::Standard(Box::new(entry))
    }
}

impl AnalysisEntry {
    fn path(&self) -> PathBuf {
        match self {
            Self::Standard(entry) => entry.path(),
            #[cfg(target_os = "macos")]
            Self::Bulk { parent, entry } => parent.join(&entry.name),
        }
    }
    fn file_name(&self) -> OsString {
        match self {
            Self::Standard(entry) => entry.file_name(),
            #[cfg(target_os = "macos")]
            Self::Bulk { entry, .. } => entry.name.clone(),
        }
    }
    fn metadata(&self) -> io::Result<fs::Metadata> {
        guarded_io(|| match self {
            Self::Standard(entry) => entry.metadata(),
            #[cfg(target_os = "macos")]
            Self::Bulk { .. } => fs::symlink_metadata(self.path()),
        })
    }
    fn progress_weight(&self) -> f64 {
        match self {
            Self::Standard(entry) => match guarded_io(|| entry.file_type()) {
                Ok(kind) if !kind.is_dir() => 1.0,
                _ => 16.0,
            },
            #[cfg(target_os = "macos")]
            Self::Bulk { entry, .. } => {
                if entry.metadata.is_some() {
                    1.0
                } else {
                    16.0
                }
            }
        }
    }
}

struct BufferedDirectory {
    entries: Vec<io::Result<AnalysisEntry>>,
    truncated: bool,
}

enum DirectoryEntries<'a> {
    Streaming(fs::ReadDir),
    Buffered(std::vec::IntoIter<io::Result<AnalysisEntry>>),
    Prefetched(
        std::vec::IntoIter<io::Result<AnalysisEntry>>,
        Box<DirectoryEntries<'a>>,
    ),
    #[cfg(target_os = "macos")]
    Bulk {
        directory: crate::macos_bulk::BulkDirectory,
        parent: Arc<PathBuf>,
        pending: std::vec::IntoIter<io::Result<crate::macos_bulk::BulkEntry>>,
        cancel: &'a AtomicBool,
    },
    Done(std::marker::PhantomData<&'a AtomicBool>),
}

impl Iterator for DirectoryEntries<'_> {
    type Item = io::Result<AnalysisEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self {
                Self::Streaming(entries) => match guarded_io(|| Ok(entries.next())) {
                    Ok(value) => return value.map(|entry| entry.map(AnalysisEntry::from)),
                    Err(error) => {
                        *self = Self::Done(std::marker::PhantomData);
                        return Some(Err(error));
                    }
                },
                Self::Buffered(entries) => return entries.next(),
                Self::Prefetched(entries, remaining) => {
                    return entries.next().or_else(|| remaining.next())
                }
                #[cfg(target_os = "macos")]
                Self::Bulk {
                    directory,
                    parent,
                    pending,
                    cancel,
                } => {
                    if let Some(entry) = pending.next() {
                        return Some(entry.map(|entry| AnalysisEntry::Bulk {
                            parent: Arc::clone(parent),
                            entry,
                        }));
                    }
                    match directory.next_batch(cancel) {
                        Ok(Some(batch)) => {
                            *pending = batch.into_iter();
                        }
                        Ok(None) | Err(crate::macos_bulk::BulkError::Cancelled) => {
                            *self = Self::Done(std::marker::PhantomData);
                            return None;
                        }
                        Err(crate::macos_bulk::BulkError::Unsupported(_)) => {
                            let result = guarded_io(|| fs::read_dir(parent.as_path()));
                            match result {
                                Ok(entries) => {
                                    *self = Self::Streaming(entries);
                                }
                                Err(error) => {
                                    *self = Self::Done(std::marker::PhantomData);
                                    return Some(Err(error));
                                }
                            }
                        }
                        Err(error) => {
                            let error = bulk_error(error);
                            *self = Self::Done(std::marker::PhantomData);
                            return Some(Err(error));
                        }
                    }
                }
                Self::Done(_) => return None,
            }
        }
    }
}

impl DirectoryEntries<'_> {
    fn prepare_progress(self, depth: usize, cancel: &AtomicBool) -> Self {
        let limit = if depth <= 2 {
            ROOT_PREFETCH_ENTRIES
        } else {
            DEEP_PROGRESS_ENTRIES
        };
        self.prepare_progress_with_limit(cancel, limit)
    }

    fn prepare_progress_with_limit(self, cancel: &AtomicBool, limit: usize) -> Self {
        if matches!(self, Self::Buffered(_)) {
            return self;
        }
        let mut remaining = self;
        let mut entries = Vec::new();
        while entries.len() <= limit && !cancel.load(Ordering::Relaxed) {
            let Some(entry) = remaining.next() else {
                return Self::Buffered(entries.into_iter());
            };
            entries.push(entry);
        }
        // Reuse this directory's reader, never a separate counting pass. Keep
        // the bounded prefix and stream the remainder if its size is unknown.
        Self::Prefetched(entries.into_iter(), Box::new(remaining))
    }

    fn progress_weights(&self, depth: usize) -> Option<Vec<f64>> {
        let Self::Buffered(entries) = self else {
            return None;
        };
        Some(
            entries
                .as_slice()
                .iter()
                .map(|entry| match entry {
                    // Deep standard entries are measured once by the walker.
                    // Do not add file_type/stat calls just to weight progress.
                    Ok(AnalysisEntry::Standard(_)) if depth > 2 => 1.0,
                    Ok(entry) => entry.progress_weight(),
                    Err(_) => 16.0,
                })
                .collect(),
        )
    }
}

fn open_directory_entries<'a>(
    path: &Path,
    cancel: &'a AtomicBool,
) -> io::Result<DirectoryEntries<'a>> {
    #[cfg(target_os = "macos")]
    {
        match crate::macos_bulk::BulkDirectory::open(path) {
            Ok(directory) => {
                return Ok(DirectoryEntries::Bulk {
                    directory,
                    parent: Arc::new(path.to_path_buf()),
                    pending: Vec::new().into_iter(),
                    cancel,
                })
            }
            Err(crate::macos_bulk::BulkError::Unsupported(_)) => {}
            Err(error) => return Err(bulk_error(error)),
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = cancel;
    guarded_io(|| fs::read_dir(path)).map(DirectoryEntries::Streaming)
}

fn directory_entries<'a>(
    path: &Path,
    cancel: &'a AtomicBool,
) -> Result<(DirectoryEntries<'a>, bool), DirectoryReadFailure> {
    if !requires_bounded_enumeration(path) {
        return open_directory_entries(path, cancel)
            .map(|entries| (entries, false))
            .map_err(DirectoryReadFailure::Io);
    }
    let path = path.to_path_buf();
    let buffered = run_directory_worker(cancel, PROTECTED_DIRECTORY_TIMEOUT, move |stop| {
        if stop.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "目录检查已停止。",
            ));
        }
        let mut reader = open_directory_entries(&path, &stop)?;
        let mut entries = Vec::new();
        while entries.len() < PROTECTED_DIRECTORY_ENTRY_LIMIT && !stop.load(Ordering::Relaxed) {
            let Some(entry) = reader.next() else {
                return Ok(BufferedDirectory {
                    entries,
                    truncated: false,
                });
            };
            if stop.load(Ordering::Relaxed) {
                break;
            }
            entries.push(entry);
        }
        Ok(BufferedDirectory {
            truncated: entries.len() >= PROTECTED_DIRECTORY_ENTRY_LIMIT,
            entries,
        })
    })?;
    Ok((
        DirectoryEntries::Buffered(buffered.entries.into_iter()),
        buffered.truncated,
    ))
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
    let metadata = match guarded_io(|| fs::symlink_metadata(path)) {
        Ok(metadata) => metadata,
        Err(error) => {
            ctx.record_error(&error);
            ctx.warn(format!(
                "无法读取 {}：{}。此处占用未知，结果为部分统计。",
                path.display(),
                error
            ));
            return empty_partial();
        }
    };
    measure_metadata(path, metadata, depth, retain, ctx)
}

fn measure_entry<F: Fn(AnalysisProgress)>(
    entry: AnalysisEntry,
    depth: usize,
    retain: bool,
    ctx: &mut Context<'_, F>,
) -> Measured {
    if !ctx.check() {
        return empty_partial();
    }
    #[cfg(target_os = "macos")]
    if let AnalysisEntry::Bulk { entry: bulk, .. } = &entry {
        if let Some(metadata) = bulk.metadata {
            ctx.visits = ctx.visits.saturating_add(1);
            let totals = ctx.record_bulk_file(metadata);
            ctx.complete_work(ctx.work_budget);
            if ctx.progress_due() {
                ctx.emit(&entry.path());
            }
            let displayed = if retain {
                let path = entry.path();
                ctx.displayed_nodes += 1;
                Some(Displayed {
                    nodes: 1,
                    node: DirectoryNode {
                        id: format!("{}-{}", ctx.analysis_id, ctx.visits),
                        path: path.to_str().unwrap_or_default().to_owned(),
                        name: entry.file_name().to_string_lossy().into_owned(),
                        kind: "file".into(),
                        bytes: totals.bytes,
                        files: 1,
                        dirs: 0,
                        children: Vec::new(),
                        has_children: false,
                        partial: false,
                        omitted_children: 0,
                        size_known: true,
                        size_source: "scan".into(),
                        device: metadata.device,
                        inode: metadata.inode,
                    },
                })
            } else {
                None
            };
            return Measured { totals, displayed };
        }
    }
    // DirEntry::metadata does not follow symlinks. On supported Unix platforms
    // it can stat relative to the directory without allocating a full child path.
    let metadata = match entry.metadata() {
        Ok(metadata) => metadata,
        Err(error) => {
            ctx.record_error(&error);
            ctx.warn(format!(
                "无法读取 {}：{}。此处占用未知，结果为部分统计。",
                entry.path().display(),
                error
            ));
            return empty_partial();
        }
    };
    if !retain && !metadata.is_dir() {
        // Deep files contribute to every ancestor's total, but they need no UI
        // node, identifier, name, heap item, or path string in memory.
        ctx.visits = ctx.visits.saturating_add(1);
        let totals = ctx.record_metadata(&metadata, is_cloud_placeholder(&metadata));
        ctx.complete_work(ctx.work_budget);
        if ctx.progress_due() {
            ctx.emit(&entry.path());
        }
        return Measured {
            totals,
            displayed: None,
        };
    }
    measure_metadata(&entry.path(), metadata, depth, retain, ctx)
}

fn measure_metadata<F: Fn(AnalysisProgress)>(
    path: &Path,
    metadata: fs::Metadata,
    depth: usize,
    retain: bool,
    ctx: &mut Context<'_, F>,
) -> Measured {
    let cloud_placeholder = is_cloud_placeholder(&metadata);
    measure_metadata_with_policy(path, metadata, depth, retain, cloud_placeholder, ctx)
}

fn measure_metadata_with_policy<F: Fn(AnalysisProgress)>(
    path: &Path,
    metadata: fs::Metadata,
    depth: usize,
    retain: bool,
    cloud_placeholder: bool,
    ctx: &mut Context<'_, F>,
) -> Measured {
    if !ctx.check() {
        return empty_partial();
    }
    ctx.visits = ctx.visits.saturating_add(1);
    let node_visit = ctx.visits;
    let work_budget = ctx.work_budget;
    let credit_before = ctx.work_credit;
    let symlink = metadata.file_type().is_symlink();
    let directory = metadata.is_dir() && !symlink;
    let cross_device = directory && metadata.dev() != ctx.device;
    let mut totals = ctx.record_metadata(&metadata, cloud_placeholder);
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
        ctx.skipped_mount_count = ctx.skipped_mount_count.saturating_add(1);
        ctx.warn(format!(
            "{} 位于另一文件系统或挂载卷，已跳过其内容；占用未知。",
            path.display()
        ));
    } else if directory && cloud_placeholder {
        // A dataless .epub/.app/package is still a directory. read_dir can ask
        // File Provider to download it, then block inside the OS before the next
        // cancellation check. Leave online contents alone and count only stat's
        // already allocated local blocks.
    } else if directory && depth >= MAX_DEPTH {
        totals.partial = true;
        ctx.depth_limited_count = ctx.depth_limited_count.saturating_add(1);
        ctx.warn(format!(
            "{} 超过安全遍历深度，已跳过其内容；占用未知。",
            path.display()
        ));
    } else if directory {
        match directory_entries(path, ctx.cancel) {
            Err(DirectoryReadFailure::Io(error)) => {
                totals.partial = true;
                ctx.record_error(&error);
                ctx.warn(format!(
                    "无法列出 {}：{}。此目录内容占用未知，结果为部分统计。",
                    path.display(),
                    error
                ));
            }
            Err(failure @ (DirectoryReadFailure::TimedOut | DirectoryReadFailure::WorkerLimit)) => {
                totals.partial = true;
                // Count both actual OS timeouts and directories never enumerated
                // because earlier timed-out workers still occupy all slots.
                ctx.blocked_directory_count = ctx.blocked_directory_count.saturating_add(1);
                ctx.other_error_count = ctx.other_error_count.saturating_add(1);
                let warning = if matches!(failure, DirectoryReadFailure::TimedOut) {
                    format!(
                        "系统未及时返回 {} 的目录内容，已跳过。可以检查完整磁盘访问权限后重扫；此处占用未知。",
                        path.display()
                    )
                } else {
                    format!(
                        "此前的受保护目录仍未返回，等待线程已达到限额；{} 暂未读取。可以检查完整磁盘访问权限后重扫；此处占用未知。",
                        path.display()
                    )
                };
                ctx.warn(warning);
            }
            Err(DirectoryReadFailure::Cancelled) => {
                totals.partial = true;
                ctx.stopped = true;
            }
            Ok((entries, truncated)) => {
                let mut entries = entries.prepare_progress(depth, ctx.cancel);
                let weights = entries.progress_weights(depth);
                let total_weight = weights
                    .as_ref()
                    .map(|weights| weights.iter().sum::<f64>())
                    .unwrap_or(0.0);
                if depth == 0 {
                    ctx.estimate_ready = weights.is_some() && !truncated;
                }
                let unknown_stream = weights.is_none();
                if unknown_stream {
                    // The amount of remaining work is unknown for this reader.
                    // Continue the actual walk without showing a fixed percent.
                    ctx.unknown_work_streams += 1;
                }
                // Reserve completion credit once at the selected root, rather
                // than shrinking every descendant's budget at every depth.
                let child_budget = work_budget * if depth == 0 { 0.9 } else { 1.0 };
                let mut completed_entries = 0_u64;
                if truncated {
                    totals.partial = true;
                    ctx.other_error_count = ctx.other_error_count.saturating_add(1);
                    ctx.warn(format!(
                        "{} 的受保护目录项目超过 {} 项，未列出的内容占用未知。",
                        path.display(),
                        PROTECTED_DIRECTORY_ENTRY_LIMIT
                    ));
                }
                loop {
                    if !ctx.check() {
                        totals.partial = true;
                        break;
                    }
                    let Some(entry) = entries.next() else {
                        break;
                    };
                    let position = completed_entries;
                    completed_entries += 1;
                    ctx.work_budget = match &weights {
                        Some(weights) if total_weight > 0.0 => {
                            child_budget * weights[position as usize] / total_weight
                        }
                        // Credit only observed entries while an unknown reader
                        // remains indeterminate. Actual exhaustion settles its
                        // remaining budget; no time or file-byte guesses apply.
                        _ => {
                            child_budget
                                / (completed_entries as f64 * (completed_entries + 1) as f64)
                        }
                    };
                    let entry = match entry {
                        Ok(entry) => entry,
                        Err(error) => {
                            totals.partial = true;
                            ctx.record_error(&error);
                            ctx.warn(format!(
                                "无法读取 {} 的某个子项：{}。部分内容占用未知。",
                                path.display(),
                                error
                            ));
                            continue;
                        }
                    };
                    totals.children += 1;
                    let child = measure_entry(entry, depth + 1, show_children, ctx);
                    totals.bytes = totals.bytes.saturating_add(child.totals.bytes);
                    totals.files = totals.files.saturating_add(child.totals.files);
                    totals.dirs = totals.dirs.saturating_add(child.totals.dirs);
                    totals.partial |= child.totals.partial;
                    if let Some(displayed) = child.displayed {
                        largest.push(displayed);
                        if largest.len() > ctx.child_cap {
                            if let Some(omitted) = largest.pop() {
                                ctx.displayed_nodes =
                                    ctx.displayed_nodes.saturating_sub(omitted.nodes);
                            }
                        }
                    }
                }
                if unknown_stream {
                    ctx.unknown_work_streams -= 1;
                }
            }
        }
        // The walk is not a filesystem snapshot; flag directories changing while measured.
        match guarded_io(|| fs::symlink_metadata(path)) {
            Ok(after)
                if after.dev() == metadata.dev()
                    && after.ino() == metadata.ino()
                    && after.mtime() == metadata.mtime()
                    && after.mtime_nsec() == metadata.mtime_nsec()
                    && after.ctime() == metadata.ctime()
                    && after.ctime_nsec() == metadata.ctime_nsec() => {}
            after => {
                totals.partial = true;
                ctx.changed_directory_count = ctx.changed_directory_count.saturating_add(1);
                if let Err(error) = after {
                    ctx.record_error(&error);
                }
                ctx.warn(format!(
                    "{} 在分析期间发生变化；其占用仅为部分时间点的统计。",
                    path.display()
                ));
            }
        }
    }
    let has_children = directory && !cloud_placeholder && (totals.children > 0 || totals.partial);
    if !totals.partial {
        ctx.complete_work((work_budget - (ctx.work_credit - credit_before)).max(0.0));
    }
    let size_known = !totals.partial || totals.bytes > 0;
    if directory && ctx.summaries.should_consider(totals.bytes, path) {
        if let Some(path_string) = path.to_str() {
            ctx.summaries.consider(DirectorySummary {
                id: format!("{}-{node_visit}", ctx.analysis_id),
                path: path_string.to_owned(),
                name: path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "/".into()),
                bytes: totals.bytes,
                files: totals.files,
                dirs: totals.dirs,
                has_children,
                partial: totals.partial,
                device: metadata.dev(),
                inode: metadata.ino(),
                size_known,
            });
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
                id: format!("{}-{node_visit}", ctx.analysis_id),
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
                has_children,
                partial: totals.partial,
                omitted_children,
                size_known,
                size_source: "scan".into(),
                device: metadata.dev(),
                inode: metadata.ino(),
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
    let path =
        guarded_io(|| path.canonicalize()).map_err(|error| format!("无法打开选定目录：{error}"))?;
    let metadata = guarded_io(|| fs::symlink_metadata(&path))
        .map_err(|error| format!("无法读取选定目录：{error}"))?;
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
    let mut ctx = Context::new(cancel, progress, analysis_id.clone(), metadata.dev());
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
            size_known: false,
            size_source: "unknown".into(),
            device: metadata.dev(),
            inode: metadata.ino(),
        });
    let scan_complete = !root.partial && !cancelled;
    // A short scan may finish inside the 200 ms throttle. Its final work-based
    // estimate remains <=99; completion is communicated by the actual report.
    ctx.last_progress = None;
    ctx.emit(&path);
    let measured_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let summary_index = Arc::new(ctx.summaries.finish(
        analysis_id.clone(),
        path.to_string_lossy().into_owned(),
        metadata.dev(),
        measured_at,
    ));
    debug_assert!(summary_index.len() <= DIRECTORY_SUMMARIES);
    Ok(AnalysisReport {
        source_analysis_id: analysis_id.clone(),
        analysis_id,
        root,
        duration_ms: ctx.started.elapsed().as_millis() as u64,
        scanned_files: ctx.files,
        warnings: ctx.warnings,
        cancelled,
        total_bytes,
        available_bytes,
        scan_complete,
        permission_denied_count: ctx.permission_denied_count,
        other_error_count: ctx.other_error_count,
        skipped_mount_count: ctx.skipped_mount_count,
        changed_directory_count: ctx.changed_directory_count,
        depth_limited_count: ctx.depth_limited_count,
        cloud_placeholder_count: ctx.cloud_placeholder_count,
        blocked_directory_count: ctx.blocked_directory_count,
        cached_browse: false,
        measured_at,
        summary_index,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::path::PathBuf;
    use std::sync::Mutex;

    static DIRECTORY_WORKER_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn wait_for_directory_workers() {
        let deadline = Instant::now() + Duration::from_secs(1);
        while ACTIVE_DIRECTORY_WORKERS.load(Ordering::Acquire) > 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(ACTIVE_DIRECTORY_WORKERS.load(Ordering::Acquire), 0);
    }

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
        assert!(report.scan_complete);
        assert_eq!(report.permission_denied_count, 0);
        assert_eq!(report.other_error_count, 0);
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
        let mut ctx = Context::new(
            &cancel,
            |_| {},
            "fixture".into(),
            fs::metadata(&fixture.0).unwrap().dev(),
        );
        ctx.child_cap = 1;
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
        assert!(!report.scan_complete);
        assert_eq!(report.scanned_files, 0);
        let restricted = fixture.0.join("restricted");
        fs::set_permissions(&restricted, fs::Permissions::from_mode(0o0)).unwrap();
        if fs::read_dir(&restricted).is_err() {
            let report = analyze(&fixture.0, &AtomicBool::new(false), |_| {}).unwrap();
            assert!(report.root.partial);
            assert!(!report.scan_complete);
            assert!(report.permission_denied_count > 0);
            assert!(report
                .warnings
                .iter()
                .any(|warning| warning.contains("未知")));
            assert_eq!(report.scanned_files, 0);
        }
        fs::set_permissions(&restricted, fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn traversal_continues_past_the_former_time_and_entry_limits() {
        let fixture = Fixture::new();
        let file = fixture.file("a/b/c/d/remaining.bin", 32_768);
        let cancel = AtomicBool::new(false);
        let mut ctx = Context::new(
            &cancel,
            |_| {},
            "fixture".into(),
            fs::metadata(&fixture.0).unwrap().dev(),
        );
        // Advance the counters without creating millions of redundant fixtures.
        ctx.visits = 2_000_000;
        ctx.started = Instant::now() - Duration::from_secs(301);
        let measured = measure(&fixture.0, 0, true, &mut ctx);
        assert!(!measured.totals.partial);
        assert_eq!(measured.totals.files, 1);
        assert!(measured.totals.bytes >= blocks(&file));
        assert_eq!(ctx.visits, 2_000_006);
        assert!(!ctx.stopped);
        assert!(ctx.warnings.is_empty());
    }

    #[test]
    fn hard_link_blocks_are_counted_once_across_sibling_folders() {
        let fixture = Fixture::new();
        let original = fixture.file("left/large.bin", 32_768);
        fs::create_dir(fixture.0.join("right")).unwrap();
        fs::hard_link(&original, fixture.0.join("right/alias.bin")).unwrap();
        let report = analyze(&fixture.0, &AtomicBool::new(false), |_| {}).unwrap();
        let expected = blocks(&fixture.0)
            + blocks(&fixture.0.join("left"))
            + blocks(&fixture.0.join("right"))
            + blocks(&original);
        assert_eq!(report.root.bytes, expected);
        assert_eq!(report.root.files, 2);
        assert_eq!(report.scanned_files, 2);
        assert!(report.scan_complete);
        assert_eq!(
            report
                .root
                .children
                .iter()
                .flat_map(|folder| &folder.children)
                .filter(|node| node.bytes > 0)
                .count(),
            1
        );
    }

    #[test]
    fn errors_and_safety_skips_have_distinct_diagnostics() {
        let fixture = Fixture::new();
        fixture.file("child/kept.bin", 4_096);
        let cancel = AtomicBool::new(false);
        let mut ctx = Context::new(
            &cancel,
            |_| {},
            "fixture".into(),
            fs::metadata(&fixture.0).unwrap().dev(),
        );
        ctx.record_error(&io::Error::from_raw_os_error(libc::EACCES));
        ctx.record_error(&io::Error::from_raw_os_error(libc::EPERM));
        ctx.record_error(&io::Error::from_raw_os_error(libc::ENOENT));
        assert_eq!(ctx.permission_denied_count, 2);
        assert_eq!(ctx.other_error_count, 1);

        let measured = measure(&fixture.0, MAX_DEPTH, true, &mut ctx);
        assert!(measured.totals.partial);
        assert_eq!(ctx.depth_limited_count, 1);
        assert_eq!(measured.totals.files, 0);

        ctx.device = ctx.device.wrapping_add(1);
        let measured = measure(&fixture.0, 0, true, &mut ctx);
        assert!(measured.totals.partial);
        assert_eq!(ctx.skipped_mount_count, 1);
        assert_eq!(measured.totals.bytes, 0);
    }

    #[test]
    fn cancellation_is_checked_between_undisplayed_file_entries() {
        let fixture = Fixture::new();
        fixture.file("first.bin", 4_096);
        fixture.file("second.bin", 4_096);
        let cancel = AtomicBool::new(false);
        let mut ctx = Context::new(
            &cancel,
            |_| cancel.store(true, Ordering::Relaxed),
            "fixture".into(),
            fs::metadata(&fixture.0).unwrap().dev(),
        );
        let mut entries = fs::read_dir(&fixture.0).unwrap();
        let first = measure_entry(entries.next().unwrap().unwrap().into(), 4, false, &mut ctx);
        assert_eq!(first.totals.files, 1);
        assert!(first.displayed.is_none());
        assert!(cancel.load(Ordering::Relaxed));
        let second = measure_entry(entries.next().unwrap().unwrap().into(), 4, false, &mut ctx);
        assert!(second.totals.partial);
        assert_eq!(second.totals.files, 0);
        assert_eq!(ctx.files, 1);
    }

    #[test]
    fn directory_changes_during_a_scan_are_reported() {
        let fixture = Fixture::new();
        let changed = AtomicBool::new(false);
        let report = analyze(&fixture.0, &AtomicBool::new(false), |_| {
            if !changed.swap(true, Ordering::Relaxed) {
                fixture.file("arrived.bin", 4_096);
            }
        })
        .unwrap();
        assert_eq!(report.scanned_files, 1);
        assert_eq!(report.changed_directory_count, 1);
        assert!(report.root.partial);
        assert!(!report.scan_complete);
    }

    #[test]
    fn macos_dataless_flag_policy_excludes_other_flags() {
        assert!(flags_are_cloud_placeholder(SF_DATALESS));
        // Actual File Provider directories can include unrelated flags too.
        assert!(flags_are_cloud_placeholder(0x4000_0060));
        assert!(!flags_are_cloud_placeholder(0x0000_0060));
        assert!(!flags_are_cloud_placeholder(0));
    }

    #[test]
    fn online_cloud_directories_are_not_enumerated() {
        let fixture = Fixture::new();
        fixture.file("online.epub/chapter/content.bin", 32_768);
        let online = fixture.0.join("online.epub");
        let metadata = fs::symlink_metadata(&online).unwrap();
        let known_local_blocks = metadata.blocks().saturating_mul(512);
        let cancel = AtomicBool::new(false);
        let mut ctx = Context::new(&cancel, |_| {}, "fixture".into(), metadata.dev());
        // SF_DATALESS is controlled by File Provider and cannot safely be set
        // on an ordinary fixture. Inject the same policy decision after stat.
        let measured = measure_metadata_with_policy(&online, metadata, 0, true, true, &mut ctx);
        let node = measured.displayed.unwrap().node;
        assert_eq!(ctx.visits, 1);
        assert_eq!(ctx.cloud_placeholder_count, 1);
        assert_eq!(node.bytes, known_local_blocks);
        assert_eq!(node.files, 0);
        assert_eq!(node.omitted_children, 0);
        assert!(node.children.is_empty());
        assert!(!node.has_children);
        assert!(node.partial);
        assert!(ctx
            .warnings
            .iter()
            .any(|warning| warning.contains("在线云盘")));
    }

    #[test]
    fn online_files_count_only_already_allocated_blocks() {
        let fixture = Fixture::new();
        let file = fixture.file("online.bin", 4_096);
        let metadata = fs::symlink_metadata(&file).unwrap();
        let known_local_blocks = metadata.blocks().saturating_mul(512);
        let cancel = AtomicBool::new(false);
        let mut ctx = Context::new(&cancel, |_| {}, "fixture".into(), metadata.dev());
        let measured = measure_metadata_with_policy(&file, metadata, 0, true, true, &mut ctx);
        assert_eq!(measured.totals.bytes, known_local_blocks);
        assert_eq!(measured.totals.files, 1);
        assert!(!measured.totals.partial);
        assert_eq!(ctx.cloud_placeholder_count, 1);
    }

    #[test]
    fn entire_container_trees_use_bounded_enumeration() {
        assert!(requires_bounded_enumeration(Path::new(
            "/Users/example/Library/Group Containers"
        )));
        assert!(requires_bounded_enumeration(Path::new(
            "/System/Volumes/Data/Users/example/Library/Containers"
        )));
        assert!(requires_bounded_enumeration(Path::new(
            "/Users/example/Library/Group Containers/example.app"
        )));
        assert!(requires_bounded_enumeration(Path::new(
            "/System/Volumes/Data/Users/example/Library/Containers/example.app/Data/Library/Caches"
        )));
        assert!(requires_bounded_enumeration(Path::new(
            "/Users/example/Library/Group Containers/example.app/subfolder/leaf"
        )));
        assert!(!requires_bounded_enumeration(Path::new(
            "/example/Group Containers"
        )));
        assert!(requires_bounded_enumeration(Path::new(
            "/Users/example/Library/Caches"
        )));
        assert!(!requires_bounded_enumeration(Path::new(
            "/example/Library/Containers-backup/child"
        )));
    }

    #[test]
    fn personal_data_trees_are_protected_without_matching_unrelated_names() {
        for folder in [
            "Desktop",
            "Documents",
            "Downloads",
            "Music",
            "Movies",
            "Pictures",
            "Library",
        ] {
            assert!(requires_bounded_enumeration(Path::new(&format!(
                "/Users/example/{folder}"
            ))));
            assert!(requires_bounded_enumeration(Path::new(&format!(
                "/System/Volumes/Data/Users/example/{folder}/deep/subfolder"
            ))));
            assert!(!requires_bounded_enumeration(Path::new(&format!(
                "/example/{folder}/deep/subfolder"
            ))));
        }
        for folder in ["Code", ".cargo", "go", "Applications", "Music-backup"] {
            assert!(!requires_bounded_enumeration(Path::new(&format!(
                "/Users/example/{folder}/deep/subfolder"
            ))));
        }
        assert!(requires_bounded_enumeration(Path::new(
            "/Users/Users/Music/album"
        )));
        assert!(!requires_bounded_enumeration(Path::new(
            "/Users/../Music/album"
        )));
    }

    #[test]
    fn accessible_container_roots_are_fully_counted() {
        let _lock = DIRECTORY_WORKER_TEST_LOCK.lock().unwrap();
        wait_for_directory_workers();
        let fixture = Fixture::new();
        fixture.file("Library/Group Containers/example.app/content.bin", 32_768);
        let report = analyze(
            &fixture.0.join("Library/Group Containers"),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert_eq!(report.scanned_files, 1);
        assert!(report.scan_complete);
        assert_eq!(report.blocked_directory_count, 0);
        assert_eq!(report.other_error_count, 0);
        wait_for_directory_workers();
    }

    #[test]
    fn accessible_personal_subdirectories_are_fully_counted() {
        let _lock = DIRECTORY_WORKER_TEST_LOCK.lock().unwrap();
        wait_for_directory_workers();
        let fixture = Fixture::new();
        fixture.file("Users/example/Music/album/disc/track.bin", 32_768);
        let report = analyze(
            &fixture.0.join("Users/example/Music"),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert_eq!(report.scanned_files, 1);
        assert_eq!(report.root.dirs, 3);
        assert!(report.scan_complete);
        assert_eq!(report.blocked_directory_count, 0);
        assert_eq!(report.other_error_count, 0);
        wait_for_directory_workers();
    }

    #[test]
    fn blocked_workers_return_promptly_and_repeated_scans_are_bounded() {
        let _lock = DIRECTORY_WORKER_TEST_LOCK.lock().unwrap();
        wait_for_directory_workers();
        struct ReleaseOnDrop(Arc<AtomicBool>);
        impl Drop for ReleaseOnDrop {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Relaxed);
            }
        }
        let release = ReleaseOnDrop(Arc::new(AtomicBool::new(false)));
        let cancel = AtomicBool::new(false);
        for _ in 0..MAX_DIRECTORY_WORKERS {
            let blocked = Arc::clone(&release.0);
            let started = Instant::now();
            let result = run_directory_worker(&cancel, Duration::from_millis(5), move |_| {
                // Simulate a kernel iterator that ignores cancellation until the
                // OS returns. Only the waiting caller must return promptly.
                while !blocked.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(1));
                }
                Ok(())
            });
            assert!(matches!(result, Err(DirectoryReadFailure::TimedOut)));
            assert!(started.elapsed() < Duration::from_millis(500));
        }
        assert_eq!(
            ACTIVE_DIRECTORY_WORKERS.load(Ordering::Acquire),
            MAX_DIRECTORY_WORKERS
        );
        let result = run_directory_worker(&cancel, Duration::from_millis(5), |_| Ok(()));
        assert!(matches!(result, Err(DirectoryReadFailure::WorkerLimit)));
        drop(release);
        wait_for_directory_workers();
    }

    #[test]
    fn directory_wait_observes_cancellation_without_waiting_for_the_os() {
        let _lock = DIRECTORY_WORKER_TEST_LOCK.lock().unwrap();
        wait_for_directory_workers();
        let cancel = Arc::new(AtomicBool::new(false));
        let delayed_cancel = Arc::clone(&cancel);
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            delayed_cancel.store(true, Ordering::Relaxed);
        });
        let started = Instant::now();
        let result = run_directory_worker(&cancel, Duration::from_secs(1), |stop| {
            while !stop.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(1));
            }
            Ok(())
        });
        assert!(matches!(result, Err(DirectoryReadFailure::Cancelled)));
        assert!(started.elapsed() < Duration::from_millis(500));
        canceller.join().unwrap();
        wait_for_directory_workers();
    }

    #[test]
    fn cached_browse_lists_only_immediate_entries_and_keeps_source_directory_totals() {
        let fixture = Fixture::new();
        fixture.file("a/b/c/d/inside/old.bin", 32_768);
        let full = analyze(&fixture.0, &AtomicBool::new(false), |_| {}).unwrap();
        let target = &full.root.children[0].children[0].children[0];
        assert!(target.children.is_empty());
        let selected = directory_for_browse(&full, &target.id).unwrap();
        let cached_bytes = selected.bytes;
        // Changing a grandchild after the full scan proves browse does not walk
        // it again, even though its existing summary is retained for display.
        fixture.file("a/b/c/d/inside/arrived.bin", 65_536);
        let browsed = browse(
            selected,
            Arc::clone(&full.summary_index),
            full.total_bytes,
            full.available_bytes,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(browsed.cached_browse);
        assert_eq!(browsed.root.id, target.id);
        assert_eq!(browsed.root.bytes, cached_bytes);
        assert_eq!(browsed.root.size_source, "cached");
        assert_eq!(browsed.scanned_files, 0);
        assert_eq!(browsed.root.children.len(), 1);
        let directory = &browsed.root.children[0];
        assert_eq!(directory.name, "d");
        assert!(directory.children.is_empty());
        assert_eq!(directory.files, 1);
        assert!(directory.size_known);
        assert_eq!(directory.size_source, "cached");
        assert_eq!(browsed.measured_at, full.measured_at);
        assert_eq!(browsed.source_analysis_id, full.analysis_id);
        assert!(Arc::ptr_eq(&browsed.summary_index, &full.summary_index));
        assert!(fixture.0.join("a/b/c/d/inside/old.bin").exists());
        assert!(fixture.0.join("a/b/c/d/inside/arrived.bin").exists());
        let value = serde_json::to_value(&browsed).unwrap();
        assert!(value.get("summaryIndex").is_none());
        assert!(value.get("storage").is_none());
        assert!(value["root"].get("device").is_none());
    }

    #[test]
    fn uncached_new_directories_have_unknown_sizes_and_can_be_opened_shallowly() {
        let fixture = Fixture::new();
        fixture.file("known/file.bin", 4_096);
        let full = analyze(&fixture.0, &AtomicBool::new(false), |_| {}).unwrap();
        fixture.file("new/deep/never-recursed.bin", 32_768);
        let browsed = browse(
            directory_for_browse(&full, &full.root.id).unwrap(),
            Arc::clone(&full.summary_index),
            full.total_bytes,
            full.available_bytes,
            &AtomicBool::new(false),
        )
        .unwrap();
        let unknown = browsed
            .root
            .children
            .iter()
            .find(|node| node.name == "new")
            .unwrap();
        assert!(!unknown.size_known);
        assert_eq!(unknown.size_source, "unknown");
        assert!(unknown.partial);
        assert_eq!(unknown.bytes, 0);
        assert!(unknown.has_children);
        let next = browse(
            directory_for_browse(&browsed, &unknown.id).unwrap(),
            Arc::clone(&browsed.summary_index),
            full.total_bytes,
            full.available_bytes,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(next.root.children.len(), 1);
        assert_eq!(next.root.children[0].name, "deep");
        assert!(!next.root.children[0].size_known);
        assert_eq!(next.scanned_files, 0);
        assert!(!next.root.size_known);
    }

    #[test]
    fn browse_authorization_rejects_hidden_ids_other_scopes_and_replaced_directories() {
        let fixture = Fixture::new();
        fixture.file("a/b/c/d/inside.bin", 4_096);
        let full = analyze(&fixture.0, &AtomicBool::new(false), |_| {}).unwrap();
        let hidden = full
            .summary_index
            .lookup(fixture.0.join("a/b/c/d").to_str().unwrap())
            .unwrap();
        assert!(directory_for_browse(&full, &hidden.id).is_err());
        assert!(directory_for_browse(&full, "not-a-cached-id").is_err());
        let mut outside = full.clone();
        outside.root.path = format!("{}-old", fixture.0.display());
        assert!(directory_for_browse(&outside, &outside.root.id).is_err());
        let target = &full.root.children[0];
        let selected = directory_for_browse(&full, &target.id).unwrap();
        fs::rename(fixture.0.join("a"), fixture.0.join("original-a")).unwrap();
        fs::create_dir(fixture.0.join("a")).unwrap();
        assert!(browse(
            selected,
            Arc::clone(&full.summary_index),
            full.total_bytes,
            full.available_bytes,
            &AtomicBool::new(false)
        )
        .is_err());
        assert!(fixture.0.join("original-a/b/c/d/inside.bin").exists());
    }

    #[test]
    fn cancelled_browse_preserves_cached_totals_without_reading_descendants() {
        let fixture = Fixture::new();
        fixture.file("a/b/c/keep.bin", 4_096);
        let full = analyze(&fixture.0, &AtomicBool::new(false), |_| {}).unwrap();
        let cancelled = browse(
            directory_for_browse(&full, &full.root.id).unwrap(),
            Arc::clone(&full.summary_index),
            full.total_bytes,
            full.available_bytes,
            &AtomicBool::new(true),
        )
        .unwrap();
        assert!(cancelled.cancelled);
        assert!(cancelled.root.children.is_empty());
        assert_eq!(cancelled.root.bytes, full.root.bytes);
        assert_eq!(cancelled.scanned_files, 0);
        assert!(fixture.0.join("a/b/c/keep.bin").exists());
    }

    #[test]
    fn exhausted_directory_workers_fall_back_to_authorized_cached_directories_only() {
        let _lock = DIRECTORY_WORKER_TEST_LOCK.lock().unwrap();
        wait_for_directory_workers();
        let fixture = Fixture::new();
        fixture.file("Users/example/Library/direct/grandchild/keep.bin", 32_768);
        fixture.file("Users/example/Library/other/keep.bin", 8_192);
        fixture.file("Users/example/Library/direct-file.log", 4_096);
        let full = analyze(&fixture.0, &AtomicBool::new(false), |_| {}).unwrap();
        wait_for_directory_workers();
        let target = &full.root.children[0].children[0].children[0];
        assert_eq!(target.name, "Library");
        fixture.file("Users/example/Library/new/uncached.bin", 16_384);
        // Reserve all slots without blocking any real operating-system worker.
        let slots: Vec<_> = (0..MAX_DIRECTORY_WORKERS)
            .map(|_| DirectoryWorkerSlot::reserve().unwrap())
            .collect();
        let browsed = browse(
            directory_for_browse(&full, &target.id).unwrap(),
            Arc::clone(&full.summary_index),
            full.total_bytes,
            full.available_bytes,
            &AtomicBool::new(false),
        )
        .unwrap();
        drop(slots);
        assert_eq!(browsed.blocked_directory_count, 1);
        assert_eq!(browsed.other_error_count, 1);
        assert!(browsed.root.partial);
        assert!(browsed.cached_browse);
        assert_eq!(browsed.root.bytes, target.bytes);
        assert_eq!(browsed.scanned_files, 0);
        let names: HashSet<_> = browsed
            .root
            .children
            .iter()
            .map(|node| node.name.as_str())
            .collect();
        assert_eq!(names, HashSet::from(["direct", "other"]));
        assert!(browsed
            .warnings
            .iter()
            .any(|warning| warning.contains("列表可能有遗漏")));
        assert!(Arc::ptr_eq(&browsed.summary_index, &full.summary_index));
        for node in &browsed.root.children {
            assert_eq!(node.kind, "directory");
            assert_eq!(node.size_source, "cached");
            assert!(node.children.is_empty());
            assert_eq!(node.id, full.summary_index.lookup(&node.path).unwrap().id);
            assert!(directory_for_browse(&browsed, &node.id).is_ok());
        }
        assert!(fixture
            .0
            .join("Users/example/Library/direct/grandchild/keep.bin")
            .exists());
        assert!(fixture
            .0
            .join("Users/example/Library/new/uncached.bin")
            .exists());
        wait_for_directory_workers();
    }

    #[test]
    fn cached_directories_fallback_is_bounded_and_uses_exact_parent_and_scope() {
        let fixture = Fixture::new();
        fixture.file("parent/child/grandchild/keep.bin", 4_096);
        fixture.file("parent-old/sibling/keep.bin", 4_096);
        let full = analyze(&fixture.0, &AtomicBool::new(false), |_| {}).unwrap();
        let parent = fixture.0.join("parent");
        let (rows, count) =
            cached_direct_subdirs(&full.summary_index, &parent, &AtomicBool::new(false));
        assert_eq!(count, 1);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows.peek().unwrap().node.path,
            parent.join("child").to_str().unwrap()
        );
        assert!(cached_direct_subdirs(
            &full.summary_index,
            Path::new(&format!("{}-old", fixture.0.display())),
            &AtomicBool::new(false)
        )
        .0
        .is_empty());
        assert!(
            cached_direct_subdirs(&full.summary_index, &parent, &AtomicBool::new(true))
                .0
                .is_empty()
        );

        let mut index = (*full.summary_index).clone();
        let sample = index
            .lookup(parent.join("child").to_str().unwrap())
            .unwrap()
            .clone();
        for entry in 0..DISPLAY_CHILDREN + 5 {
            let mut summary = sample.clone();
            summary.path = parent
                .join(format!("cached-{entry}"))
                .to_str()
                .unwrap()
                .to_owned();
            summary.id = format!("cached-id-{entry}");
            summary.bytes = entry as u64;
            index.entries.insert(summary.path.clone(), summary);
        }
        let (rows, count) = cached_direct_subdirs(&index, &parent, &AtomicBool::new(false));
        assert_eq!(count, DISPLAY_CHILDREN as u64 + 6);
        assert_eq!(rows.len(), DISPLAY_CHILDREN);
        let mut foreign = sample.clone();
        foreign.path = parent.join("foreign").to_str().unwrap().to_owned();
        foreign.device = index.device.wrapping_add(1);
        index.entries.insert(foreign.path.clone(), foreign);
        let (_, after) = cached_direct_subdirs(&index, &parent, &AtomicBool::new(false));
        assert_eq!(after, count);
    }

    #[test]
    fn browse_rejects_an_ancestor_symlink_even_when_the_final_directory_identity_matches() {
        let fixture = Fixture::new();
        fixture.file("scope/parent/child/keep.bin", 4_096);
        let scope = fixture.0.join("scope");
        let full = analyze(&scope, &AtomicBool::new(false), |_| {}).unwrap();
        let child = &full.root.children[0].children[0];
        let selected = directory_for_browse(&full, &child.id).unwrap();
        let identity = selected.filesystem_identity();
        let outside = fixture.0.join("outside-parent");
        fs::rename(scope.join("parent"), &outside).unwrap();
        symlink(&outside, scope.join("parent")).unwrap();
        let final_metadata = fs::symlink_metadata(&selected.path).unwrap();
        assert!(!final_metadata.file_type().is_symlink());
        assert_eq!((final_metadata.dev(), final_metadata.ino()), identity);
        let error = browse(
            selected,
            Arc::clone(&full.summary_index),
            full.total_bytes,
            full.available_bytes,
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(error.starts_with("ANALYSIS_NODE_CHANGED:"));
        assert!(outside.join("child/keep.bin").exists());
    }

    #[test]
    fn missing_browse_paths_are_distinguished_from_permission_and_changed_path_errors() {
        let fixture = Fixture::new();
        fixture.file("parent/child/keep.bin", 4_096);
        let full = analyze(&fixture.0, &AtomicBool::new(false), |_| {}).unwrap();
        let child = &full.root.children[0].children[0];
        let selected = directory_for_browse(&full, &child.id).unwrap();
        fs::remove_dir_all(fixture.0.join("parent/child")).unwrap();
        let error = browse(
            selected,
            Arc::clone(&full.summary_index),
            full.total_bytes,
            full.available_bytes,
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert_eq!(error, "ANALYSIS_NODE_MISSING:此目录已被移动或删除。");
        let denied = browse_path_error(io::Error::from_raw_os_error(libc::EACCES));
        assert!(!denied.starts_with("ANALYSIS_NODE_MISSING:"));
        assert!(denied.contains("访问权限"));
        assert!(
            browse_path_error(io::Error::from_raw_os_error(libc::ENOTDIR))
                .starts_with("ANALYSIS_NODE_CHANGED:")
        );
    }

    #[test]
    fn fresh_favorite_locations_open_direct_entries_without_a_recursive_analysis() {
        let fixture = Fixture::new();
        fixture.file("folder/deep/never-recursed.bin", 32_768);
        fixture.file("direct.bin", 4_096);
        let opened = browse_location(&fixture.0, &AtomicBool::new(false)).unwrap();
        assert!(opened.cached_browse);
        assert!(!opened.root.size_known);
        assert_eq!(opened.root.size_source, "unknown");
        assert_eq!(opened.measured_at, 0);
        assert_eq!(opened.summary_index.len(), 0);
        assert_eq!(opened.scanned_files, 1);
        assert_eq!(opened.root.children.len(), 2);
        let folder = opened
            .root
            .children
            .iter()
            .find(|node| node.name == "folder")
            .unwrap();
        assert!(!folder.size_known);
        assert!(folder.children.is_empty());
        assert!(directory_for_browse(&opened, &folder.id).is_ok());
        let cancelled = browse_location(&fixture.0, &AtomicBool::new(true)).unwrap();
        assert!(cancelled.cancelled);
        assert!(cancelled.root.children.is_empty());
        assert!(fixture.0.join("folder/deep/never-recursed.bin").exists());
    }

    #[test]
    fn permanent_directory_pool_reuses_no_more_than_four_worker_threads() {
        let _lock = DIRECTORY_WORKER_TEST_LOCK.lock().unwrap();
        wait_for_directory_workers();
        let cancel = AtomicBool::new(false);
        let pool = DirectoryWorkerPool::shared().unwrap() as *const DirectoryWorkerPool;
        let mut threads = HashSet::new();
        for _ in 0..200 {
            threads.insert(
                run_directory_worker(&cancel, Duration::from_secs(1), |_| {
                    Ok(thread::current().id())
                })
                .unwrap(),
            );
            assert_eq!(
                DirectoryWorkerPool::shared().unwrap() as *const DirectoryWorkerPool,
                pool
            );
        }
        assert!(threads.len() <= MAX_DIRECTORY_WORKERS);
        wait_for_directory_workers();
    }

    #[test]
    fn root_prefetch_overflow_preserves_the_entire_stream_without_an_estimate() {
        let fixture = Fixture::new();
        for file in 0..20 {
            fixture.file(&format!("file-{file}"), 32);
        }
        let cancel = AtomicBool::new(false);
        let reader = open_directory_entries(&fixture.0, &cancel).unwrap();
        let reader = reader.prepare_progress_with_limit(&cancel, 2);
        assert!(reader.progress_weights(0).is_none());
        let names: HashSet<_> = reader.map(|entry| entry.unwrap().file_name()).collect();
        assert_eq!(names.len(), 20);
    }

    #[test]
    fn deep_leaf_work_keeps_its_credit_through_a_long_directory_chain() {
        let fixture = Fixture::new();
        let chain = (0..24)
            .map(|depth| format!("level-{depth}"))
            .collect::<Vec<_>>()
            .join("/");
        for file in 0..128 {
            fixture.file(&format!("{chain}/file-{file}.bin"), 32);
        }
        let updates = std::cell::RefCell::new(Vec::new());
        let cancel = AtomicBool::new(false);
        let mut ctx = Context::new(
            &cancel,
            |progress| updates.borrow_mut().push(progress),
            "deep-progress".into(),
            fs::metadata(&fixture.0).unwrap().dev(),
        );
        ctx.unthrottled_progress = true;
        let measured = measure(&fixture.0, 0, true, &mut ctx);
        assert_eq!(measured.totals.files, 128);
        assert!(!measured.totals.partial);
        let updates = updates.borrow();
        let half_done = updates
            .iter()
            .find(|progress| progress.scanned_files == 64)
            .unwrap();
        assert!(half_done.estimated_percent.unwrap() >= 40.0);
        let last_leaf = updates
            .iter()
            .rev()
            .find(|progress| progress.scanned_files == 128)
            .unwrap();
        assert!(last_leaf.estimated_percent.unwrap() >= 85.0);
        assert!(last_leaf.estimated_percent.unwrap() < 99.0);
        let estimates: Vec<_> = updates
            .iter()
            .filter_map(|progress| progress.estimated_percent)
            .collect();
        assert!(estimates.windows(2).all(|pair| pair[0] <= pair[1]));
        assert_eq!(ctx.estimated_percent(), Some(99.0));
    }

    #[test]
    fn deep_entry_progress_is_independent_of_file_bytes() {
        let mut first_entry_estimates = Vec::new();
        let mut measured_bytes = Vec::new();
        for file_size in [32, 1_048_576] {
            let fixture = Fixture::new();
            fixture.file("a/b/c/d/e/one.bin", file_size);
            fixture.file("a/b/c/d/e/two.bin", file_size);
            let updates = std::cell::RefCell::new(Vec::new());
            let cancel = AtomicBool::new(false);
            let mut ctx = Context::new(
                &cancel,
                |progress| updates.borrow_mut().push(progress),
                "size-independent-progress".into(),
                fs::metadata(&fixture.0).unwrap().dev(),
            );
            ctx.unthrottled_progress = true;
            let measured = measure(&fixture.0, 0, true, &mut ctx);
            assert_eq!(measured.totals.files, 2);
            measured_bytes.push(measured.totals.bytes);
            first_entry_estimates.push(
                updates
                    .borrow()
                    .iter()
                    .find(|progress| progress.scanned_files == 1)
                    .unwrap()
                    .estimated_percent
                    .unwrap(),
            );
        }
        assert!(measured_bytes[1] > measured_bytes[0]);
        assert_eq!(first_entry_estimates, [45.0, 45.0]);
    }

    #[test]
    fn an_oversized_deep_stream_is_indeterminate_and_keeps_every_entry() {
        let fixture = Fixture::new();
        for file in 0..DEEP_PROGRESS_ENTRIES + 17 {
            fixture.file(&format!("a/b/c/d/e/file-{file}.bin"), 32);
        }
        let updates = std::cell::RefCell::new(Vec::new());
        let cancel = AtomicBool::new(false);
        let mut ctx = Context::new(
            &cancel,
            |progress| updates.borrow_mut().push(progress),
            "deep-stream-progress".into(),
            fs::metadata(&fixture.0).unwrap().dev(),
        );
        ctx.unthrottled_progress = true;
        let measured = measure(&fixture.0, 0, true, &mut ctx);
        assert_eq!(measured.totals.files, (DEEP_PROGRESS_ENTRIES + 17) as u64);
        assert!(!measured.totals.partial);
        assert!(updates
            .borrow()
            .iter()
            .filter(|progress| progress.scanned_files > 0)
            .all(|progress| progress.estimated_percent.is_none()));
        assert_eq!(ctx.unknown_work_streams, 0);
        assert_eq!(ctx.estimated_percent(), Some(99.0));
    }

    #[test]
    fn cancelling_inside_a_deep_unknown_stream_does_not_settle_unfinished_work() {
        let fixture = Fixture::new();
        for file in 0..DEEP_PROGRESS_ENTRIES + 17 {
            fixture.file(&format!("a/b/c/d/e/file-{file}.bin"), 32);
        }
        let updates = std::cell::RefCell::new(Vec::new());
        let cancel = AtomicBool::new(false);
        let mut ctx = Context::new(
            &cancel,
            |progress| {
                if progress.scanned_files >= 100 {
                    cancel.store(true, Ordering::Relaxed);
                }
                updates.borrow_mut().push(progress);
            },
            "cancelled-deep-stream".into(),
            fs::metadata(&fixture.0).unwrap().dev(),
        );
        ctx.unthrottled_progress = true;
        let measured = measure(&fixture.0, 0, true, &mut ctx);
        assert_eq!(measured.totals.files, 100);
        assert!(measured.totals.partial);
        assert!(ctx.stopped);
        assert_eq!(ctx.unknown_work_streams, 0);
        assert!(ctx.estimated_percent().unwrap() < 99.0);
        assert!(updates
            .borrow()
            .iter()
            .filter_map(|progress| progress.estimated_percent)
            .all(|percent| percent < 99.0));
    }

    #[test]
    fn estimates_are_work_based_monotonic_and_never_claim_full_completion() {
        let fixture = Fixture::new();
        fixture.file("a/first.bin", 4_096);
        fixture.file("b/second.bin", 8_192);
        let updates = std::cell::RefCell::new(Vec::new());
        let report = analyze(&fixture.0, &AtomicBool::new(false), |progress| {
            updates.borrow_mut().push(progress.estimated_percent);
        })
        .unwrap();
        assert!(report.scan_complete);
        let updates = updates.into_inner();
        assert_eq!(updates[0], None);
        let estimated: Vec<_> = updates.into_iter().flatten().collect();
        assert_eq!(estimated.last(), Some(&99.0));
        assert!(estimated.windows(2).all(|pair| pair[0] <= pair[1]));
        assert!(estimated
            .iter()
            .all(|percent| percent.is_finite() && *percent >= 0.0 && *percent < 100.0));

        let cancelled_updates = std::cell::RefCell::new(Vec::new());
        let cancelled = analyze(&fixture.0, &AtomicBool::new(true), |progress| {
            cancelled_updates
                .borrow_mut()
                .push(progress.estimated_percent);
        })
        .unwrap();
        assert!(cancelled.cancelled);
        assert!(cancelled_updates.into_inner().iter().all(Option::is_none));
    }

    #[test]
    fn changed_directory_reserves_uncertain_work_instead_of_reaching_ninety_nine() {
        let fixture = Fixture::new();
        fixture.file("first.bin", 4_096);
        let changed = AtomicBool::new(false);
        let updates = std::cell::RefCell::new(Vec::new());
        let report = analyze(&fixture.0, &AtomicBool::new(false), |progress| {
            if !changed.swap(true, Ordering::Relaxed) {
                fixture.file("arrived.bin", 4_096);
            }
            updates.borrow_mut().push(progress.estimated_percent);
        })
        .unwrap();
        assert!(!report.scan_complete);
        assert!(report.root.partial);
        assert!(updates
            .into_inner()
            .into_iter()
            .flatten()
            .all(|percent| percent < 99.0));
    }
}
