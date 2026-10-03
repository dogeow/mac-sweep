//! Persistent directory locations. Favorites never become cleanup candidates.
use crate::analysis_entry::{self, EntryState};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_FAVORITES: usize = 64;
const MAX_BYTES: usize = 64 * 1024;
static STORE_LOCK: Mutex<()> = Mutex::new(());
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
const MISSING: &str = "FAVORITE_MISSING:这个收藏目录已被移动或删除。";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FavoriteDirectory {
    pub id: String,
    pub path: String,
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct FavoritesList {
    pub favorites: Vec<FavoriteDirectory>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Store {
    version: u32,
    favorites: Vec<FavoriteDirectory>,
}
impl Default for Store {
    fn default() -> Self {
        Self {
            version: 1,
            favorites: Vec::new(),
        }
    }
}

fn unique_token() -> String {
    format!(
        "{}-{}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    )
}
fn configuration_error(error: io::Error) -> String {
    if error.kind() == io::ErrorKind::PermissionDenied {
        "无法读取或保存收藏记录，请检查应用数据目录的访问权限。原文件不会被覆盖。".into()
    } else {
        "暂时无法读取或保存收藏记录，原文件不会被覆盖，请稍后重试。".into()
    }
}
fn invalid_configuration() -> String {
    "收藏记录损坏、超出上限或格式不支持，已保留原文件，暂时不能更改收藏。".into()
}
fn directory_error(error: io::Error) -> String {
    if error.kind() == io::ErrorKind::NotFound {
        MISSING.into()
    } else if error.kind() == io::ErrorKind::PermissionDenied {
        "没有权限读取这个收藏目录，请检查访问权限后重试。".into()
    } else {
        "暂时无法读取这个收藏目录，请检查目录是否仍可访问。".into()
    }
}
fn valid_path(value: &str) -> bool {
    let path = Path::new(value);
    path.is_absolute()
        && !value.contains('\0')
        && !path
            .components()
            .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
        && path.components().collect::<PathBuf>().as_os_str() == path.as_os_str()
}
fn validate(store: &Store) -> Result<(), String> {
    let mut ids = HashSet::new();
    let mut paths = HashSet::new();
    if store.version != 1 || store.favorites.len() > MAX_FAVORITES {
        return Err(invalid_configuration());
    }
    for favorite in &store.favorites {
        if favorite.id.is_empty()
            || favorite.id.len() > 160
            || favorite.id.contains('\0')
            || favorite.name.trim().is_empty()
            || favorite.name.len() > 1024
            || favorite.name.contains('\0')
            || !valid_path(&favorite.path)
            || !ids.insert(&favorite.id)
            || !paths.insert(&favorite.path)
        {
            return Err(invalid_configuration());
        }
    }
    Ok(())
}
fn directory_storage(data_dir: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(data_dir) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(true),
        Ok(_) => Err(invalid_configuration()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(configuration_error(error)),
    }
}
fn load(data_dir: &Path) -> Result<Store, String> {
    if !directory_storage(data_dir)? {
        return Ok(Store::default());
    }
    let path = data_dir.join("favorites.json");
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
        Ok(_) => return Err(invalid_configuration()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Store::default()),
        Err(error) => return Err(configuration_error(error)),
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(&path)
        .map_err(configuration_error)?;
    if !file.metadata().map_err(configuration_error)?.is_file() {
        return Err(invalid_configuration());
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(configuration_error)?;
    if bytes.len() > MAX_BYTES {
        return Err(invalid_configuration());
    }
    let store: Store = serde_json::from_slice(&bytes).map_err(|_| invalid_configuration())?;
    validate(&store)?;
    Ok(store)
}
fn save(data_dir: &Path, store: &Store) -> Result<(), String> {
    validate(store)?;
    let bytes = serde_json::to_vec_pretty(store).map_err(|_| invalid_configuration())?;
    if bytes.len() > MAX_BYTES {
        return Err("收藏记录过大，请移除部分收藏后重试。".into());
    }
    if !directory_storage(data_dir)? {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(data_dir)
            .map_err(configuration_error)?;
    }
    if !directory_storage(data_dir)? {
        return Err(invalid_configuration());
    }
    let destination = data_dir.join("favorites.json");
    match fs::symlink_metadata(&destination) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
        Ok(_) => return Err(invalid_configuration()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(configuration_error(error)),
    }
    let temporary = data_dir.join(format!(".favorites-{}.tmp", unique_token()));
    let mut created = false;
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temporary)
            .map_err(configuration_error)?;
        created = true;
        file.write_all(&bytes).map_err(configuration_error)?;
        file.sync_all().map_err(configuration_error)?;
        fs::rename(&temporary, &destination).map_err(configuration_error)?;
        created = false;
        Ok(())
    })();
    if created {
        let _ = fs::remove_file(&temporary);
    }
    result
}
fn list(store: Store) -> FavoritesList {
    FavoritesList {
        favorites: store.favorites,
        warning: None,
    }
}

pub fn get(data_dir: &Path) -> FavoritesList {
    match STORE_LOCK.lock() {
        Ok(_guard) => match load(data_dir) {
            Ok(store) => list(store),
            Err(warning) => FavoritesList {
                favorites: Vec::new(),
                warning: Some(warning),
            },
        },
        Err(_) => FavoritesList {
            favorites: Vec::new(),
            warning: Some("收藏记录暂时不可用，请重启应用后重试。".into()),
        },
    }
}

pub fn add_recorded(
    data_dir: &Path,
    path: &Path,
    source_root: &Path,
    device: u64,
    inode: u64,
) -> Result<FavoritesList, String> {
    let _guard = STORE_LOCK
        .lock()
        .map_err(|_| "收藏记录暂时不可用，请重启应用后重试。")?;
    let mut store = load(data_dir)?;
    match analysis_entry::check(path, source_root, device, inode)? {
        EntryState::Ready => {}
        EntryState::Missing => return Err(MISSING.into()),
        EntryState::Changed => return Err("这个目录已变化，请重新分析后再收藏。".into()),
    }
    let metadata = fs::symlink_metadata(path).map_err(directory_error)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("只能收藏真实的文件夹。".into());
    }
    if metadata.dev() != device || metadata.ino() != inode {
        return Err("这个目录已变化，请重新分析后再收藏。".into());
    }
    let canonical = path.canonicalize().map_err(directory_error)?;
    if canonical != path || !canonical.starts_with(source_root) {
        return Err("这个目录路径已变化，请重新分析后再收藏。".into());
    }
    let value = canonical
        .to_str()
        .ok_or("这个目录名称无法保存为收藏。")?
        .to_owned();
    if store
        .favorites
        .iter()
        .any(|favorite| favorite.path == value)
    {
        return Ok(list(store));
    }
    if store.favorites.len() >= MAX_FAVORITES {
        return Err("最多收藏 64 个目录，请先移除一个收藏。".into());
    }
    store.favorites.push(FavoriteDirectory {
        id: format!("favorite-{}", unique_token()),
        path: value,
        name: canonical
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/".into()),
    });
    save(data_dir, &store)?;
    Ok(list(store))
}

pub fn remove(data_dir: &Path, favorite_id: &str) -> Result<FavoritesList, String> {
    let _guard = STORE_LOCK
        .lock()
        .map_err(|_| "收藏记录暂时不可用，请重启应用后重试。")?;
    let mut store = load(data_dir)?;
    let count = store.favorites.len();
    store
        .favorites
        .retain(|favorite| favorite.id != favorite_id);
    if store.favorites.len() != count {
        save(data_dir, &store)?;
    }
    Ok(list(store))
}

pub fn resolve(data_dir: &Path, favorite_id: &str) -> Result<FavoriteDirectory, String> {
    let _guard = STORE_LOCK
        .lock()
        .map_err(|_| "收藏记录暂时不可用，请重启应用后重试。")?;
    let store = load(data_dir)?;
    let favorite = store
        .favorites
        .into_iter()
        .find(|favorite| favorite.id == favorite_id)
        .ok_or("这个收藏已不在列表中，请刷新收藏列表。")?;
    let path = Path::new(&favorite.path);
    let metadata = fs::symlink_metadata(path).map_err(directory_error)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("这个收藏位置已不再是文件夹，请移除后重新添加。".into());
    }
    if path.canonicalize().map_err(directory_error)? != path {
        return Err("这个收藏路径已变化，请重新添加收藏。".into());
    }
    // A favorite is a saved location; no inode identity survives restarts.
    Ok(favorite)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("mac-sweep-favorites-test-{}", unique_token()));
            fs::create_dir(&root).unwrap();
            Self(root.canonicalize().unwrap())
        }
        fn data(&self) -> PathBuf {
            self.0.join("app-data")
        }
        fn directory(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            fs::create_dir_all(&path).unwrap();
            path.canonicalize().unwrap()
        }
        fn add(&self, path: &Path) -> Result<FavoritesList, String> {
            let metadata = fs::symlink_metadata(path).unwrap();
            add_recorded(&self.data(), path, &self.0, metadata.dev(), metadata.ino())
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn favorites_persist_with_stable_ids_and_duplicate_locations_keep_the_existing_id() {
        let fixture = Fixture::new();
        assert!(get(&fixture.data()).favorites.is_empty());
        assert!(!fixture.data().exists());
        let directory = fixture.directory("目录 with spaces");
        let original = fixture.add(&directory).unwrap().favorites.remove(0);
        let restarted = get(&fixture.data());
        assert_eq!(restarted.favorites, vec![original.clone()]);
        assert_eq!(
            fixture.add(&directory).unwrap().favorites,
            vec![original.clone()]
        );
        assert_eq!(resolve(&fixture.data(), &original.id).unwrap(), original);
        assert_eq!(
            fs::metadata(fixture.data()).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(fixture.data().join("favorites.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn saved_locations_can_be_removed_when_missing_and_opened_after_same_name_rebuild() {
        let fixture = Fixture::new();
        let directory = fixture.directory("position");
        let original = fixture.add(&directory).unwrap().favorites.remove(0);
        fs::rename(&directory, fixture.0.join("original-position")).unwrap();
        assert_eq!(resolve(&fixture.data(), &original.id).unwrap_err(), MISSING);
        assert_eq!(get(&fixture.data()).favorites, vec![original.clone()]);
        fs::create_dir(&directory).unwrap();
        assert_eq!(resolve(&fixture.data(), &original.id).unwrap(), original);
        fs::remove_dir(&directory).unwrap();
        assert!(remove(&fixture.data(), &original.id)
            .unwrap()
            .favorites
            .is_empty());
    }

    #[test]
    fn damaged_configuration_is_reported_and_mutations_preserve_its_exact_bytes() {
        let fixture = Fixture::new();
        let directory = fixture.directory("folder");
        fs::create_dir(fixture.data()).unwrap();
        for bytes in [
            b"not-json".to_vec(),
            vec![b' '; MAX_BYTES + 1],
            br#"{"version":2,"favorites":[]}"#.to_vec(),
        ] {
            fs::write(fixture.data().join("favorites.json"), &bytes).unwrap();
            assert!(get(&fixture.data()).warning.is_some());
            assert!(fixture.add(&directory).is_err());
            assert!(remove(&fixture.data(), "unknown").is_err());
            assert_eq!(
                fs::read(fixture.data().join("favorites.json")).unwrap(),
                bytes
            );
        }
    }

    #[test]
    fn configuration_symlinks_and_nonregular_files_never_redirect_reads_or_writes() {
        let fixture = Fixture::new();
        let directory = fixture.directory("folder");
        fs::create_dir(fixture.data()).unwrap();
        let unrelated = fixture.0.join("unrelated.json");
        let bytes = br#"{"version":1,"favorites":[]}"#;
        fs::write(&unrelated, bytes).unwrap();
        let config = fixture.data().join("favorites.json");
        symlink(&unrelated, &config).unwrap();
        assert!(get(&fixture.data()).warning.is_some());
        assert!(fixture.add(&directory).is_err());
        assert!(remove(&fixture.data(), "unknown").is_err());
        assert_eq!(fs::read(&unrelated).unwrap(), bytes);
        fs::remove_file(&config).unwrap();
        fs::create_dir(&config).unwrap();
        assert!(get(&fixture.data()).warning.is_some());
        assert!(fixture.add(&directory).is_err());
    }

    #[test]
    fn hard_limit_duplicate_schema_and_oversized_save_do_not_replace_the_store() {
        let fixture = Fixture::new();
        let directory = fixture.directory("new-folder");
        let favorites: Vec<_> = (0..MAX_FAVORITES)
            .map(|index| FavoriteDirectory {
                id: format!("saved-{index}"),
                path: fixture
                    .0
                    .join(format!("location-{index}"))
                    .to_str()
                    .unwrap()
                    .to_owned(),
                name: format!("location-{index}"),
            })
            .collect();
        save(
            &fixture.data(),
            &Store {
                version: 1,
                favorites: favorites.clone(),
            },
        )
        .unwrap();
        let original = fs::read(fixture.data().join("favorites.json")).unwrap();
        // Loading is configuration-only even though none of these locations exist.
        assert_eq!(get(&fixture.data()).favorites.len(), MAX_FAVORITES);
        assert!(fixture.add(&directory).unwrap_err().contains("64"));
        assert_eq!(
            fs::read(fixture.data().join("favorites.json")).unwrap(),
            original
        );
        let duplicate = Store {
            version: 1,
            favorites: vec![favorites[0].clone(), favorites[0].clone()],
        };
        assert!(validate(&duplicate).is_err());
        let large = Store {
            version: 1,
            favorites: favorites
                .into_iter()
                .enumerate()
                .map(|(index, mut favorite)| {
                    favorite.path = format!("/{index}/{}", "x".repeat(1500));
                    favorite
                })
                .collect(),
        };
        assert!(save(&fixture.data(), &large).is_err());
        assert_eq!(
            fs::read(fixture.data().join("favorites.json")).unwrap(),
            original
        );
    }

    #[test]
    fn concurrent_additions_do_not_lose_updates() {
        let fixture = Fixture::new();
        let mut threads = Vec::new();
        for index in 0..8 {
            let path = fixture.directory(&format!("folder-{index}"));
            let data = fixture.data();
            let root = fixture.0.clone();
            threads.push(std::thread::spawn(move || {
                let metadata = fs::metadata(&path).unwrap();
                add_recorded(&data, &path, &root, metadata.dev(), metadata.ino()).unwrap();
            }));
        }
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(get(&fixture.data()).favorites.len(), 8);
    }

    #[test]
    fn stale_identity_ancestor_redirect_and_non_directory_are_rejected_before_persisting() {
        let fixture = Fixture::new();
        let directory = fixture.directory("parent/folder");
        let metadata = fs::metadata(&directory).unwrap();
        let identity = (metadata.dev(), metadata.ino());
        fs::rename(fixture.0.join("parent"), fixture.0.join("outside-parent")).unwrap();
        symlink(fixture.0.join("outside-parent"), fixture.0.join("parent")).unwrap();
        assert!(add_recorded(
            &fixture.data(),
            &directory,
            &fixture.0,
            identity.0,
            identity.1
        )
        .is_err());
        assert!(!fixture.data().exists());
        let file = fixture.0.join("file.bin");
        fs::write(&file, b"private temporary contents").unwrap();
        let metadata = fs::metadata(&file).unwrap();
        assert!(add_recorded(
            &fixture.data(),
            &file,
            &fixture.0,
            metadata.dev(),
            metadata.ino()
        )
        .is_err());
        assert!(!fixture.data().exists());
    }

    #[test]
    fn permission_failures_are_never_reported_as_missing_or_allowed_to_overwrite() {
        let fixture = Fixture::new();
        let directory = fixture.directory("folder");
        fixture.add(&directory).unwrap();
        let config = fixture.data().join("favorites.json");
        let original = fs::read(&config).unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o0)).unwrap();
        if fs::read(&config).is_err() {
            assert!(get(&fixture.data()).warning.is_some());
            assert!(fixture.add(&directory).is_err());
            assert!(remove(&fixture.data(), "anything").is_err());
        }
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(fs::read(&config).unwrap(), original);
        assert!(
            !directory_error(io::Error::from(io::ErrorKind::PermissionDenied))
                .starts_with("FAVORITE_MISSING:")
        );
        assert!(
            !directory_error(io::Error::from(io::ErrorKind::Interrupted))
                .starts_with("FAVORITE_MISSING:")
        );
    }
}
