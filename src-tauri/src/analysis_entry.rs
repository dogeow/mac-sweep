//! Check a recorded analysis entry before handing its path to Finder.
//! Missing entries are a normal consequence of editing files outside the app.

use std::{fs, io, os::unix::fs::MetadataExt, path::Path};

#[derive(Debug, PartialEq, Eq)]
pub enum EntryState {
    Ready,
    Missing,
    Changed,
}

fn read_error(error: io::Error) -> Result<EntryState, String> {
    match error.kind() {
        io::ErrorKind::NotFound => Ok(EntryState::Missing),
        io::ErrorKind::NotADirectory => Ok(EntryState::Changed),
        io::ErrorKind::PermissionDenied => {
            Err("没有权限读取这个项目，请检查访问权限后重试。".into())
        }
        _ if error.raw_os_error() == Some(libc::ELOOP) => Ok(EntryState::Changed),
        _ => Err("暂时无法读取这个项目，请稍后重试。".into()),
    }
}

pub fn check(
    path: &Path,
    source_root: &Path,
    device: u64,
    inode: u64,
) -> Result<EntryState, String> {
    if !path.is_absolute() || !path.starts_with(source_root) {
        return Ok(EntryState::Changed);
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => return read_error(error),
    };
    if metadata.dev() != device || metadata.ino() != inode {
        return Ok(EntryState::Changed);
    }
    let root_now = match source_root.canonicalize() {
        Ok(root) => root,
        Err(error) => return read_error(error),
    };
    if root_now != source_root {
        return Ok(EntryState::Changed);
    }
    // Resolve the parent, preserving the ability to reveal a recorded symlink
    // itself without following that entry's target. An ancestor replacement is
    // rejected even when the selected file's own device and inode are unchanged.
    if let Some(parent) = path.parent() {
        let parent_now = match parent.canonicalize() {
            Ok(parent) => parent,
            Err(error) => return read_error(error),
        };
        if parent_now != parent {
            return Ok(EntryState::Changed);
        }
    }
    Ok(EntryState::Ready)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        os::unix::fs::symlink,
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "mac-sweep-entry-test-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn identity(path: &Path) -> (u64, u64) {
        let metadata = fs::symlink_metadata(path).unwrap();
        (metadata.dev(), metadata.ino())
    }

    #[test]
    fn moved_file_and_deleted_parent_are_missing_instead_of_finder_errors() {
        let fixture = Fixture::new();
        let parent = fixture.0.join("folder");
        fs::create_dir(&parent).unwrap();
        let file = parent.join("file.bin");
        fs::write(&file, b"temporary fixture").unwrap();
        let (dev, ino) = identity(&file);
        assert_eq!(
            check(&file, &fixture.0, dev, ino).unwrap(),
            EntryState::Ready
        );
        fs::rename(&file, fixture.0.join("moved.bin")).unwrap();
        assert_eq!(
            check(&file, &fixture.0, dev, ino).unwrap(),
            EntryState::Missing
        );
        fs::remove_dir(&parent).unwrap();
        assert_eq!(
            check(&file, &fixture.0, dev, ino).unwrap(),
            EntryState::Missing
        );
    }

    #[test]
    fn replacement_at_the_same_path_is_changed() {
        let fixture = Fixture::new();
        let file = fixture.0.join("file.bin");
        fs::write(&file, b"original").unwrap();
        let (dev, ino) = identity(&file);
        fs::rename(&file, fixture.0.join("original.bin")).unwrap();
        fs::write(&file, b"replacement").unwrap();
        assert_eq!(
            check(&file, &fixture.0, dev, ino).unwrap(),
            EntryState::Changed
        );
    }

    #[test]
    fn replaced_ancestor_cannot_reveal_outside_the_recorded_scope() {
        let fixture = Fixture::new();
        let root = fixture.0.join("scope");
        let parent = root.join("parent");
        fs::create_dir_all(&parent).unwrap();
        let file = parent.join("file.bin");
        fs::write(&file, b"original").unwrap();
        let (dev, ino) = identity(&file);
        let outside = fixture.0.join("outside");
        fs::rename(&parent, &outside).unwrap();
        symlink(&outside, &parent).unwrap();
        assert_eq!(identity(&file), (dev, ino));
        assert_eq!(check(&file, &root, dev, ino).unwrap(), EntryState::Changed);
    }

    #[test]
    fn a_recorded_symlink_can_be_revealed_without_following_its_target() {
        let fixture = Fixture::new();
        let link = fixture.0.join("link");
        symlink(fixture.0.join("missing-target"), &link).unwrap();
        let (dev, ino) = identity(&link);
        assert_eq!(
            check(&link, &fixture.0, dev, ino).unwrap(),
            EntryState::Ready
        );
    }

    #[test]
    fn permission_and_other_io_failures_are_never_classified_as_deletion() {
        assert!(read_error(io::Error::from(io::ErrorKind::PermissionDenied)).is_err());
        assert!(read_error(io::Error::from(io::ErrorKind::Interrupted)).is_err());
        assert_eq!(
            read_error(io::Error::from(io::ErrorKind::NotFound)).unwrap(),
            EntryState::Missing
        );
        assert_eq!(
            read_error(io::Error::from(io::ErrorKind::NotADirectory)).unwrap(),
            EntryState::Changed
        );
        assert_eq!(
            read_error(io::Error::from_raw_os_error(libc::ELOOP)).unwrap(),
            EntryState::Changed
        );
    }
}
