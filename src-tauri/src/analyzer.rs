//! Read-only directory sizes. This module never supplies cleanup candidates.
//! Allocated blocks include directory/link storage. Multiply-linked regular files
//! are counted once per inode; APFS shared extents cannot be deduplicated here.

use serde::Serialize;
use std::cmp::Ordering as CmpOrdering;
use std::collections::{BinaryHeap, HashSet};
use std::ffi::CString;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DISPLAY_DEPTH: usize = 3;
const DISPLAY_CHILDREN: usize = 300;
const DISPLAY_NODES: u64 = 15_000;
const MAX_DEPTH: usize = 256;
// macOS SDK sys/stat.h: SF_DATALESS marks a File Provider object whose content
// is online. Enumerating an online directory/package can trigger hydration.
const SF_DATALESS: u32 = 0x4000_0000;
const PROTECTED_DIRECTORY_TIMEOUT: Duration = Duration::from_secs(10);
const DIRECTORY_WAIT_POLL: Duration = Duration::from_millis(100);
const PROTECTED_DIRECTORY_ENTRY_LIMIT: usize = 100_000;
const MAX_DIRECTORY_WORKERS: usize = 4;
static ACTIVE_DIRECTORY_WORKERS: AtomicUsize = AtomicUsize::new(0);
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
    /// Only multiply-linked files need an inode set; ordinary files use no entry.
    hard_links: HashSet<(u64, u64)>,
    permission_denied_count: u64,
    other_error_count: u64,
    skipped_mount_count: u64,
    changed_directory_count: u64,
    depth_limited_count: u64,
    cloud_placeholder_count: u64,
    blocked_directory_count: u64,
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
        });
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
    thread::Builder::new()
        .name("mac-sweep-directory".into())
        .spawn(move || {
            let _slot = slot;
            let result = operation(worker_stop);
            // A dropped receiver frees buffered entries when a delayed OS call
            // finally returns; no scan context or report is held by this thread.
            let _ = sender.send(result);
        })
        .map_err(DirectoryReadFailure::Io)?;
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

struct BufferedDirectory {
    entries: Vec<io::Result<fs::DirEntry>>,
    truncated: bool,
}

enum DirectoryEntries {
    Streaming(fs::ReadDir),
    Buffered(std::vec::IntoIter<io::Result<fs::DirEntry>>),
}

impl Iterator for DirectoryEntries {
    type Item = io::Result<fs::DirEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Streaming(entries) => entries.next(),
            Self::Buffered(entries) => entries.next(),
        }
    }
}

fn directory_entries(
    path: &Path,
    cancel: &AtomicBool,
) -> Result<(DirectoryEntries, bool), DirectoryReadFailure> {
    if !requires_bounded_enumeration(path) {
        return fs::read_dir(path)
            .map(|entries| (DirectoryEntries::Streaming(entries), false))
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
        let mut reader = fs::read_dir(path)?;
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
    let metadata = match fs::symlink_metadata(path) {
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
    entry: fs::DirEntry,
    depth: usize,
    retain: bool,
    ctx: &mut Context<'_, F>,
) -> Measured {
    if !ctx.check() {
        return empty_partial();
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
            Ok((mut entries, truncated)) => {
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
            }
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
                has_children: directory
                    && !cloud_placeholder
                    && (totals.children > 0 || totals.partial),
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
        });
    let scan_complete = !root.partial && !cancelled;
    Ok(AnalysisReport {
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
        let first = measure_entry(entries.next().unwrap().unwrap(), 4, false, &mut ctx);
        assert_eq!(first.totals.files, 1);
        assert!(first.displayed.is_none());
        assert!(cancel.load(Ordering::Relaxed));
        let second = measure_entry(entries.next().unwrap().unwrap(), 4, false, &mut ctx);
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
}
