//! Conservative, read-only macOS inventory and snapshot-bound native Trash cleanup.
//! Sizes are allocated blocks (512-byte units), not logical file length. Moving a
//! file to Trash is recoverable but does not free its space until Trash is emptied.

use serde::{Deserialize, Serialize};
use std::collections::{hash_map::DefaultHasher, HashMap, HashSet};
use std::ffi::CString;
use std::fs::{self, Metadata};
use std::hash::{Hash, Hasher};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_VISITS: u64 = 1_000_000;
const MAX_ITEMS: usize = 10_000;
const MAX_SOURCE_ITEMS: usize = 500;
const MAX_MANIFEST_ENTRIES: u64 = 25_000;
const MAX_DEPTH: usize = 32;
const MAX_WARNINGS: usize = 60;
const MAX_SCAN_TIME: Duration = Duration::from_secs(180);
static NEXT_SCAN: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ScanOptions {
    pub include_caches: bool,
    pub include_logs: bool,
    pub include_installers: bool,
    pub include_orphans: bool,
    pub min_age_days: u64,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            include_caches: true,
            include_logs: true,
            include_installers: true,
            include_orphans: true,
            min_age_days: 14,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskInfo {
    pub total_bytes: u64,
    pub available_bytes: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupItem {
    pub id: String,
    pub path: String,
    pub name: String,
    pub category: String,
    pub risk: String,
    pub bytes: u64,
    pub files: u64,
    pub is_directory: bool,
    pub modified_at: u64,
    pub reason: String,
    pub selected_by_default: bool,
    pub app_name: Option<String>,
    pub bundle_id: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanReport {
    pub scan_id: String,
    pub started_at: u64,
    pub duration_ms: u64,
    pub disk: DiskInfo,
    pub items: Vec<CleanupItem>,
    pub warnings: Vec<String>,
    pub installed_app_count: u64,
    pub scanned_files: u64,
    pub cancelled: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanProgress {
    pub phase: String,
    pub current_path: String,
    pub scanned_files: u64,
    pub found_items: u64,
    pub bytes_found: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupProgress {
    pub completed: u64,
    pub total: u64,
    pub current_path: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupResult {
    pub id: String,
    pub path: String,
    pub bytes: u64,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupReport {
    pub moved: Vec<CleanupResult>,
    pub failed: Vec<CleanupResult>,
    pub bytes_moved: u64,
}

pub struct ScanSnapshot {
    pub report: ScanReport,
    home: PathBuf,
    options: ScanOptions,
    validated: HashMap<String, ValidatedItem>,
    source_counts: HashMap<(String, String), usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Identity {
    device: u64,
    inode: u64,
}

impl Identity {
    fn of(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Fingerprint {
    identity: Identity,
    size: u64,
    blocks: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    links: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl Fingerprint {
    fn of(metadata: &Metadata) -> Self {
        Self {
            identity: Identity::of(metadata),
            size: metadata.len(),
            blocks: metadata.blocks(),
            mode: metadata.mode(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            links: metadata.nlink(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Manifest {
    digest: u64,
    bytes: u64,
    files: u64,
    entries: u64,
    newest: u64,
}

#[derive(Clone, Copy, Debug)]
enum Rule {
    Cache,
    RebuildableCache,
    Log,
    Installer,
    OrphanPreference,
    OrphanMetadata,
    OrphanState,
}

#[derive(Clone)]
struct ValidatedItem {
    path: PathBuf,
    root: PathBuf,
    ancestors: Vec<(PathBuf, Identity)>,
    fingerprint: Fingerprint,
    manifest: Manifest,
    rule: Rule,
    orphan_id: Option<String>,
    owner_name: String,
}

#[derive(Clone, Debug)]
struct InstalledApp {
    id: String,
    name: String,
    path: PathBuf,
}

struct AppInventory {
    apps: Vec<InstalledApp>,
    complete: bool,
    active_paths: HashSet<PathBuf>,
    active_complete: bool,
}

impl AppInventory {
    fn related(&self, id: &str) -> Option<&InstalledApp> {
        self.apps.iter().find(|app| bundle_related(&app.id, id))
    }

    fn active_related(&self, label: &str) -> bool {
        let normalized = normalized_name(label);
        self.active_paths.iter().any(|path| {
            let active_name = normalized_name(&file_name(path));
            active_name == normalized
                || active_name == normalized_name(label.rsplit('.').next().unwrap_or(label))
        }) || self.apps.iter().any(|app| {
            self.active_paths.contains(&app.path)
                && (bundle_related(&app.id, label)
                    || normalized == normalized_name(&app.name)
                    || normalized == normalized_name(&file_name(&app.path)))
        })
    }
}

#[derive(Debug)]
enum WalkError {
    Fresh,
    Protected,
    Symlink,
    Cancelled,
    Limit,
    Io(PathBuf, io::Error),
}

struct Context<'a, F: Fn(ScanProgress)> {
    cancel: &'a AtomicBool,
    progress: F,
    started: Instant,
    last_progress: Option<Instant>,
    phase: &'static str,
    visits: u64,
    files: u64,
    found: u64,
    bytes: u64,
    cutoff: u64,
    warnings: Vec<String>,
    stopped: bool,
}

impl<F: Fn(ScanProgress)> Context<'_, F> {
    fn warn(&mut self, message: String) {
        if self.warnings.len() < MAX_WARNINGS && !self.warnings.contains(&message) {
            self.warnings.push(message);
        }
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
        (self.progress)(ScanProgress {
            phase: self.phase.to_string(),
            current_path: path.to_string_lossy().into_owned(),
            scanned_files: self.files,
            found_items: self.found,
            bytes_found: self.bytes,
        });
    }

    fn visit(&mut self, path: &Path, file: bool) -> Result<(), WalkError> {
        if self.cancel.load(Ordering::Relaxed) {
            self.stopped = true;
            return Err(WalkError::Cancelled);
        }
        if self.stopped || self.visits >= MAX_VISITS || self.started.elapsed() > MAX_SCAN_TIME {
            self.stopped = true;
            self.warn(
                "扫描已达到时间或文件数量上限，当前结果为部分结果。可按类别分别扫描。".into(),
            );
            return Err(WalkError::Limit);
        }
        self.visits += 1;
        self.files += u64::from(file);
        self.emit(path);
        Ok(())
    }

    fn handle_walk_error(&mut self, error: WalkError) {
        match error {
            WalkError::Io(path, error) => self.warn(format!(
                "无法完整读取 {}：{}。此处占用未知，未列为可清理项目。",
                path.display(),
                error
            )),
            WalkError::Limit if !self.stopped => {
                self.warn("某个目录超过安全遍历上限，已跳过；显示的空间不包含该目录。".into())
            }
            _ => {}
        }
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

fn normalized_name(name: &str) -> String {
    name.trim_end_matches(".app")
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn reverse_dns(id: &str) -> bool {
    let labels: Vec<_> = id.split('.').collect();
    labels.len() >= 3
        && labels.iter().all(|label| {
            !label.is_empty()
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        })
        && labels[0].bytes().all(|byte| byte.is_ascii_alphabetic())
}

// Shared vendor identifiers are ambiguous: a sibling app can still own the data.
fn bundle_related(left: &str, right: &str) -> bool {
    let left = left.to_ascii_lowercase();
    let right = right.to_ascii_lowercase();
    left == right
        || left.starts_with(&format!("{right}."))
        || right.starts_with(&format!("{left}."))
        || (reverse_dns(&left)
            && reverse_dns(&right)
            && left.split('.').take(2).eq(right.split('.').take(2)))
}

fn protected_label(label: &str) -> bool {
    let label = label.to_ascii_lowercase();
    [
        "com.apple",
        "org.mozilla",
        "com.google",
        "com.brave",
        "com.operasoftware",
        "com.vivaldi",
        "com.microsoft.edgemac",
        "safari",
        "firefox",
        "chrome",
        "chromium",
        "minecraft",
        "mojang",
        "multimc",
        "prismlauncher",
        "curseforge",
        "steam",
        "battle.net",
        "huggingface",
        "hugging-face",
        "torch",
        "mlx",
        "model",
        "weights",
        "ollama",
        "lmstudio",
    ]
    .iter()
    .any(|protected| label.contains(protected))
        || matches!(
            label.as_str(),
            ".git" | ".trash" | "worlds" | "saves" | "playerdata" | "region"
        )
}

fn protected_file(path: &Path) -> bool {
    let name = file_name(path).to_ascii_lowercase();
    // Browser/account state can have no extension. Only regenerable caches are
    // eligible, including when a profile puts these files inside a cache root.
    protected_label(&name)
        || matches!(
            name.as_str(),
            "cookies"
                | "history"
                | "login data"
                | "web data"
                | "bookmarks"
                | "preferences"
                | "secure preferences"
                | "sessions"
                | "session storage"
                | "local storage"
                | "indexeddb"
                | "service worker"
                | "cache storage"
                | "cachestorage"
                | "sessionstore.jsonlz4"
                | "logins.json"
        )
        || ["cookies-", "history-", "login data-", "web data-"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
        || [
            ".db-wal",
            ".db-shm",
            ".db-journal",
            ".sqlite-wal",
            ".sqlite-shm",
            ".sqlite-journal",
        ]
        .iter()
        .any(|suffix| name.ends_with(suffix))
        || matches!(
            path.extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase()
                .as_str(),
            "safetensors"
                | "gguf"
                | "ggml"
                | "pt"
                | "pth"
                | "ckpt"
                | "onnx"
                | "sqlite"
                | "sqlite3"
                | "db"
                | "mca"
        )
}

fn runtime_cache_label(label: &str) -> bool {
    let label = label.to_ascii_lowercase();
    [
        "playwright",
        "node-gyp",
        "typescript",
        "extension.js",
        "electron",
        "swiftpm",
        "cargo",
        "npm",
        "yarn",
        "pnpm",
        "homebrew",
        "gradle",
        "cocoapods",
        "composer",
        "copilot",
        "wasilibs",
        "claude",
        "codex",
        "cursor",
        "modrinth",
        "mcaselector",
    ]
    .iter()
    .any(|runtime| label.contains(runtime))
        || matches!(label.as_str(), "pip" | "uv" | "node" | "deno" | "bun")
}

// Explicit regenerable locations, not entire tool homes, browser profiles,
// sandbox containers, or Application Support directories.
fn fixed_cache_sources(home: &Path) -> Vec<(PathBuf, &'static str)> {
    [
        (".npm/_cacache", "npm"),
        (".cache/pip", "pip"),
        (".cache/uv", "uv"),
        (".cache/node-gyp", "node-gyp"),
        (".cache/yarn", "Yarn"),
        (".cache/pnpm", "pnpm"),
        (".cargo/registry/cache", "Cargo"),
        ("go/pkg/mod/cache/download", "Go"),
        (".gradle/caches", "Gradle"),
        ("Library/pnpm/store", "pnpm"),
        ("Library/Caches/Homebrew", "Homebrew"),
        ("Library/Caches/CocoaPods", "CocoaPods"),
        ("Library/Caches/pip", "pip"),
        ("Library/Caches/uv", "uv"),
        ("Library/Caches/Yarn", "Yarn"),
        ("Library/Developer/Xcode/DerivedData", "Xcode"),
        ("Library/Caches/com.apple.dt.Xcode", "Xcode"),
        ("Library/Caches/Google/Chrome", "Google Chrome"),
        ("Library/Caches/com.google.Chrome", "Google Chrome"),
        ("Library/Caches/Firefox/Profiles", "Firefox"),
        ("Library/Caches/Microsoft Edge", "Microsoft Edge"),
        ("Library/Caches/com.microsoft.edgemac", "Microsoft Edge"),
        ("Library/Caches/com.apple.Safari/WebKitCache", "Safari"),
    ]
    .into_iter()
    .map(|(path, owner)| (home.join(path), owner))
    .collect()
}

const APP_CACHE_FOLDERS: [&str; 6] = [
    "Cache",
    "Code Cache",
    "GPUCache",
    "DawnCache",
    "DawnGraphiteCache",
    "DawnWebGPUCache",
];

fn extra_root_allowed(home: &Path, root: &Path, owner: &str) -> bool {
    if fixed_cache_sources(home)
        .iter()
        .any(|(path, name)| path == root && *name == owner)
    {
        return true;
    }
    // Derive both the owner and the root from literal normal components. Never
    // accept paths outside the allowlist even if a registered item is malformed.
    if owner.is_empty()
        || protected_label(owner)
        || Path::new(owner).components().count() != 1
        || !matches!(
            Path::new(owner).components().next(),
            Some(Component::Normal(_))
        )
    {
        return false;
    }
    if reverse_dns(owner)
        && root
            == home
                .join("Library/Containers")
                .join(owner)
                .join("Data/Library/Caches")
    {
        return true;
    }
    APP_CACHE_FOLDERS.iter().any(|folder| {
        root == home
            .join("Library/Application Support")
            .join(owner)
            .join(folder)
    })
}

fn cache_display_name(owner: &str) -> String {
    match owner {
        "npm" | "Cargo" | "Go" | "Homebrew" | "CocoaPods" | "Yarn" | "pnpm" => {
            format!("{owner} 下载缓存")
        }
        "Xcode" => "Xcode 编译缓存".into(),
        _ => owner.into(),
    }
}

fn protected_content_file(path: &Path, allow_app_state: bool) -> bool {
    if allow_app_state
        && matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("db" | "sqlite" | "sqlite3")
        )
    {
        protected_label(&file_name(path))
    } else {
        protected_file(path)
    }
}

fn is_installer(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "dmg" | "pkg" | "mpkg" | "iso"
    )
}

fn is_log(path: &Path) -> bool {
    let name = file_name(path).to_ascii_lowercase();
    [".log", ".txt", ".crash", ".ips", ".out", ".err", ".trace"]
        .iter()
        .any(|extension| name.ends_with(extension))
        || name.contains(".log.")
        || name.ends_with(".log.gz")
}

fn safe_ancestors(path: &Path) -> Result<Vec<(PathBuf, Identity)>, String> {
    if !path.is_absolute() {
        return Err("只允许绝对路径。".into());
    }
    let mut current = PathBuf::new();
    let mut identities = Vec::new();
    for component in path.components() {
        if matches!(component, Component::ParentDir | Component::CurDir) {
            return Err("路径包含不安全的相对组件。".into());
        }
        current.push(component.as_os_str());
        let metadata = fs::symlink_metadata(&current)
            .map_err(|error| format!("无法验证 {}：{}", current.display(), error))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(format!("已跳过符号链接或非目录：{}", current.display()));
        }
        identities.push((current.clone(), Identity::of(&metadata)));
    }
    Ok(identities)
}

fn safe_root<F: Fn(ScanProgress)>(root: &Path, ctx: &mut Context<'_, F>) -> bool {
    if matches!(fs::symlink_metadata(root), Err(error) if error.kind() == io::ErrorKind::NotFound) {
        return false;
    }
    match safe_ancestors(root) {
        Ok(_) => true,
        Err(error) => {
            ctx.warn(format!("{error}。此目录未扫描，空间占用未知。"));
            false
        }
    }
}

fn read_children<F: Fn(ScanProgress)>(
    path: &Path,
    ctx: &mut Context<'_, F>,
) -> Option<Vec<PathBuf>> {
    if let Err(error) = safe_ancestors(path) {
        ctx.warn(format!("{error}。此处占用未知，未继续遍历。"));
        return None;
    }
    let read = match fs::read_dir(path) {
        Ok(read) => read,
        Err(error) => {
            ctx.handle_walk_error(WalkError::Io(path.to_path_buf(), error));
            return None;
        }
    };
    let mut children = Vec::new();
    for entry in read {
        if ctx.cancel.load(Ordering::Relaxed) {
            ctx.stopped = true;
            return None;
        }
        match entry {
            Ok(entry) => children.push(entry.path()),
            Err(error) => {
                ctx.handle_walk_error(WalkError::Io(path.to_path_buf(), error));
                return None;
            }
        }
        if children.len() as u64 >= MAX_MANIFEST_ENTRIES {
            ctx.warn(format!(
                "{} 项目过多，已跳过，空间占用未知。",
                path.display()
            ));
            return None;
        }
    }
    children.sort();
    Some(children)
}

fn inventory(
    path: &Path,
    cutoff: Option<u64>,
    allow_app_state: bool,
    visit: &mut impl FnMut(&Path, bool) -> Result<(), WalkError>,
) -> Result<(Fingerprint, Manifest), WalkError> {
    let mut hash = DefaultHasher::new();
    let mut manifest = Manifest {
        digest: 0,
        bytes: 0,
        files: 0,
        entries: 0,
        newest: 0,
    };
    let fingerprint = inventory_entry(
        path,
        path,
        0,
        cutoff,
        allow_app_state,
        visit,
        &mut hash,
        &mut manifest,
    )?;
    manifest.digest = hash.finish();
    Ok((fingerprint, manifest))
}

#[allow(clippy::too_many_arguments)]
fn inventory_entry(
    path: &Path,
    base: &Path,
    depth: usize,
    cutoff: Option<u64>,
    allow_app_state: bool,
    visit: &mut impl FnMut(&Path, bool) -> Result<(), WalkError>,
    hash: &mut DefaultHasher,
    manifest: &mut Manifest,
) -> Result<Fingerprint, WalkError> {
    if depth > MAX_DEPTH || manifest.entries >= MAX_MANIFEST_ENTRIES {
        return Err(WalkError::Limit);
    }
    let metadata =
        fs::symlink_metadata(path).map_err(|error| WalkError::Io(path.to_path_buf(), error))?;
    visit(path, metadata.is_file())?;
    if metadata.file_type().is_symlink() {
        return Err(WalkError::Symlink);
    }
    if (!metadata.is_file() && !metadata.is_dir())
        || (metadata.is_file() && metadata.nlink() > 1)
        || protected_content_file(path, allow_app_state)
    {
        return Err(WalkError::Protected);
    }
    let modified = u64::try_from(metadata.mtime()).unwrap_or_default();
    if cutoff.is_some_and(|cutoff| modified > cutoff) {
        return Err(WalkError::Fresh);
    }
    let fingerprint = Fingerprint::of(&metadata);
    path.strip_prefix(base)
        .unwrap_or(path)
        .as_os_str()
        .as_bytes()
        .hash(hash);
    fingerprint.hash(hash);
    manifest.entries += 1;
    manifest.bytes = manifest
        .bytes
        .saturating_add(metadata.blocks().saturating_mul(512));
    manifest.files += u64::from(metadata.is_file());
    manifest.newest = manifest.newest.max(modified);
    if metadata.is_dir() {
        let mut children = Vec::new();
        let read = fs::read_dir(path).map_err(|error| WalkError::Io(path.to_path_buf(), error))?;
        for entry in read {
            // Check cancellation/time even when a single directory is very wide.
            visit(path, false)?;
            children.push(
                entry
                    .map_err(|error| WalkError::Io(path.to_path_buf(), error))?
                    .path(),
            );
            if children.len() as u64 >= MAX_MANIFEST_ENTRIES {
                return Err(WalkError::Limit);
            }
        }
        children.sort();
        for child in children {
            inventory_entry(
                &child,
                base,
                depth + 1,
                cutoff,
                allow_app_state,
                visit,
                hash,
                manifest,
            )?;
        }
        // Reject an entry inserted, removed, or renamed while this manifest was built.
        let after =
            fs::symlink_metadata(path).map_err(|error| WalkError::Io(path.to_path_buf(), error))?;
        if Fingerprint::of(&after) != fingerprint {
            return Err(WalkError::Fresh);
        }
    }
    Ok(fingerprint)
}

fn disk_info(path: &Path) -> Result<DiskInfo, String> {
    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| "硬盘路径无效。")?;
    let mut status = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: CString is NUL terminated; statvfs writes the supplied valid struct.
    if unsafe { libc::statvfs(path.as_ptr(), status.as_mut_ptr()) } != 0 {
        return Err(format!("无法读取硬盘容量：{}", io::Error::last_os_error()));
    }
    // SAFETY: statvfs succeeded and initialized status.
    let status = unsafe { status.assume_init() };
    let block_size = if status.f_frsize == 0 {
        status.f_bsize
    } else {
        status.f_frsize
    };
    Ok(DiskInfo {
        total_bytes: (status.f_blocks as u64).saturating_mul(block_size),
        available_bytes: (status.f_bavail as u64).saturating_mul(block_size),
    })
}

fn running_app_paths() -> Result<HashSet<PathBuf>, String> {
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("/bin/ps")
            .args(["-axo", "comm="])
            .output()
            .map_err(|error| format!("无法读取正在运行的应用：{error}"))?;
        if !output.status.success() {
            return Err("无法读取正在运行的应用，暂不能安全确认清理。".into());
        }
        {
            Ok(String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| {
                    let command = line.trim();
                    command
                        .find(".app/")
                        .map(|end| PathBuf::from(&command[..end + 4]))
                })
                .collect())
        }
    }
    #[cfg(not(target_os = "macos"))]
    Ok(HashSet::new())
}

fn app_inventory<F: Fn(ScanProgress)>(home: &Path, ctx: &mut Context<'_, F>) -> AppInventory {
    let running = running_app_paths();
    if let Err(error) = &running {
        ctx.warn(format!("{error}。已取消所有候选的默认勾选。"));
    }
    let active_complete = running.is_ok();
    let mut result = AppInventory {
        apps: Vec::new(),
        complete: true,
        active_paths: running.unwrap_or_default(),
        active_complete,
    };
    for root in [
        PathBuf::from("/Applications"),
        home.join("Applications"),
        PathBuf::from("/System/Applications"),
        PathBuf::from("/System/Library/CoreServices"),
    ] {
        if matches!(fs::symlink_metadata(&root), Err(error) if error.kind() == io::ErrorKind::NotFound)
        {
            continue;
        }
        if !safe_root(&root, ctx) {
            result.complete = false;
            continue;
        }
        let mut stack = vec![(root, 0_usize)];
        while let Some((path, depth)) = stack.pop() {
            if ctx.stopped {
                result.complete = false;
                break;
            }
            let Some(children) = read_children(&path, ctx) else {
                result.complete = false;
                continue;
            };
            for child in children {
                if ctx.visit(&child, false).is_err() {
                    result.complete = false;
                    break;
                }
                let metadata = match fs::symlink_metadata(&child) {
                    Ok(metadata) => metadata,
                    Err(error) => {
                        ctx.handle_walk_error(WalkError::Io(child, error));
                        result.complete = false;
                        continue;
                    }
                };
                if metadata.file_type().is_symlink() {
                    // A symlinked application makes absence-based identification uncertain.
                    if child
                        .extension()
                        .is_some_and(|extension| extension == "app")
                        && !child.starts_with("/System")
                        && !protected_label(&file_name(&child))
                    {
                        result.complete = false;
                        ctx.warn(format!(
                            "发现链接形式的应用 {}，已停用卸载残留推断。",
                            child.display()
                        ));
                    }
                    continue;
                }
                if !metadata.is_dir() {
                    continue;
                }
                if child
                    .extension()
                    .is_some_and(|extension| extension == "app")
                {
                    let info = child.join("Contents/Info.plist");
                    let valid_info = fs::symlink_metadata(&info).is_ok_and(|metadata| {
                        metadata.is_file()
                            && !metadata.file_type().is_symlink()
                            && metadata.len() < 4 * 1024 * 1024
                    });
                    let plist = if valid_info {
                        plist::Value::from_file(&info).ok()
                    } else {
                        None
                    };
                    let dictionary = plist.as_ref().and_then(plist::Value::as_dictionary);
                    let id = dictionary
                        .and_then(|dictionary| dictionary.get("CFBundleIdentifier"))
                        .and_then(plist::Value::as_string);
                    if let Some(id) = id {
                        let name = dictionary
                            .and_then(|dictionary| {
                                dictionary
                                    .get("CFBundleDisplayName")
                                    .or_else(|| dictionary.get("CFBundleName"))
                            })
                            .and_then(plist::Value::as_string)
                            .map(str::to_owned)
                            .unwrap_or_else(|| {
                                file_name(&child).trim_end_matches(".app").to_string()
                            });
                        result.apps.push(InstalledApp {
                            id: id.to_string(),
                            name,
                            path: child,
                        });
                    } else if !child.starts_with("/System") && !protected_label(&file_name(&child))
                    {
                        result.complete = false;
                        ctx.warn(format!(
                            "无法识别应用 {} 的标识，已停用卸载残留推断。",
                            child.display()
                        ));
                    }
                } else if depth < 6 && !file_name(&child).starts_with('.') {
                    stack.push((child, depth + 1));
                }
            }
        }
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn add_candidate<F: Fn(ScanProgress)>(
    snapshot: &mut ScanSnapshot,
    ctx: &mut Context<'_, F>,
    apps: &AppInventory,
    path: &Path,
    root: &Path,
    rule: Rule,
    orphan_id: Option<String>,
    owner_name: &str,
    prepared: Option<(Fingerprint, Manifest)>,
) {
    let extra_cache = matches!(rule, Rule::RebuildableCache);
    if (extra_cache && !extra_root_allowed(&snapshot.home, root, owner_name))
        || (!extra_cache && protected_label(owner_name))
        || apps.active_related(owner_name)
        || orphan_id
            .as_ref()
            .is_some_and(|id| !apps.complete || apps.related(id).is_some())
        || source_full(snapshot, ctx, rule, owner_name, orphan_id.as_deref())
    {
        return;
    }
    let measured = if let Some(prepared) = prepared {
        Ok(prepared)
    } else {
        let cutoff = ctx.cutoff;
        inventory(
            path,
            Some(cutoff),
            matches!(rule, Rule::OrphanMetadata),
            &mut |path, file| ctx.visit(path, file),
        )
    };
    let (fingerprint, manifest) = match measured {
        Ok(result) => result,
        Err(error) => {
            ctx.handle_walk_error(error);
            return;
        }
    };
    let ancestors = match safe_ancestors(path.parent().unwrap_or(root)) {
        Ok(ancestors) => ancestors,
        Err(error) => {
            ctx.warn(error);
            return;
        }
    };
    // Native Trash accepts UTF-8 URLs. Do not round-trip a lossy path from UI.
    let Some(display_path) = path.to_str() else {
        ctx.warn("某个路径不是有效 UTF-8，已跳过以避免错误识别或移动文件。".into());
        return;
    };
    let runtime_cache = matches!(rule, Rule::Cache) && runtime_cache_label(owner_name);
    let review = !apps.active_complete
        || extra_cache
        || runtime_cache
        || orphan_id.is_some()
        || matches!(
            rule,
            Rule::Installer | Rule::OrphanMetadata | Rule::OrphanPreference | Rule::OrphanState
        );
    let category = candidate_category(rule, orphan_id.as_deref());
    let reason = if extra_cache {
        "已知可重新生成的下载、编译或应用缓存，候选及全部子项均超过保留天数。可能影响离线工作、首次打开或重新编译；先退出相关工具并手动确认。不会包含整个应用数据目录。"
    } else if matches!(rule, Rule::OrphanState) {
        "疑似已卸载应用留下的窗口恢复状态。未找到同标识或同开发者的应用并不证明已经卸载，可能影响恢复上次打开的窗口；需手动确认。"
    } else if matches!(rule, Rule::OrphanMetadata) {
        "旧应用数据文件夹：未在常见应用目录找到同标识或同开发者的应用，但这不证明已经卸载。可能包含设置、数据库、聊天记录和个人文件，必须打开查看并手动确认。"
    } else if orphan_id.is_some() {
        "未在常见应用目录找到同标识或同开发者的应用；这不证明已经卸载，可能属于便携应用或后台组件。仅列出旧缓存或偏好元数据，需手动核对。"
    } else if runtime_cache {
        "开发工具或浏览器自动化的运行时缓存，可能仍被项目引用、用于离线工作或运行程序。即使时间较旧也需要手动核对，清理后可能重新下载。"
    } else {
        match rule {
            Rule::Cache => "位于用户缓存目录，候选文件及所有子项均超过保留天数；已避开浏览器状态、数据库、模型与游戏数据。应用下次使用时可能重新生成。",
            Rule::Log => "位于用户日志目录且超过保留天数；仅移动日志文件，保留目录及最近日志。",
            Rule::Installer => "下载目录中的旧磁盘镜像或安装包，可能用于重装或离线使用；需手动确认。",
            _ => "疑似旧偏好元数据，需要手动核对。",
        }
    };
    let app = apps.related(owner_name);
    let id = format!(
        "{}-{}",
        snapshot.report.scan_id,
        snapshot.report.items.len() + 1
    );
    let item = CleanupItem {
        id: id.clone(),
        path: display_path.to_string(),
        name: file_name(path),
        category: category.into(),
        risk: if review { "review" } else { "low" }.into(),
        bytes: manifest.bytes,
        files: manifest.files,
        is_directory: fingerprint.mode & 0o170_000 == 0o040_000,
        modified_at: manifest.newest,
        reason: reason.into(),
        selected_by_default: !review,
        app_name: app
            .map(|app| app.name.clone())
            .or_else(|| extra_cache.then(|| cache_display_name(owner_name))),
        bundle_id: orphan_id.clone().or_else(|| app.map(|app| app.id.clone())),
    };
    ctx.found += 1;
    ctx.bytes = ctx.bytes.saturating_add(item.bytes);
    *snapshot
        .source_counts
        .entry((category.into(), owner_name.into()))
        .or_default() += 1;
    snapshot.validated.insert(
        id,
        ValidatedItem {
            path: path.to_path_buf(),
            root: root.to_path_buf(),
            ancestors,
            fingerprint,
            manifest,
            rule,
            orphan_id,
            owner_name: owner_name.into(),
        },
    );
    snapshot.report.items.push(item);
}

fn candidate_category(rule: Rule, orphan_id: Option<&str>) -> &'static str {
    if orphan_id.is_some() {
        return "orphan";
    }
    match rule {
        Rule::Cache | Rule::RebuildableCache => "cache",
        Rule::Log => "logs",
        Rule::Installer => "installer",
        _ => "orphan",
    }
}

fn source_full<F: Fn(ScanProgress)>(
    snapshot: &ScanSnapshot,
    ctx: &mut Context<'_, F>,
    rule: Rule,
    owner: &str,
    orphan: Option<&str>,
) -> bool {
    if snapshot.report.items.len() >= MAX_ITEMS {
        ctx.warn("候选项目已达到 10,000 项上限，当前结果为部分结果。可按类别分别检查。".into());
        return true;
    }
    let key = (candidate_category(rule, orphan).into(), owner.into());
    if snapshot.source_counts.get(&key).copied().unwrap_or(0) >= MAX_SOURCE_ITEMS {
        ctx.warn(format!(
            "{owner} 的本类候选已达到 500 项，已保留其余内容并继续检查其他应用。"
        ));
        return true;
    }
    false
}

#[allow(clippy::too_many_arguments)]
fn walk_cache_branches<F: Fn(ScanProgress)>(
    snapshot: &mut ScanSnapshot,
    ctx: &mut Context<'_, F>,
    apps: &AppInventory,
    root: &Path,
    start: &Path,
    rule: Rule,
    owner: &str,
    orphan: Option<&str>,
    depth: usize,
) {
    if ctx.stopped || apps.active_related(owner) || source_full(snapshot, ctx, rule, owner, orphan)
    {
        return;
    }
    if depth > MAX_DEPTH {
        ctx.handle_walk_error(WalkError::Limit);
        return;
    }
    if protected_file(start) {
        return;
    }
    let cutoff = ctx.cutoff;
    match inventory(start, Some(cutoff), false, &mut |path, file| {
        ctx.visit(path, file)
    }) {
        Ok(prepared) => {
            add_candidate(
                snapshot,
                ctx,
                apps,
                start,
                root,
                rule,
                orphan.map(str::to_owned),
                owner,
                Some(prepared),
            );
            // Never also register descendants of a complete directory candidate.
            return;
        }
        Err(WalkError::Cancelled) => return,
        Err(error) => ctx.handle_walk_error(error),
    }
    if ctx.stopped
        || !fs::symlink_metadata(start).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
    {
        return;
    }
    // Preserve a fresh/partly protected parent. Independently validate complete
    // old branches rather than flattening every old descendant into a candidate.
    if let Some(children) = read_children(start, ctx) {
        for child in children {
            if ctx.stopped || source_full(snapshot, ctx, rule, owner, orphan) {
                break;
            }
            walk_cache_branches(
                snapshot,
                ctx,
                apps,
                root,
                &child,
                rule,
                owner,
                orphan,
                depth + 1,
            );
        }
    }
}

fn scan_extra_caches<F: Fn(ScanProgress)>(
    snapshot: &mut ScanSnapshot,
    ctx: &mut Context<'_, F>,
    apps: &AppInventory,
) {
    let home = snapshot.home.clone();
    let mut sources: Vec<(PathBuf, String)> = fixed_cache_sources(&home)
        .into_iter()
        .map(|(path, owner)| (path, owner.into()))
        .collect();
    for (base, sandbox) in [
        (home.join("Library/Containers"), true),
        (home.join("Library/Application Support"), false),
    ] {
        if ctx.stopped || !safe_root(&base, ctx) {
            continue;
        }
        let Some(entries) = read_children(&base, ctx) else {
            continue;
        };
        for app in entries {
            let owner = file_name(&app);
            if ctx.stopped {
                break;
            }
            if owner.starts_with('.')
                || protected_label(&owner)
                || apps.active_related(&owner)
                || !fs::symlink_metadata(&app)
                    .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
            {
                continue;
            }
            if sandbox {
                if reverse_dns(&owner) {
                    sources.push((app.join("Data/Library/Caches"), owner));
                }
            } else {
                for name in APP_CACHE_FOLDERS {
                    sources.push((app.join(name), owner.clone()));
                }
            }
        }
    }
    let mut seen = HashSet::new();
    for (root, owner) in sources {
        if ctx.stopped || snapshot.report.items.len() >= MAX_ITEMS {
            break;
        }
        if !seen.insert(root.clone())
            || apps.active_related(&owner)
            || !extra_root_allowed(&home, &root, &owner)
            || !safe_root(&root, ctx)
        {
            continue;
        }
        // Keep the cache root itself so the application can reuse its location.
        if let Some(children) = read_children(&root, ctx) {
            for child in children {
                if ctx.stopped || source_full(snapshot, ctx, Rule::RebuildableCache, &owner, None) {
                    break;
                }
                walk_cache_branches(
                    snapshot,
                    ctx,
                    apps,
                    &root,
                    &child,
                    Rule::RebuildableCache,
                    &owner,
                    None,
                    0,
                );
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn walk_files<F: Fn(ScanProgress)>(
    snapshot: &mut ScanSnapshot,
    ctx: &mut Context<'_, F>,
    apps: &AppInventory,
    root: &Path,
    start: &Path,
    rule: Rule,
    owner_name: &str,
    orphan_id: Option<&str>,
) {
    let mut stack = vec![(start.to_path_buf(), 0_usize)];
    while let Some((directory, depth)) = stack.pop() {
        if ctx.stopped || apps.active_related(owner_name) {
            return;
        }
        let directory_owner = if owner_name.is_empty() {
            directory
                .strip_prefix(root)
                .ok()
                .and_then(|relative| relative.components().next())
                .map(|component| component.as_os_str().to_string_lossy().into_owned())
                .unwrap_or_default()
        } else {
            owner_name.into()
        };
        if !directory_owner.is_empty()
            && source_full(snapshot, ctx, rule, &directory_owner, orphan_id)
        {
            continue;
        }
        let Some(children) = read_children(&directory, ctx) else {
            continue;
        };
        for child in children {
            if ctx.stopped {
                return;
            }
            let relative_owner = if owner_name.is_empty() {
                child
                    .strip_prefix(root)
                    .ok()
                    .and_then(|relative| relative.components().next())
                    .map(|component| component.as_os_str().to_string_lossy().into_owned())
                    .unwrap_or_default()
            } else {
                owner_name.into()
            };
            if protected_label(&relative_owner)
                || apps.active_related(&relative_owner)
                || source_full(snapshot, ctx, rule, &relative_owner, orphan_id)
            {
                continue;
            }
            let metadata = match fs::symlink_metadata(&child) {
                Ok(metadata) => metadata,
                Err(error) => {
                    ctx.handle_walk_error(WalkError::Io(child, error));
                    continue;
                }
            };
            if ctx.visit(&child, metadata.is_file()).is_err() {
                return;
            }
            if metadata.file_type().is_symlink() || protected_file(&child) {
                continue;
            }
            if metadata.is_dir() {
                if depth < MAX_DEPTH {
                    stack.push((child, depth + 1));
                } else {
                    ctx.warn(format!("{} 超过遍历深度上限，已跳过。", child.display()));
                }
            } else if metadata.is_file()
                && u64::try_from(metadata.mtime()).unwrap_or_default() <= ctx.cutoff
                && (!matches!(rule, Rule::Log) || is_log(&child))
            {
                add_candidate(
                    snapshot,
                    ctx,
                    apps,
                    &child,
                    root,
                    rule,
                    orphan_id.map(str::to_owned),
                    &relative_owner,
                    None,
                );
            }
        }
    }
}

pub fn scan(
    home: &Path,
    mut options: ScanOptions,
    cancel: &AtomicBool,
    progress: impl Fn(ScanProgress),
) -> Result<ScanSnapshot, String> {
    let home = home
        .canonicalize()
        .map_err(|error| format!("无法读取用户主目录：{error}"))?;
    if home == Path::new("/") || !home.is_dir() {
        return Err("用户主目录无效。".into());
    }
    let started_at = unix_now();
    let minimum_adjusted = options.min_age_days == 0;
    options.min_age_days = options.min_age_days.max(1);
    let disk = disk_info(&home)?;
    let mut ctx = Context {
        cancel,
        progress,
        started: Instant::now(),
        last_progress: None,
        phase: "applications",
        visits: 0,
        files: 0,
        found: 0,
        bytes: 0,
        cutoff: started_at.saturating_sub(options.min_age_days.saturating_mul(86_400)),
        warnings: Vec::new(),
        stopped: false,
    };
    if minimum_adjusted {
        ctx.warn("为保留最近文件，保留时间最短为 1 天。".into());
    }
    ctx.emit(&home);
    let apps = app_inventory(&home, &mut ctx);
    let mut snapshot = ScanSnapshot {
        report: ScanReport {
            scan_id: format!(
                "{}-{}-{}",
                started_at,
                std::process::id(),
                NEXT_SCAN.fetch_add(1, Ordering::Relaxed)
            ),
            started_at,
            duration_ms: 0,
            disk,
            items: Vec::new(),
            warnings: Vec::new(),
            installed_app_count: apps.apps.len() as u64,
            scanned_files: 0,
            cancelled: false,
        },
        home: home.clone(),
        options: options.clone(),
        validated: HashMap::new(),
        source_counts: HashMap::new(),
    };
    let logs = home.join("Library/Logs");
    if options.include_logs && !ctx.stopped && safe_root(&logs, &mut ctx) {
        ctx.phase = "logs";
        walk_files(
            &mut snapshot,
            &mut ctx,
            &apps,
            &logs,
            &logs,
            Rule::Log,
            "",
            None,
        );
    }
    let downloads = home.join("Downloads");
    if options.include_installers && !ctx.stopped && safe_root(&downloads, &mut ctx) {
        ctx.phase = "installers";
        // Deliberately only immediate download entries: nested folders may be user projects.
        if let Some(children) = read_children(&downloads, &mut ctx) {
            for child in children {
                if ctx.stopped {
                    break;
                }
                if is_installer(&child) {
                    let owner = file_name(&child);
                    add_candidate(
                        &mut snapshot,
                        &mut ctx,
                        &apps,
                        &child,
                        &downloads,
                        Rule::Installer,
                        None,
                        &owner,
                        None,
                    );
                }
            }
        }
    }
    if options.include_orphans && !ctx.stopped {
        ctx.phase = "orphans";
        if !apps.complete {
            ctx.warn("应用清单不完整，已跳过基于应用缺失的残留判断，避免误判已安装应用。".into());
        } else {
            for (root, rule) in [
                (home.join("Library/Preferences"), Rule::OrphanPreference),
                (
                    home.join("Library/Saved Application State"),
                    Rule::OrphanState,
                ),
                (
                    home.join("Library/Application Support"),
                    Rule::OrphanMetadata,
                ),
            ] {
                if ctx.stopped || !safe_root(&root, &mut ctx) {
                    continue;
                }
                let Some(children) = read_children(&root, &mut ctx) else {
                    continue;
                };
                for child in children {
                    if ctx.stopped {
                        break;
                    }
                    let name = file_name(&child);
                    let id = match rule {
                        Rule::OrphanPreference => match name.strip_suffix(".plist") {
                            Some(id) => id.to_string(),
                            None => continue,
                        },
                        Rule::OrphanState => match name.strip_suffix(".savedState") {
                            Some(id) => id.to_string(),
                            None => continue,
                        },
                        _ => name.clone(),
                    };
                    if !reverse_dns(&id) || protected_label(&id) || apps.related(&id).is_some() {
                        continue;
                    }
                    if matches!(rule, Rule::OrphanPreference)
                        && !fs::symlink_metadata(&child).is_ok_and(|metadata| {
                            metadata.is_file() && !metadata.file_type().is_symlink()
                        })
                    {
                        continue;
                    }
                    add_candidate(
                        &mut snapshot,
                        &mut ctx,
                        &apps,
                        &child,
                        &root,
                        rule,
                        Some(id.clone()),
                        &id,
                        None,
                    );
                }
            }
        }
    }
    if options.include_caches && !ctx.stopped {
        ctx.phase = "caches";
        scan_extra_caches(&mut snapshot, &mut ctx, &apps);
    }
    let caches = home.join("Library/Caches");
    if options.include_caches && !ctx.stopped && safe_root(&caches, &mut ctx) {
        ctx.phase = "caches";
        if let Some(children) = read_children(&caches, &mut ctx) {
            for child in children {
                if ctx.stopped {
                    break;
                }
                let owner = file_name(&child);
                if owner.starts_with('.')
                    || protected_label(&owner)
                    || apps.active_related(&owner)
                    || fixed_cache_sources(&home)
                        .iter()
                        .any(|(root, _)| root.starts_with(&child))
                {
                    continue;
                }
                let orphan = options.include_orphans
                    && apps.complete
                    && reverse_dns(&owner)
                    && apps.related(&owner).is_none();
                walk_cache_branches(
                    &mut snapshot,
                    &mut ctx,
                    &apps,
                    &caches,
                    &child,
                    Rule::Cache,
                    &owner,
                    orphan.then_some(owner.as_str()),
                    0,
                );
            }
        }
    }
    snapshot.report.items.sort_by(|left, right| {
        right
            .bytes
            .cmp(&left.bytes)
            .then_with(|| left.path.cmp(&right.path))
    });
    snapshot.report.duration_ms = ctx.started.elapsed().as_millis() as u64;
    snapshot.report.scanned_files = ctx.files;
    snapshot.report.cancelled = cancel.load(Ordering::Relaxed);
    snapshot.report.warnings = ctx.warnings;
    Ok(snapshot)
}

fn validate_item(
    snapshot: &ScanSnapshot,
    item: &ValidatedItem,
    apps: &AppInventory,
) -> Result<(), String> {
    if !apps.active_complete {
        return Err("无法验证正在运行的应用，已保留项目；请稍后重新扫描。".into());
    }
    let allowed_root = match item.rule {
        Rule::Cache => snapshot.home.join("Library/Caches"),
        Rule::RebuildableCache
            if extra_root_allowed(&snapshot.home, &item.root, &item.owner_name) =>
        {
            item.root.clone()
        }
        Rule::RebuildableCache => return Err("缓存路径不在已知规则范围内。".into()),
        Rule::Log => snapshot.home.join("Library/Logs"),
        Rule::Installer => snapshot.home.join("Downloads"),
        Rule::OrphanPreference => snapshot.home.join("Library/Preferences"),
        Rule::OrphanMetadata => snapshot.home.join("Library/Application Support"),
        Rule::OrphanState => snapshot.home.join("Library/Saved Application State"),
    };
    if item.root != allowed_root || item.path == item.root || !item.path.starts_with(&item.root) {
        return Err("路径不在扫描允许的清理范围内。".into());
    }
    if apps.active_related(&item.owner_name) {
        return Err("关联应用正在运行，请退出应用后重新扫描。".into());
    }
    if let Some(id) = &item.orphan_id {
        if !apps.complete || apps.related(id).is_some() {
            return Err("应用清单已变化或无法完整确认，已保留疑似残留；请重新扫描。".into());
        }
    }
    let ancestors = safe_ancestors(item.path.parent().ok_or("路径没有父目录。")?)?;
    if ancestors != item.ancestors {
        return Err("父目录在扫描后被替换，已保留项目；请重新扫描。".into());
    }
    let started = Instant::now();
    let mut visits = 0_u64;
    let cutoff = unix_now().saturating_sub(snapshot.options.min_age_days.saturating_mul(86_400));
    let measured = inventory(
        &item.path,
        Some(cutoff),
        matches!(item.rule, Rule::OrphanMetadata),
        &mut |_, _| {
            visits += 1;
            if visits > MAX_MANIFEST_ENTRIES * 3 || started.elapsed() > Duration::from_secs(20) {
                Err(WalkError::Limit)
            } else {
                Ok(())
            }
        },
    )
    .map_err(|error| match error {
        WalkError::Io(_, error) => format!("文件无法重新验证：{error}"),
        _ => "文件内容、时间或目录结构已变化，或包含受保护项目；请重新扫描。".into(),
    })?;
    if measured.0 != item.fingerprint || measured.1 != item.manifest {
        return Err("文件或子目录在扫描后发生变化，已保留项目；请重新扫描。".into());
    }
    // Recheck parents after the manifest walk, immediately before handing off to native Trash.
    if safe_ancestors(item.path.parent().ok_or("路径没有父目录。")?)? != item.ancestors {
        return Err("验证期间父目录发生变化，已保留项目。".into());
    }
    Ok(())
}

pub(crate) fn native_trash(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        use trash::macos::{DeleteMethod, TrashContextExtMacos};
        let mut context = trash::TrashContext::default();
        context.set_delete_method(DeleteMethod::NsFileManager);
        context
            .delete(path)
            .map_err(|error| format!("无法移入废纸篓：{error}"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        Err("此清理功能仅支持 macOS 原生废纸篓。".into())
    }
}

pub fn cleanup(
    snapshot: &mut ScanSnapshot,
    ids: &[String],
    progress: impl Fn(CleanupProgress),
) -> Result<CleanupReport, String> {
    let apps = current_app_inventory(&snapshot.home);
    cleanup_with_inventory(
        snapshot,
        ids,
        progress,
        native_trash,
        apps,
        running_app_paths,
    )
}

fn current_app_inventory(home: &Path) -> AppInventory {
    let cancel = AtomicBool::new(false);
    let mut context = Context {
        cancel: &cancel,
        progress: |_| {},
        started: Instant::now(),
        last_progress: None,
        phase: "applications",
        visits: 0,
        files: 0,
        found: 0,
        bytes: 0,
        cutoff: 0,
        warnings: Vec::new(),
        stopped: false,
    };
    app_inventory(home, &mut context)
}

fn cleanup_with_inventory(
    snapshot: &mut ScanSnapshot,
    ids: &[String],
    progress: impl Fn(CleanupProgress),
    trash_item: impl Fn(&Path) -> Result<(), String>,
    mut apps: AppInventory,
    refresh_running: impl Fn() -> Result<HashSet<PathBuf>, String>,
) -> Result<CleanupReport, String> {
    if ids.len() > MAX_ITEMS {
        return Err("单次清理项目过多，请重新扫描。".into());
    }
    let mut unique = HashSet::new();
    let ids: Vec<_> = ids
        .iter()
        .filter(|id| unique.insert((*id).clone()))
        .collect();
    // Reject the entire request before any mutation if the UI sends a stale/foreign ID.
    if ids
        .iter()
        .any(|id| !snapshot.validated.contains_key(id.as_str()))
    {
        return Err("清理选择不属于当前扫描结果；请重新扫描。".into());
    }
    let mut report = CleanupReport {
        moved: Vec::new(),
        failed: Vec::new(),
        bytes_moved: 0,
    };
    for (index, id) in ids.iter().enumerate() {
        let item = snapshot
            .validated
            .get(id.as_str())
            .expect("IDs validated before cleanup")
            .clone();
        progress(CleanupProgress {
            completed: index as u64,
            total: ids.len() as u64,
            current_path: item.path.to_string_lossy().into_owned(),
        });
        // Applications may start during a batch. Refresh before each filesystem validation.
        let validation = refresh_running().and_then(|running| {
            apps.active_paths = running;
            apps.active_complete = true;
            validate_item(snapshot, &item, &apps)
        });
        let outcome = validation
            .and_then(|()| trash_item(&item.path))
            .and_then(|()| match fs::symlink_metadata(&item.path) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Ok(metadata) if Identity::of(&metadata) != item.fingerprint.identity => Ok(()),
                Ok(_) => Err("原文件仍在原路径，未能确认移入废纸篓；已保留结果。".into()),
                Err(error) => Err(format!("移动后无法验证原路径：{error}")),
            });
        let result = CleanupResult {
            id: (*id).clone(),
            path: item.path.to_string_lossy().into_owned(),
            bytes: item.manifest.bytes,
            error: outcome.as_ref().err().cloned(),
        };
        if outcome.is_ok() {
            snapshot.validated.remove(id.as_str());
            snapshot.report.items.retain(|entry| entry.id != **id);
            report.bytes_moved = report.bytes_moved.saturating_add(result.bytes);
            report.moved.push(result);
        } else {
            report.failed.push(result);
        }
        progress(CleanupProgress {
            completed: (index + 1) as u64,
            total: ids.len() as u64,
            current_path: item.path.to_string_lossy().into_owned(),
        });
    }
    if let Ok(disk) = disk_info(&snapshot.home) {
        snapshot.report.disk = disk;
    }
    Ok(report)
}

#[cfg(test)]
#[path = "scanner_coverage_tests.rs"]
mod coverage_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn cleanup_with(
        snapshot: &mut ScanSnapshot,
        ids: &[String],
        progress: impl Fn(CleanupProgress),
        trash_item: impl Fn(&Path) -> Result<(), String>,
    ) -> Result<CleanupReport, String> {
        cleanup_with_inventory(snapshot, ids, progress, trash_item, no_apps(), || {
            Ok(HashSet::new())
        })
    }

    struct Fixture {
        home: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "macos-cleaner-{}-{}",
                std::process::id(),
                NEXT_SCAN.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            let home = path.canonicalize().unwrap();
            for folder in [
                "Library/Caches",
                "Library/Logs",
                "Library/Preferences",
                "Library/Application Support",
                "Downloads",
                "Applications",
            ] {
                fs::create_dir_all(home.join(folder)).unwrap();
            }
            Self { home }
        }
        fn file(&self, relative: &str) -> PathBuf {
            let path = self.home.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, b"fixture contents").unwrap();
            path
        }
        fn old(&self, path: &Path) {
            let time = unix_now() as i64 - 40 * 86_400;
            let times = [libc::timespec {
                tv_sec: time,
                tv_nsec: 0,
            }; 2];
            let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
            assert_eq!(
                unsafe {
                    libc::utimensat(
                        libc::AT_FDCWD,
                        c_path.as_ptr(),
                        times.as_ptr(),
                        libc::AT_SYMLINK_NOFOLLOW,
                    )
                },
                0
            );
        }
        fn snapshot_for(&self, path: &Path, rule: Rule) -> ScanSnapshot {
            self.old(path);
            let (fingerprint, manifest) = inventory(path, None, false, &mut |_, _| Ok(())).unwrap();
            let id = "fixture-1".to_string();
            let root = match rule {
                Rule::Cache => self.home.join("Library/Caches"),
                Rule::RebuildableCache => self.home.join(".npm/_cacache"),
                Rule::Log => self.home.join("Library/Logs"),
                Rule::Installer => self.home.join("Downloads"),
                Rule::OrphanPreference => self.home.join("Library/Preferences"),
                Rule::OrphanMetadata => self.home.join("Library/Application Support"),
                Rule::OrphanState => self.home.join("Library/Saved Application State"),
            };
            let item = CleanupItem {
                id: id.clone(),
                path: path.to_str().unwrap().into(),
                name: file_name(path),
                category: "logs".into(),
                risk: "low".into(),
                bytes: manifest.bytes,
                files: manifest.files,
                is_directory: fingerprint.mode & 0o170_000 == 0o040_000,
                modified_at: manifest.newest,
                reason: "fixture".into(),
                selected_by_default: true,
                app_name: None,
                bundle_id: None,
            };
            let validated = ValidatedItem {
                path: path.into(),
                root,
                ancestors: safe_ancestors(path.parent().unwrap()).unwrap(),
                fingerprint,
                manifest,
                rule,
                orphan_id: None,
                owner_name: "fixture".into(),
            };
            ScanSnapshot {
                report: ScanReport {
                    scan_id: "fixture".into(),
                    started_at: unix_now(),
                    duration_ms: 0,
                    disk: disk_info(&self.home).unwrap(),
                    items: vec![item],
                    warnings: vec![],
                    installed_app_count: 0,
                    scanned_files: 1,
                    cancelled: false,
                },
                home: self.home.clone(),
                options: ScanOptions::default(),
                validated: HashMap::from([(id, validated)]),
                source_counts: HashMap::new(),
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.home);
        }
    }

    fn no_apps() -> AppInventory {
        AppInventory {
            apps: vec![],
            complete: true,
            active_paths: HashSet::new(),
            active_complete: true,
        }
    }

    #[test]
    fn nested_change_rejects_whole_cache_directory() {
        let fixture = Fixture::new();
        let file = fixture.file("Library/Caches/test-cache/nested/old.cache");
        fixture.old(&file);
        fixture.old(file.parent().unwrap());
        let directory = fixture.home.join("Library/Caches/test-cache");
        let snapshot = fixture.snapshot_for(&directory, Rule::Cache);
        let validated = snapshot.validated.get("fixture-1").unwrap();
        assert!(validate_item(&snapshot, validated, &no_apps()).is_ok());
        fs::write(&file, b"changed contents").unwrap();
        fixture.old(&file); // Same old mtime does not hide changed length/ctime.
        assert!(validate_item(&snapshot, validated, &no_apps()).is_err());
    }

    #[test]
    fn replaced_file_and_symlink_parent_are_rejected() {
        let fixture = Fixture::new();
        let file = fixture.file("Library/Logs/demo/old.log");
        let snapshot = fixture.snapshot_for(&file, Rule::Log);
        let validated = snapshot.validated.get("fixture-1").unwrap();
        let saved = fixture.home.join("saved.log");
        fs::rename(&file, &saved).unwrap();
        fs::write(&file, b"fixture contents").unwrap();
        fixture.old(&file);
        assert!(validate_item(&snapshot, validated, &no_apps()).is_err());
        let directory = file.parent().unwrap();
        let moved = fixture.home.join("moved-logs");
        fs::rename(directory, &moved).unwrap();
        symlink(&moved, directory).unwrap();
        assert!(validate_item(&snapshot, validated, &no_apps()).is_err());
    }

    #[test]
    fn recent_child_symlink_and_user_state_prevent_directory_candidate() {
        let fixture = Fixture::new();
        let old = fixture.file("Library/Caches/demo/old.cache");
        fixture.old(&old);
        let recent = fixture.file("Library/Caches/demo/recent.cache");
        let directory = old.parent().unwrap();
        fixture.old(directory);
        assert!(matches!(
            inventory(
                directory,
                Some(unix_now() - 14 * 86_400),
                false,
                &mut |_, _| Ok(())
            ),
            Err(WalkError::Fresh)
        ));
        fixture.old(&recent);
        let link = directory.join("link");
        symlink(&old, &link).unwrap();
        assert!(matches!(
            inventory(directory, None, false, &mut |_, _| Ok(())),
            Err(WalkError::Symlink)
        ));
        fs::remove_file(link).unwrap();
        fixture.file("Library/Caches/demo/state.sqlite");
        assert!(matches!(
            inventory(directory, None, false, &mut |_, _| Ok(())),
            Err(WalkError::Protected)
        ));
    }

    #[test]
    fn orphan_app_state_is_reviewed_and_models_and_vendor_siblings_are_protected() {
        assert!(reverse_dns("com.vendor.deleted"));
        assert!(!reverse_dns("My Personal Folder"));
        assert!(bundle_related("com.vendor.installed", "com.vendor.deleted"));
        assert!(bundle_related("org.example.app", "org.example.app.helper"));
        assert!(!bundle_related("com.other.app", "com.vendor.deleted"));
        assert!(protected_label("com.apple.oldtool"));
        assert!(protected_label("org.prismlauncher.PrismLauncher"));
        assert!(!is_installer(Path::new("backup.zip")));
        let fixture = Fixture::new();
        fixture.file("Library/Application Support/com.vendor.deleted/prefs.plist");
        let path = fixture
            .home
            .join("Library/Application Support/com.vendor.deleted");
        assert!(inventory(&path, None, true, &mut |_, _| Ok(())).is_ok());
        fixture.file("Library/Application Support/com.vendor.deleted/documents.json");
        fixture.file("Library/Application Support/com.vendor.deleted/state.sqlite");
        assert!(inventory(&path, None, true, &mut |_, _| Ok(())).is_ok());
        fixture.file("Library/Application Support/com.vendor.deleted/weights.safetensors");
        assert!(matches!(
            inventory(&path, None, true, &mut |_, _| Ok(())),
            Err(WalkError::Protected)
        ));
    }

    #[test]
    fn cleanup_is_snapshot_bound_and_keeps_failed_items_and_scan_id() {
        let fixture = Fixture::new();
        let file = fixture.file("Library/Logs/old.log");
        let mut snapshot = fixture.snapshot_for(&file, Rule::Log);
        let called = std::cell::Cell::new(false);
        let unknown = cleanup_with(
            &mut snapshot,
            &["foreign-id".into()],
            |_| {},
            |_| {
                called.set(true);
                Ok(())
            },
        );
        assert!(unknown.is_err());
        assert!(!called.get());
        let failed = cleanup_with(
            &mut snapshot,
            &["fixture-1".into()],
            |_| {},
            |_| Err("fixture refusal".into()),
        )
        .unwrap();
        assert_eq!(failed.failed.len(), 1);
        assert_eq!(snapshot.report.items.len(), 1);
        assert!(file.exists());
        // Mock native Trash with an in-fixture move; never invoke actual user Trash in tests.
        let destination = fixture.home.join("mock-trash.log");
        let moved = cleanup_with(
            &mut snapshot,
            &["fixture-1".into(), "fixture-1".into()],
            |_| {},
            |path| fs::rename(path, &destination).map_err(|error| error.to_string()),
        )
        .unwrap();
        assert_eq!(moved.moved.len(), 1, "{:?}", moved.failed);
        assert!(snapshot.report.items.is_empty());
        assert_eq!(snapshot.report.scan_id, "fixture");
        assert!(destination.exists());
    }

    #[test]
    fn pre_cancelled_scan_returns_partial_cancelled_report() {
        let fixture = Fixture::new();
        let cancel = AtomicBool::new(true);
        let report = scan(&fixture.home, ScanOptions::default(), &cancel, |_| {})
            .unwrap()
            .report;
        assert!(report.cancelled);
        assert!(report.items.is_empty());
    }

    #[test]
    fn mixed_orphan_cache_and_developer_runtime_never_become_default_selection() {
        let fixture = Fixture::new();
        let old = fixture.file("Library/Caches/com.fixturevendor.removed/old.cache");
        fixture.old(&old);
        fixture.file("Library/Caches/com.fixturevendor.removed/recent.cache");
        let runtime = fixture.file("Library/Caches/ms-playwright/runtime.cache");
        fixture.old(&runtime);
        let cancel = AtomicBool::new(false);
        let mut ctx = Context {
            cancel: &cancel,
            progress: |_| {},
            started: Instant::now(),
            last_progress: None,
            phase: "caches",
            visits: 0,
            files: 0,
            found: 0,
            bytes: 0,
            cutoff: unix_now() - 14 * 86_400,
            warnings: vec![],
            stopped: false,
        };
        let mut snapshot = fixture.snapshot_for(&old, Rule::Cache);
        snapshot.validated.clear();
        snapshot.report.items.clear();
        let root = fixture.home.join("Library/Caches");
        let orphan_directory = root.join("com.fixturevendor.removed");
        walk_files(
            &mut snapshot,
            &mut ctx,
            &no_apps(),
            &root,
            &orphan_directory,
            Rule::Cache,
            "com.fixturevendor.removed",
            Some("com.fixturevendor.removed"),
        );
        add_candidate(
            &mut snapshot,
            &mut ctx,
            &no_apps(),
            &runtime,
            &root,
            Rule::Cache,
            None,
            "ms-playwright",
            None,
        );
        assert_eq!(snapshot.report.items.len(), 2);
        assert!(snapshot
            .report
            .items
            .iter()
            .all(|item| item.risk == "review" && !item.selected_by_default));
        assert!(snapshot
            .report
            .items
            .iter()
            .any(|item| item.category == "orphan"));
    }

    #[test]
    fn newly_started_app_and_failed_process_inventory_preserve_files() {
        let fixture = Fixture::new();
        let first = fixture.file("Library/Logs/first.log");
        let second = fixture.file("Library/Logs/second.log");
        let mut snapshot = fixture.snapshot_for(&first, Rule::Log);
        let mut other = fixture.snapshot_for(&second, Rule::Log);
        let mut other_item = other.report.items.remove(0);
        other_item.id = "fixture-2".into();
        snapshot.report.items.push(other_item);
        snapshot.validated.insert(
            "fixture-2".into(),
            other.validated.remove("fixture-1").unwrap(),
        );
        let refreshes = std::cell::Cell::new(0);
        let destination = fixture.home.join("mock-first.log");
        let report = cleanup_with_inventory(
            &mut snapshot,
            &["fixture-1".into(), "fixture-2".into()],
            |_| {},
            |path| fs::rename(path, &destination).map_err(|error| error.to_string()),
            no_apps(),
            || {
                let refresh = refreshes.get();
                refreshes.set(refresh + 1);
                Ok(if refresh == 0 {
                    HashSet::new()
                } else {
                    HashSet::from([PathBuf::from("/Applications/fixture.app")])
                })
            },
        )
        .unwrap();
        assert_eq!(report.moved.len(), 1);
        assert_eq!(report.failed.len(), 1);
        assert!(second.exists());
        let called = std::cell::Cell::new(false);
        let failed = cleanup_with_inventory(
            &mut snapshot,
            &["fixture-2".into()],
            |_| {},
            |_| {
                called.set(true);
                Ok(())
            },
            no_apps(),
            || Err("process permission denied".into()),
        )
        .unwrap();
        assert_eq!(failed.failed.len(), 1);
        assert!(!called.get());
        assert!(second.exists());
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "Moves only a self-created temporary fixture to native macOS Trash; run explicitly"]
    fn native_trash_moves_only_temporary_fixture() {
        let fixture = Fixture::new();
        let file = fixture.file("Library/Logs/native-trash-fixture.log");
        let mut snapshot = fixture.snapshot_for(&file, Rule::Log);
        let expected_bytes = snapshot.report.items[0].bytes;
        let report = cleanup(&mut snapshot, &["fixture-1".into()], |_| {}).unwrap();
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert_eq!(report.moved.len(), 1);
        assert_eq!(report.bytes_moved, expected_bytes);
        assert!(!file.exists());
        assert_eq!(snapshot.report.scan_id, "fixture");
    }
}
