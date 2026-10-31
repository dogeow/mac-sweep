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
    /// Deletability verdict: `safe` (regenerable, no user data), `review`
    /// (probably unneeded but check first) or `caution` (may hold user data).
    pub safety: &'static str,
    /// What the user loses or must redo after moving this item to Trash.
    pub impact: String,
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
    pub outcome: &'static str,
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
    cleanup_uncertain: bool,
}

impl ScanSnapshot {
    pub(crate) fn invalidate_cleanup(&mut self) {
        self.cleanup_uncertain = true;
    }
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
    PartialDownload,
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
    active_apps: Vec<InstalledApp>,
    active_complete: bool,
}

impl AppInventory {
    fn related(&self, id: &str) -> Option<&InstalledApp> {
        if let Some(exact) = self.apps.iter().find(|app| app.id.eq_ignore_ascii_case(id)) {
            return Some(exact);
        }
        let mut matches = self
            .apps
            .iter()
            .filter(|app| bundle_hierarchically_related(&app.id, id));
        let first = matches.next()?;
        // Multiple copies of one bundle ID still identify the same owner. Two
        // different hierarchical IDs do not establish one application owner.
        if matches.any(|app| !app.id.eq_ignore_ascii_case(&first.id)) {
            None
        } else {
            Some(first)
        }
    }

    fn has_possible_owner(&self, id: &str) -> bool {
        self.apps
            .iter()
            .chain(&self.active_apps)
            .any(|app| bundle_related(&app.id, id))
    }

    fn owner_uncertain(&self, id: &str) -> bool {
        self.related(id).is_none() && self.has_possible_owner(id)
    }

    fn refresh_active_paths(&mut self, running: HashSet<PathBuf>) -> Result<(), String> {
        self.active_paths = running;
        self.active_apps.clear();
        self.active_complete = false;
        for path in &self.active_paths {
            let app = read_app_metadata(path).map_err(|error| {
                format!(
                    "无法识别正在运行的应用 {}：{error}。暂不能安全确认清理。",
                    path.display()
                )
            })?;
            self.active_apps.push(app);
        }
        self.active_complete = true;
        Ok(())
    }

    fn active_related(&self, label: &str) -> bool {
        let normalized = normalized_name(label);
        self.active_paths.iter().any(|path| {
            let active_name = normalized_name(&file_name(path));
            active_name == normalized
                || active_name == normalized_name(label.rsplit('.').next().unwrap_or(label))
        }) || self.active_apps.iter().any(|app| {
            bundle_hierarchically_related(&app.id, label)
                || normalized == normalized_name(&app.name)
                || normalized == normalized_name(&file_name(&app.path))
        }) || self.apps.iter().any(|app| {
            self.active_paths.contains(&app.path)
                && (bundle_hierarchically_related(&app.id, label)
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
    app_name_without_extension(name)
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn app_name_without_extension(name: &str) -> &str {
    if name
        .get(name.len().saturating_sub(4)..)
        .is_some_and(|extension| extension.eq_ignore_ascii_case(".app"))
    {
        &name[..name.len() - 4]
    } else {
        name
    }
}

fn is_app_bundle(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
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

fn bundle_hierarchically_related(left: &str, right: &str) -> bool {
    let left = left.to_ascii_lowercase();
    let right = right.to_ascii_lowercase();
    left == right
        || left.starts_with(&format!("{right}."))
        || right.starts_with(&format!("{left}."))
}

// Shared vendor identifiers are ambiguous. They veto absence-based orphan
// inference, but must never supply display ownership or merge application groups.
fn bundle_related(left: &str, right: &str) -> bool {
    bundle_hierarchically_related(left, right)
        || (reverse_dns(left)
            && reverse_dns(right)
            && left
                .split('.')
                .take(2)
                .map(str::to_ascii_lowercase)
                .eq(right.split('.').take(2).map(str::to_ascii_lowercase)))
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

fn protected_content_file(path: &Path, _allow_app_state: bool) -> bool {
    // Manual review does not relax browser/account/database protections for
    // application support directories whose installed owner cannot be found.
    protected_file(path)
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

// Abandoned browser downloads: Chrome, Firefox, Safari (bundle) and Opera.
fn is_partial_download(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "crdownload" | "part" | "download" | "opdownload"
    )
}

// Installer file names usually carry versions and platform tags, e.g.
// `Firefox 131.0.dmg` or `Docker-arm64.dmg`. Keep the leading name tokens.
fn installer_product_key(path: &Path) -> String {
    const NOISE: [&str; 16] = [
        "mac",
        "macos",
        "osx",
        "darwin",
        "universal",
        "arm64",
        "aarch64",
        "x64",
        "x86",
        "intel",
        "apple",
        "silicon",
        "installer",
        "setup",
        "latest",
        "stable",
    ];
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut key = String::new();
    for token in stem.split(|character: char| !character.is_alphanumeric()) {
        let token = token.to_lowercase();
        if token.is_empty() {
            continue;
        }
        if token.starts_with(|character: char| character.is_ascii_digit())
            || NOISE.contains(&token.as_str())
            || (token.starts_with('v') && token[1..].starts_with(|c: char| c.is_ascii_digit()))
        {
            break;
        }
        key.push_str(&token);
    }
    key.strip_prefix("install").unwrap_or(&key).to_string()
}

fn installed_app_for_installer<'a>(
    apps: &'a AppInventory,
    path: &Path,
) -> Option<&'a InstalledApp> {
    let key = installer_product_key(path);
    if key.chars().count() < 3 {
        return None;
    }
    apps.apps.iter().find(|app| {
        [
            normalized_name(&app.name),
            normalized_name(&file_name(&app.path)),
        ]
        .iter()
        .any(|name| {
            name.chars().count() >= 3
                && (key == *name
                    || key.strip_prefix(name.as_str()).is_some_and(|rest| {
                        matches!(rest, "mac" | "macos" | "osx" | "installer" | "setup")
                    }))
        })
    })
}

// Download and build caches that hold nothing but regenerable artifacts. The
// owning tool must not be running (already enforced before registration).
fn pure_regenerable_cache(home: &Path, root: &Path, owner: &str) -> bool {
    let fixed = fixed_cache_sources(home)
        .iter()
        .any(|(path, name)| path == root && *name == owner);
    if fixed {
        // pnpm's store is hard-linked into projects; prefer `pnpm store prune`.
        return owner != "pnpm";
    }
    // Chromium/Electron HTTP, shader and V8 code caches outside sandbox containers.
    APP_CACHE_FOLDERS.iter().any(|folder| {
        root == home
            .join("Library/Application Support")
            .join(owner)
            .join(folder)
    })
}

fn cache_impact(owner: &str) -> &'static str {
    match owner {
        "Xcode" => "下次编译会重新构建，首次编译变慢。",
        "Google Chrome" | "Firefox" | "Microsoft Edge" | "Safari" => {
            "网页首次打开会稍慢；不影响登录、历史记录、书签和密码。"
        }
        "npm" | "pip" | "uv" | "node-gyp" | "Yarn" | "pnpm" | "Cargo" | "Go" | "Gradle"
        | "Homebrew" | "CocoaPods" => "下次安装依赖时会重新下载，离线时可能无法安装。",
        _ => "应用下次启动时会重新生成，首次打开可能稍慢。",
    }
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
        let mut paths =
            bundle_paths_from_processes(current_user_process_ids()?, native_process_executable)?;
        // Public NSWorkspaceRunningApplications is explicitly thread-safe.
        // Its bundle URLs also cover GUI bundles backed by an external script
        // interpreter; the kernel inventory covers unregistered app executables.
        let applications = objc2_app_kit::NSWorkspace::sharedWorkspace().runningApplications();
        if applications.len() > 16_384 {
            return Err("系统应用清单超出安全上限，暂不能确认清理。".into());
        }
        for application in applications.iter() {
            if application.isTerminated() {
                continue;
            }
            let file_path = |url: &objc2_foundation::NSURL| {
                let path =
                    unsafe { std::ffi::CStr::from_ptr(url.fileSystemRepresentation().as_ptr()) };
                use std::os::unix::ffi::OsStrExt;
                PathBuf::from(std::ffi::OsStr::from_bytes(path.to_bytes()))
            };
            match application.bundleURL() {
                Some(url) if url.isFileURL() => {
                    let path = file_path(&url);
                    if !path.is_absolute() {
                        return Err("系统返回的应用位置无效，暂不能确认清理。".into());
                    }
                    // Unbundled GUI processes (for example a Java app) report
                    // their executable here. Track only an enclosing app bundle.
                    if is_app_bundle(&path) {
                        paths.insert(path);
                    } else if let Some(bundle) = app_bundle_from_executable(&path)? {
                        paths.insert(bundle);
                    }
                }
                None if application.bundleIdentifier().is_none() => {}
                // XPC helpers such as WebKit's WebContent carry an identifier but
                // no bundle URL. Locate the enclosing app from the executable.
                None => match application.executableURL() {
                    Some(url) if url.isFileURL() && file_path(&url).is_absolute() => {
                        if let Some(bundle) = app_bundle_from_executable(&file_path(&url))? {
                            paths.insert(bundle);
                        }
                    }
                    _ if application
                        .bundleIdentifier()
                        .is_some_and(|id| protected_label(&id.to_string())) => {}
                    _ => return Err("无法完整取得正在运行应用的位置，暂不能确认清理。".into()),
                },
                _ => return Err("无法完整取得正在运行应用的位置，暂不能确认清理。".into()),
            }
        }
        Ok(paths)
    }
    #[cfg(not(target_os = "macos"))]
    Ok(HashSet::new())
}

fn app_bundle_from_executable(path: &Path) -> Result<Option<PathBuf>, String> {
    if !path.is_absolute() {
        return Err("系统返回的进程路径不是绝对路径，暂不能安全确认清理。".into());
    }
    let mut bundle = PathBuf::new();
    let mut components = path.components().peekable();
    while let Some(component) = components.next() {
        if matches!(component, Component::CurDir | Component::ParentDir) {
            return Err("系统返回的进程路径包含不明确组件，暂不能安全确认清理。".into());
        }
        bundle.push(component.as_os_str());
        if is_app_bundle(&bundle) {
            if components.peek().is_none() {
                return Err("系统返回的应用进程路径不完整，暂不能安全确认清理。".into());
            }
            return Ok(Some(bundle));
        }
    }
    Ok(None)
}

fn bundle_paths_from_processes(
    pids: impl IntoIterator<Item = i32>,
    executable: impl Fn(i32) -> Result<Option<PathBuf>, String>,
) -> Result<HashSet<PathBuf>, String> {
    let mut paths = HashSet::new();
    for pid in pids {
        if let Some(path) = executable(pid)? {
            if let Some(bundle) = app_bundle_from_executable(&path)? {
                paths.insert(bundle);
            }
        }
    }
    Ok(paths)
}

#[cfg(target_os = "macos")]
fn current_user_process_ids() -> Result<HashSet<i32>, String> {
    // Public SDK sys/proc_info.h selectors; libc exports the functions but not
    // these selectors. Include real-UID processes so sudo-launched helpers also
    // remain relevant to this user's home-directory cleanup.
    const PROC_UID_ONLY: u32 = 4;
    const PROC_RUID_ONLY: u32 = 5;
    const MAX_ACTIVE_PIDS: usize = 16_384;
    let mut pids = HashSet::new();
    // SAFETY: getuid/geteuid take no arguments and have no memory preconditions.
    let (uid, ruid) = unsafe { (libc::geteuid(), libc::getuid()) };
    for (selector, owner) in [(PROC_UID_ONLY, uid), (PROC_RUID_ONLY, ruid)] {
        let mut buffer = vec![0_i32; MAX_ACTIVE_PIDS];
        let capacity = std::mem::size_of_val(buffer.as_slice());
        // SAFETY: buffer points to capacity bytes of writable i32 storage.
        let bytes = unsafe {
            libc::proc_listpids(selector, owner, buffer.as_mut_ptr().cast(), capacity as i32)
        };
        if bytes <= 0
            || bytes as usize >= capacity
            || !(bytes as usize).is_multiple_of(std::mem::size_of::<i32>())
        {
            return Err("无法完整读取当前用户的进程清单，暂不能安全确认清理。".into());
        }
        pids.extend(
            buffer[..bytes as usize / std::mem::size_of::<i32>()]
                .iter()
                .copied()
                .filter(|pid| *pid > 0),
        );
    }
    if !pids.contains(&(std::process::id() as i32)) {
        return Err("当前用户的进程清单不完整，暂不能安全确认清理。".into());
    }
    Ok(pids)
}

#[cfg(target_os = "macos")]
fn native_process_info(pid: i32) -> Result<Option<libc::proc_bsdinfo>, (String, Option<i32>)> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = std::mem::size_of::<libc::proc_bsdinfo>();
    // SAFETY: info is aligned, writable storage for exactly size bytes.
    let bytes = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size as i32,
        )
    };
    if bytes != size as i32 {
        let error = io::Error::last_os_error();
        if bytes == 0 && error.raw_os_error() == Some(libc::ESRCH) {
            return Ok(None); // The process exited between enumeration and lookup.
        }
        return Err((
            format!("无法验证用户进程 {pid}：{error}。暂不能安全确认清理。"),
            error.raw_os_error(),
        ));
    }
    // SAFETY: proc_pidinfo returned the full initialized structure.
    let info = unsafe { info.assume_init() };
    if info.pbi_pid != pid as u32 {
        return Err((
            "系统返回的进程标识不一致，暂不能安全确认清理。".into(),
            None,
        ));
    }
    Ok(Some(info))
}

#[cfg(target_os = "macos")]
fn native_process_path(pid: i32) -> Result<PathBuf, io::Error> {
    let mut buffer = vec![0_u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: buffer points to the API's maximum-size writable byte array.
    let bytes = unsafe { libc::proc_pidpath(pid, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
    if bytes <= 0 {
        return Err(io::Error::last_os_error());
    }
    let length = bytes as usize;
    if length >= buffer.len() || buffer[length] != 0 || buffer[..length].contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "系统进程路径不完整。",
        ));
    }
    // Kernel path bytes, rather than argv[0] or line-delimited ps output. Preserve
    // non-UTF-8 names and newlines so they cannot hide an active app bundle.
    Ok(PathBuf::from(std::ffi::OsStr::from_bytes(
        &buffer[..length],
    )))
}

#[cfg(target_os = "macos")]
fn native_process_executable(pid: i32) -> Result<Option<PathBuf>, String> {
    let before = match native_process_info(pid) {
        Ok(Some(info)) => info,
        Ok(None) => return Ok(None),
        // Setuid processes the user started (for example `/usr/bin/login` under
        // Terminal) hide their BSD info, but their executable path is public.
        // Return that path: one outside an app bundle is irrelevant, and one
        // inside a bundle conservatively counts as a running application.
        Err((error, Some(libc::EPERM))) => {
            return native_process_path(pid).map(Some).map_err(|_| error);
        }
        Err((error, _)) => return Err(error),
    };
    // SAFETY: getuid/geteuid take no arguments and have no memory preconditions.
    let (uid, ruid) = unsafe { (libc::geteuid(), libc::getuid()) };
    if before.pbi_status == libc::SZOMB || (before.pbi_uid != uid && before.pbi_ruid != ruid) {
        return Ok(None);
    }
    let path = native_process_path(pid);
    let Some(after) = native_process_info(pid).map_err(|(error, _)| error)? else {
        return Ok(None);
    };
    if after.pbi_status == libc::SZOMB {
        return Ok(None);
    }
    if (
        before.pbi_start_tvsec,
        before.pbi_start_tvusec,
        before.pbi_uid,
        before.pbi_ruid,
    ) != (
        after.pbi_start_tvsec,
        after.pbi_start_tvusec,
        after.pbi_uid,
        after.pbi_ruid,
    ) {
        return Err("进程在应用验证期间发生变化，暂不能安全确认清理。".into());
    }
    path.map(Some).map_err(|error| {
        format!("无法读取用户进程 {pid} 的真实执行路径：{error}。暂不能安全确认清理。")
    })
}

fn read_app_metadata(path: &Path) -> Result<InstalledApp, String> {
    if !path.is_absolute() {
        return Err("应用路径不是绝对路径。".into());
    }
    // Active applications may live outside the installed inventory roots or be
    // launched through a symlink. Resolve only this OS-reported bundle, then
    // verify its actual Info.plist without traversing a symlinked Contents tree.
    let bundle = fs::canonicalize(path).map_err(|error| error.to_string())?;
    let info = bundle.join("Contents/Info.plist");
    let ancestors = safe_ancestors(info.parent().ok_or("应用元数据路径无效。")?)?;
    let before = fs::symlink_metadata(&info).map_err(|error| error.to_string())?;
    if !before.is_file() || before.file_type().is_symlink() || before.len() >= 4 * 1024 * 1024 {
        return Err("应用元数据不是可验证的普通文件。".into());
    }
    let plist = plist::Value::from_file(&info).map_err(|error| error.to_string())?;
    let dictionary = plist.as_dictionary().ok_or("应用元数据格式无效。")?;
    let id = dictionary
        .get("CFBundleIdentifier")
        .and_then(plist::Value::as_string)
        .filter(|id| !id.trim().is_empty())
        .ok_or("应用没有可验证的标识。")?;
    let name = dictionary
        .get("CFBundleDisplayName")
        .or_else(|| dictionary.get("CFBundleName"))
        .and_then(plist::Value::as_string)
        .map(str::to_owned)
        .unwrap_or_else(|| app_name_without_extension(&file_name(path)).to_string());
    let after = fs::symlink_metadata(&info).map_err(|error| error.to_string())?;
    if Fingerprint::of(&before) != Fingerprint::of(&after)
        || safe_ancestors(info.parent().ok_or("应用元数据路径无效。")?)? != ancestors
        || fs::canonicalize(path).map_err(|error| error.to_string())? != bundle
    {
        return Err("应用元数据在读取期间发生变化。".into());
    }
    Ok(InstalledApp {
        id: id.into(),
        name,
        path: bundle,
    })
}

fn app_inventory<F: Fn(ScanProgress)>(home: &Path, ctx: &mut Context<'_, F>) -> AppInventory {
    let mut result = AppInventory {
        apps: Vec::new(),
        complete: true,
        active_paths: HashSet::new(),
        active_apps: Vec::new(),
        active_complete: false,
    };
    if ctx.cancel.load(Ordering::Relaxed) || ctx.stopped {
        ctx.stopped = true;
        result.complete = false;
        return result;
    }
    let running = running_app_paths();
    if let Err(error) = running.and_then(|paths| result.refresh_active_paths(paths)) {
        ctx.warn(format!("{error}。已取消所有候选的默认勾选。"));
    }
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
                    if is_app_bundle(&child)
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
                if is_app_bundle(&child) {
                    if let Ok(app) = read_app_metadata(&child) {
                        result.apps.push(app);
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
            .is_some_and(|id| !apps.complete || apps.has_possible_owner(id))
        || snapshot.validated.values().any(|item| {
            // Sources can overlap even across categories (for example an orphan
            // support directory and its known cache subtree). Keep selections
            // disjoint so selecting a parent cannot silently include another row.
            item.path.starts_with(path) || path.starts_with(&item.path)
        })
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
        || apps.owner_uncertain(owner_name)
        || extra_cache
        || runtime_cache
        || orphan_id.is_some()
        || matches!(
            rule,
            Rule::Installer
                | Rule::PartialDownload
                | Rule::OrphanMetadata
                | Rule::OrphanPreference
                | Rule::OrphanState
        );
    let category = candidate_category(rule, orphan_id.as_deref());
    let installed = matches!(rule, Rule::Installer)
        .then(|| installed_app_for_installer(apps, path))
        .flatten();
    let pure_cache = extra_cache && pure_regenerable_cache(&snapshot.home, root, owner_name);
    let reason = if pure_cache {
        "只包含可重新生成的下载、编译或网页缓存，不含账户、设置或文档；候选及全部子项均超过保留天数，相关工具当前未运行。"
    } else if matches!(rule, Rule::PartialDownload) {
        "未完成的浏览器下载，超过保留天数且没有继续下载；这类临时文件无法直接打开或使用。"
    } else if installed.is_some() {
        "下载目录中的旧安装包，对应的应用已经安装在本机；安装包本身不再参与运行。"
    } else if extra_cache {
        "已知可重新生成的下载、编译或应用缓存，候选及全部子项均超过保留天数。可能影响离线工作、首次打开或重新编译；先退出相关工具并手动确认。不会包含整个应用数据目录。"
    } else if matches!(rule, Rule::OrphanState) {
        "疑似已卸载应用留下的窗口恢复状态。未找到同标识或同开发者的应用并不证明已经卸载，可能影响恢复上次打开的窗口；需手动确认。"
    } else if matches!(rule, Rule::OrphanMetadata) {
        "旧应用数据文件夹：未在常见应用目录找到同标识或同开发者的应用，但这不证明已经卸载。已避开已知数据库、账号状态、模型与游戏数据，其他设置和个人文件仍必须打开查看并手动确认。"
    } else if orphan_id.is_some() {
        "未在常见应用目录找到同标识或同开发者的应用；这不证明已经卸载，可能属于便携应用或后台组件。仅列出旧缓存或偏好元数据，需手动核对。"
    } else if runtime_cache {
        "开发工具或浏览器自动化的运行时缓存，可能仍被项目引用、用于离线工作或运行程序。即使时间较旧也需要手动核对，清理后可能重新下载。"
    } else if apps.owner_uncertain(owner_name) {
        "存在相关标识或同开发者的应用，但无法确认此项目所属的应用。仅列出超过保留天数的缓存或日志；必须手动核对，不能依据应用分组判断可清理。"
    } else {
        match rule {
            Rule::Cache => "位于用户缓存目录，候选文件及所有子项均超过保留天数；已避开浏览器状态、数据库、模型与游戏数据。应用下次使用时可能重新生成。",
            Rule::Log => "位于用户日志目录且超过保留天数；仅移动日志文件，保留目录及最近日志。",
            Rule::Installer => "下载目录中的旧磁盘镜像或安装包，可能用于重装或离线使用；需手动确认。",
            _ => "疑似旧偏好元数据，需要手动核对。",
        }
    };
    // `risk` keeps the default-selection policy; `safety` is the clearer
    // deletability verdict shown to the user. Uncertain ownership or an
    // incomplete running-app list never yields a `safe` verdict.
    let uncertain = !apps.active_complete || apps.owner_uncertain(owner_name);
    let safety = if !review {
        "safe"
    } else if matches!(rule, Rule::OrphanMetadata) {
        "caution"
    } else if !uncertain
        && (pure_cache || installed.is_some() || matches!(rule, Rule::PartialDownload))
    {
        "safe"
    } else {
        "review"
    };
    let impact = match rule {
        Rule::Cache | Rule::RebuildableCache if orphan_id.is_some() => {
            "对应应用似乎已卸载；若仍在使用，下次启动会重新生成缓存。".to_string()
        }
        Rule::RebuildableCache if pure_cache => cache_impact(owner_name).to_string(),
        Rule::Cache | Rule::RebuildableCache if runtime_cache || extra_cache => {
            "可能需要重新下载；离线工作或正在引用它的项目可能受影响。".to_string()
        }
        Rule::Cache | Rule::RebuildableCache => cache_impact(owner_name).to_string(),
        Rule::Log => "无法再查看这些旧日志或崩溃报告。".to_string(),
        Rule::Installer => match installed {
            Some(app) => format!("已安装「{}」；以后重装需要重新下载安装包。", app.name),
            None => "未找到对应的已安装应用；可能还没安装，或留作离线重装。".to_string(),
        },
        Rule::PartialDownload => "下载需要重新开始。".to_string(),
        Rule::OrphanPreference => "若以后重新安装该应用，设置会恢复默认。".to_string(),
        Rule::OrphanState => "该应用不会恢复上次打开的窗口。".to_string(),
        Rule::OrphanMetadata => {
            "可能包含该应用的设置、文档或账户数据；放回前，应用内无法找回。".to_string()
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
        safety,
        impact,
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
        Rule::PartialDownload => "download",
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
        cleanup_uncertain: false,
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
                let rule = if is_installer(&child) {
                    Rule::Installer
                } else if is_partial_download(&child) {
                    Rule::PartialDownload
                } else {
                    continue;
                };
                let owner = file_name(&child);
                add_candidate(
                    &mut snapshot,
                    &mut ctx,
                    &apps,
                    &child,
                    &downloads,
                    rule,
                    None,
                    &owner,
                    None,
                );
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
                    if !reverse_dns(&id) || protected_label(&id) || apps.has_possible_owner(&id) {
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
                    && !apps.has_possible_owner(&owner);
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
        Rule::Installer | Rule::PartialDownload => snapshot.home.join("Downloads"),
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
        if !apps.complete || apps.has_possible_owner(id) {
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

pub fn cleanup(
    snapshot: &mut ScanSnapshot,
    ids: &[String],
    progress: impl Fn(CleanupProgress),
) -> Result<CleanupReport, String> {
    if snapshot.cleanup_uncertain {
        return Err("上次清理结果未确认，请先检查 Finder 并重新扫描。".into());
    }
    let apps = current_app_inventory(&snapshot.home);
    cleanup_with_inventory(
        snapshot,
        ids,
        progress,
        crate::native_trash::move_recorded,
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
    trash_item: impl Fn(
        &Path,
        u64,
        u64,
    ) -> Result<crate::native_trash::Receipt, crate::native_trash::MoveError>,
    mut apps: AppInventory,
    refresh_running: impl Fn() -> Result<HashSet<PathBuf>, String>,
) -> Result<CleanupReport, String> {
    if snapshot.cleanup_uncertain {
        return Err("上次清理结果未确认，请先检查 Finder 并重新扫描。".into());
    }
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
            apps.refresh_active_paths(running)?;
            validate_item(snapshot, &item, &apps)
        });
        let outcome = validation
            .map_err(crate::native_trash::MoveError::failed)
            .and_then(|()| {
                let identity = item.fingerprint.identity;
                let receipt = trash_item(&item.path, identity.device, identity.inode)?;
                if receipt.matches(identity.device, identity.inode) {
                    Ok(())
                } else {
                    Err(crate::native_trash::MoveError {
                        message: "移动回执与原文件不符，请检查原位置与废纸篓。".into(),
                        unknown: true,
                    })
                }
            });
        let result = CleanupResult {
            id: (*id).clone(),
            path: item.path.to_string_lossy().into_owned(),
            bytes: item.manifest.bytes,
            error: outcome.as_ref().err().map(|error| error.message.clone()),
            outcome: match &outcome {
                Ok(_) => "moved",
                Err(error) if error.unknown => "unknown",
                Err(_) => "failed",
            },
        };
        if outcome.is_ok() {
            snapshot.validated.remove(id.as_str());
            snapshot.report.items.retain(|entry| entry.id != **id);
            report.bytes_moved = report.bytes_moved.saturating_add(result.bytes);
            report.moved.push(result);
        } else {
            if result.outcome == "unknown" {
                snapshot.invalidate_cleanup();
            }
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
        cleanup_with_inventory(
            snapshot,
            ids,
            progress,
            |path, device, inode| {
                trash_item(path)?;
                if fs::symlink_metadata(path).is_ok() {
                    return Err("fixture source still exists".into());
                }
                Ok(crate::native_trash::Receipt {
                    destination: PathBuf::from("/test-trash/fixture"),
                    device,
                    inode,
                })
            },
            no_apps(),
            || Ok(HashSet::new()),
        )
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
                Rule::Installer | Rule::PartialDownload => self.home.join("Downloads"),
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
                safety: "safe",
                impact: "fixture".into(),
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
                cleanup_uncertain: false,
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
            active_apps: vec![],
            active_complete: true,
        }
    }

    #[test]
    fn installers_match_installed_apps_and_partial_downloads_are_recognised() {
        let app = |name: &str| InstalledApp {
            id: format!("com.example.{}", normalized_name(name)),
            name: name.into(),
            path: PathBuf::from(format!("/Applications/{name}.app")),
        };
        let apps = AppInventory {
            apps: vec![
                app("Google Chrome"),
                app("Docker"),
                app("WeChat"),
                app("Go"),
            ],
            ..no_apps()
        };
        let installed = |file: &str| {
            installed_app_for_installer(&apps, Path::new(file)).map(|app| app.name.clone())
        };
        assert_eq!(
            installed("googlechrome.dmg").as_deref(),
            Some("Google Chrome")
        );
        assert_eq!(installed("Docker-arm64.dmg").as_deref(), Some("Docker"));
        assert_eq!(installed("Docker 4.31.0.dmg").as_deref(), Some("Docker"));
        assert_eq!(installed("WeChatMac.dmg").as_deref(), Some("WeChat"));
        assert_eq!(installed("Install Docker.pkg").as_deref(), Some("Docker"));
        // Short or unrelated names never claim an installed owner.
        assert_eq!(installed("Go1.22.pkg"), None);
        assert_eq!(installed("DockerDesktopBackup.dmg"), None);
        assert_eq!(installed("OtherTool.pkg"), None);

        assert!(is_partial_download(Path::new("movie.mp4.crdownload")));
        assert!(is_partial_download(Path::new("archive.zip.part")));
        assert!(is_partial_download(Path::new("photo.jpg.download")));
        assert!(!is_partial_download(Path::new("notes.part1.rar")));
        assert!(!is_installer(Path::new("movie.mp4.crdownload")));
    }

    #[test]
    fn only_known_regenerable_cache_roots_are_rated_safe() {
        let home = Path::new("/Users/example");
        assert!(pure_regenerable_cache(
            home,
            &home.join(".npm/_cacache"),
            "npm"
        ));
        assert!(pure_regenerable_cache(
            home,
            &home.join("Library/Developer/Xcode/DerivedData"),
            "Xcode"
        ));
        assert!(pure_regenerable_cache(
            home,
            &home.join("Library/Application Support/Slack/GPUCache"),
            "Slack"
        ));
        // pnpm's store is hard-linked into projects and sandbox caches can hold state.
        assert!(!pure_regenerable_cache(
            home,
            &home.join("Library/pnpm/store"),
            "pnpm"
        ));
        assert!(!pure_regenerable_cache(
            home,
            &home.join("Library/Containers/com.example.app/Data/Library/Caches"),
            "com.example.app"
        ));
        assert!(!pure_regenerable_cache(
            home,
            &home.join("Library/Application Support/Slack/IndexedDB"),
            "Slack"
        ));
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
        assert!(matches!(
            inventory(&path, None, true, &mut |_, _| Ok(())),
            Err(WalkError::Protected)
        ));
        fs::remove_file(path.join("state.sqlite")).unwrap();
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
            |path, device, inode| {
                fs::rename(path, &destination).map_err(|error| error.to_string())?;
                Ok(crate::native_trash::Receipt {
                    destination: destination.clone(),
                    device,
                    inode,
                })
            },
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
            |_, _, _| {
                called.set(true);
                unreachable!("running inventory failed before mutation")
            },
            no_apps(),
            || Err("process permission denied".into()),
        )
        .unwrap();
        assert_eq!(failed.failed.len(), 1);
        assert!(!called.get());
        assert!(second.exists());
    }

    #[test]
    fn unconfirmed_native_receipts_preserve_candidates_without_claiming_failure_or_success() {
        let fixture = Fixture::new();
        let file = fixture.file("Library/Logs/unconfirmed.log");
        let mut snapshot = fixture.snapshot_for(&file, Rule::Log);
        let report = cleanup_with_inventory(
            &mut snapshot,
            &["fixture-1".into()],
            |_| {},
            |_, _, _| {
                Err(crate::native_trash::MoveError {
                    message: "receipt unavailable".into(),
                    unknown: true,
                })
            },
            no_apps(),
            || Ok(HashSet::new()),
        )
        .unwrap();
        assert!(report.moved.is_empty());
        assert_eq!(report.failed[0].outcome, "unknown");
        assert_eq!(report.bytes_moved, 0);
        assert_eq!(snapshot.report.items.len(), 1);
        assert!(cleanup_with_inventory(
            &mut snapshot,
            &["fixture-1".into()],
            |_| {},
            |_, _, _| panic!("unknown results cannot be retried"),
            no_apps(),
            || panic!("no inventory refresh before refusing a retry")
        )
        .unwrap_err()
        .contains("重新扫描"));
        // A newly committed scan is the only way to replace uncertainty.
        let mut snapshot = fixture.snapshot_for(&file, Rule::Log);
        let report = cleanup_with_inventory(
            &mut snapshot,
            &["fixture-1".into()],
            |_| {},
            |_, device, inode| {
                Ok(crate::native_trash::Receipt {
                    destination: fixture.home.join("wrong-receipt"),
                    device,
                    inode: inode + 1,
                })
            },
            no_apps(),
            || Ok(HashSet::new()),
        )
        .unwrap();
        assert_eq!(report.failed[0].outcome, "unknown");
        assert!(file.exists());
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
