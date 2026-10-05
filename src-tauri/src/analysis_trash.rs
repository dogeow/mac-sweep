//! Explicit, confirmed analysis-row actions. Never used by automatic scanning.
use crate::analysis_entry::{self, EntryState};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

fn home_scopes(home: &Path) -> Vec<PathBuf> {
    let mut roots = vec![home.to_path_buf()];
    // macOS Data-volume firmlinks keep distinct canonical spellings. Accept the
    // alternate spelling only when the home directory identity actually matches.
    if let Ok(relative) = home.strip_prefix("/") {
        let alias = Path::new("/System/Volumes/Data").join(relative);
        if let (Ok(actual), Ok(other)) = (fs::metadata(home), fs::metadata(&alias)) {
            if actual.dev() == other.dev() && actual.ino() == other.ino() {
                roots.push(alias);
            }
        }
    }
    roots
}

pub fn allowed(path: &Path, home: &Path) -> bool {
    home_scopes(home).iter().any(|root| {
        let Ok(relative) = path.strip_prefix(root) else {
            return false;
        };
        if relative.as_os_str().is_empty() {
            return false;
        }
        // APFS is normally case-insensitive; spelling aliases must not bypass
        // the protection of standard folders and credential locations.
        let normalized = relative.to_string_lossy().to_ascii_lowercase();
        let relative = Path::new(&normalized);
        let exact = [
            "Library",
            "Desktop",
            "Documents",
            "Downloads",
            "Pictures",
            "Movies",
            "Music",
            "Library/Preferences",
            "Library/Containers",
            "Library/Group Containers",
            "Library/Application Support",
        ];
        if exact
            .iter()
            .any(|name| relative == Path::new(&name.to_ascii_lowercase()))
        {
            return false;
        }
        ![".Trash", ".ssh", ".gnupg", "Library/Keychains"]
            .iter()
            .any(|name| relative.starts_with(name.to_ascii_lowercase()))
    })
}

pub fn move_verified(
    path: &Path,
    source_root: &Path,
    home: &Path,
    device: u64,
    inode: u64,
    move_to_trash: impl FnOnce(
        &Path,
        u64,
        u64,
    )
        -> Result<crate::native_trash::Receipt, crate::native_trash::MoveError>,
) -> Result<&'static str, String> {
    if !allowed(path, home) {
        return Err("这个位置受保护，只能在 Finder 中查看。".into());
    }
    match analysis_entry::check(path, source_root, device, inode)? {
        EntryState::Missing => return Ok("missing"),
        EntryState::Changed => return Ok("changed"),
        EntryState::Ready => {}
    }
    let receipt = move_to_trash(path, device, inode).map_err(|error| error.message)?;
    if receipt.matches(device, inode) {
        Ok("moved")
    } else {
        Err("移动结果尚未确认，请在 Finder 检查原位置与废纸篓。".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    #[test]
    fn protected_roots_and_similar_prefixes_are_not_trash_targets() {
        let home = Path::new("/Users/example");
        for path in [
            "/",
            "/System",
            "/Users/example",
            "/Users/example/Library",
            "/Users/example/.ssh/key",
            "/Users/example/Library/Keychains/x",
            "/Users/example-old/a",
        ] {
            assert!(!allowed(Path::new(path), home));
        }
        assert!(allowed(
            Path::new("/Users/example/Code/app/target/debug"),
            home
        ));
    }
    #[test]
    fn only_recorded_identity_is_moved_and_missing_is_not_retried() {
        let home = std::env::temp_dir().join(format!(
            "mac-sweep-trash-check-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&home).unwrap();
        let home = home.canonicalize().unwrap();
        let path = home.join("owned.bin");
        fs::write(&path, b"own fixture").unwrap();
        let meta = fs::symlink_metadata(&path).unwrap();
        assert_eq!(
            move_verified(
                &path,
                &home,
                &home,
                meta.dev(),
                meta.ino() + 1,
                |_, _, _| panic!("replaced identity cannot move")
            )
            .unwrap(),
            "changed"
        );
        let destination = home.join("moved.bin");
        assert_eq!(
            move_verified(
                &path,
                &home,
                &home,
                meta.dev(),
                meta.ino(),
                |selected, device, inode| {
                    fs::rename(selected, &destination).map_err(|e| e.to_string())?;
                    Ok(crate::native_trash::Receipt {
                        destination: destination.clone(),
                        device,
                        inode,
                    })
                }
            )
            .unwrap(),
            "moved"
        );
        assert_eq!(
            move_verified(
                &path,
                &home,
                &home,
                meta.dev(),
                meta.ino(),
                |_, _, _| panic!("missing cannot move twice")
            )
            .unwrap(),
            "missing"
        );
        fs::remove_dir_all(home).unwrap();
    }
}
