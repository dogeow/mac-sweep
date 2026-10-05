//! Identity-bound macOS Trash. The native capability never falls back to paths.
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub(crate) struct Receipt {
    pub destination: PathBuf,
    pub device: u64,
    pub inode: u64,
}
impl Receipt {
    pub fn matches(&self, device: u64, inode: u64) -> bool {
        self.device == device && self.inode == inode && self.destination.is_absolute()
    }
}

#[derive(Debug)]
pub(crate) struct MoveError {
    pub message: String,
    pub unknown: bool,
}
impl MoveError {
    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            unknown: false,
        }
    }
}
impl From<String> for MoveError {
    fn from(message: String) -> Self {
        Self::failed(message)
    }
}
impl From<&str> for MoveError {
    fn from(message: &str) -> Self {
        Self::failed(message)
    }
}

pub(crate) fn move_recorded(path: &Path, device: u64, inode: u64) -> Result<Receipt, MoveError> {
    #[cfg(target_os = "macos")]
    {
        let pin = macos::prepare(path, device, inode, None)?;
        pin.move_to_trash()
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (path, device, inode);
        Err("此清理功能仅支持 macOS 原生废纸篓。".into())
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use std::{
        ffi::{c_char, c_void, CStr, CString, OsStr},
        os::unix::ffi::{OsStrExt, OsStringExt},
        ptr::NonNull,
    };
    unsafe extern "C" {
        fn sweep_trash_prepare(
            path: *const c_char,
            device: u64,
            inode: u64,
            hook: Option<extern "C" fn(*mut c_void, i32)>,
            context: *mut c_void,
            error: *mut c_char,
            capacity: usize,
        ) -> *mut c_void;
        fn sweep_trash_release(pin: *mut c_void);
        fn sweep_trash_move(
            pin: *mut c_void,
            destination: *mut c_char,
            path_capacity: usize,
            error: *mut c_char,
            capacity: usize,
        ) -> i32;
    }
    fn message(buffer: &[c_char]) -> String {
        // Native code always NUL-terminates nonempty buffers.
        unsafe {
            CStr::from_ptr(buffer.as_ptr())
                .to_string_lossy()
                .into_owned()
        }
    }
    struct Hook<'a> {
        call: &'a mut dyn FnMut(i32),
        panic: Option<Box<dyn std::any::Any + Send>>,
    }
    extern "C" fn callback(context: *mut c_void, phase: i32) {
        // Never unwind through Objective-C.
        let hook = unsafe { &mut *context.cast::<Hook<'_>>() };
        if hook.panic.is_none() {
            if let Err(panic) =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (hook.call)(phase)))
            {
                hook.panic = Some(panic);
            }
        }
    }
    pub(super) struct Pin {
        raw: NonNull<c_void>,
        device: u64,
        inode: u64,
    }
    impl Drop for Pin {
        fn drop(&mut self) {
            unsafe { sweep_trash_release(self.raw.as_ptr()) };
        }
    }
    pub(super) fn prepare(
        path: &Path,
        device: u64,
        inode: u64,
        hook: Option<&mut dyn FnMut(i32)>,
    ) -> Result<Pin, String> {
        if !path.is_absolute() {
            return Err("移动路径必须为绝对路径。".into());
        }
        let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| "路径包含无效字符。")?;
        let mut error = [0; 1024];
        let mut hook = hook.map(|call| Hook { call, panic: None });
        let context = hook
            .as_mut()
            .map_or(std::ptr::null_mut(), |h| (h as *mut Hook<'_>).cast());
        let raw = unsafe {
            sweep_trash_prepare(
                path.as_ptr(),
                device,
                inode,
                hook.as_ref()
                    .map(|_| callback as extern "C" fn(*mut c_void, i32)),
                context,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        let pin = NonNull::new(raw).map(|raw| Pin { raw, device, inode });
        if let Some(panic) = hook.and_then(|h| h.panic) {
            drop(pin);
            std::panic::resume_unwind(panic);
        }
        pin.ok_or_else(|| message(&error))
    }
    impl Pin {
        pub(super) fn move_to_trash(&self) -> Result<Receipt, MoveError> {
            let mut error = [0; 1024];
            let mut destination = [0; 4096];
            let moved = unsafe {
                sweep_trash_move(
                    self.raw.as_ptr(),
                    destination.as_mut_ptr(),
                    destination.len(),
                    error.as_mut_ptr(),
                    error.len(),
                )
            };
            if moved != 1 {
                return Err(MoveError {
                    message: message(&error),
                    unknown: moved < 0,
                });
            }
            let bytes = unsafe { CStr::from_ptr(destination.as_ptr()).to_bytes() };
            let receipt = Receipt {
                destination: PathBuf::from(std::ffi::OsString::from_vec(bytes.to_vec())),
                device: self.device,
                inode: self.inode,
            };
            if !receipt.matches(self.device, self.inode)
                || receipt.destination == Path::new(OsStr::new(""))
            {
                return Err(MoveError {
                    message: "移动回执无效，请在 Finder 检查。".into(),
                    unknown: true,
                });
            }
            Ok(receipt)
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{symlink, MetadataExt},
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(1);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "mac-sweep-native-pin-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            Self(root.canonicalize().unwrap())
        }
        fn file(&self, path: &str) -> PathBuf {
            let path = self.0.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, b"own test fixture").unwrap();
            path
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn swap_before_reference_acquisition_refuses_outside_sentinel() {
        let f = Fixture::new();
        let source = f.file("Foo/cache.db");
        let outside = f.file("Taxes/cache.db");
        let metadata = fs::symlink_metadata(&source).unwrap();
        let mut hook = |phase| {
            if phase == 0 {
                fs::rename(f.0.join("Foo"), f.0.join("Parked")).unwrap();
                symlink(f.0.join("Taxes"), f.0.join("Foo")).unwrap();
            }
        };
        assert!(macos::prepare(&source, metadata.dev(), metadata.ino(), Some(&mut hook)).is_err());
        assert!(outside.exists());
        assert!(f.0.join("Parked/cache.db").exists());
    }
    #[test]
    fn source_replacement_and_invalid_identity_fail_closed() {
        let f = Fixture::new();
        let source = f.file("source");
        let metadata = fs::symlink_metadata(&source).unwrap();
        assert!(macos::prepare(&source, metadata.dev(), metadata.ino() + 1, None).is_err());
        let mut hook = |phase| {
            if phase == 0 {
                fs::rename(&source, f.0.join("original")).unwrap();
                fs::write(&source, b"replacement").unwrap();
            }
        };
        assert!(macos::prepare(&source, metadata.dev(), metadata.ino(), Some(&mut hook)).is_err());
        assert_eq!(fs::read(&source).unwrap(), b"replacement");
    }
    #[test]
    fn symlinks_and_multiple_hard_links_are_refused_before_native_move() {
        let f = Fixture::new();
        let target = f.file("target");
        let link = f.0.join("link");
        symlink(&target, &link).unwrap();
        let metadata = fs::symlink_metadata(&link).unwrap();
        assert!(macos::prepare(&link, metadata.dev(), metadata.ino(), None).is_err());
        fs::hard_link(&target, f.0.join("second-link")).unwrap();
        let metadata = fs::symlink_metadata(&target).unwrap();
        assert!(macos::prepare(&target, metadata.dev(), metadata.ino(), None).is_err());
        assert!(target.exists());
    }
    #[test]
    fn unlink_and_recreate_same_path_cannot_bind_the_replacement() {
        let f = Fixture::new();
        let source = f.file("source");
        let metadata = fs::symlink_metadata(&source).unwrap();
        let mut hook = |phase| {
            if phase == 0 {
                fs::remove_file(&source).unwrap();
                fs::write(&source, b"unregistered replacement").unwrap();
            }
        };
        assert!(macos::prepare(&source, metadata.dev(), metadata.ino(), Some(&mut hook)).is_err());
        assert_eq!(fs::read(&source).unwrap(), b"unregistered replacement");
    }
    #[test]
    fn a_new_hard_link_after_pin_is_refused_without_trash_access() {
        let f = Fixture::new();
        let source = f.file("source");
        let metadata = fs::symlink_metadata(&source).unwrap();
        let pin = macos::prepare(&source, metadata.dev(), metadata.ino(), None).unwrap();
        fs::hard_link(&source, f.0.join("second-link")).unwrap();
        assert!(pin.move_to_trash().is_err());
        assert!(source.exists());
        assert!(f.0.join("second-link").exists());
    }
    #[test]
    #[ignore = "Moves and immediately restores only self-created fixtures through native Trash"]
    fn native_reference_survives_both_handoff_parent_swaps_and_preserves_symlink_targets() {
        let f = Fixture::new();
        let file = f.file("normal-directory/subfile");
        let folder = file.parent().unwrap();
        let metadata = fs::symlink_metadata(folder).unwrap();
        let receipt = move_recorded(folder, metadata.dev(), metadata.ino()).unwrap();
        fs::rename(&receipt.destination, f.0.join("restored-directory")).unwrap();
        assert!(f.0.join("restored-directory/subfile").exists());
        // Create our own simultaneous Trash name collision; both receipts must
        // refer to their respective identities, never a pre-existing Trash item.
        let first = f.file("one/recorded-fixture.bin");
        let second = f.file("two/recorded-fixture.bin");
        let a = fs::symlink_metadata(&first).unwrap();
        let b = fs::symlink_metadata(&second).unwrap();
        let ra = move_recorded(&first, a.dev(), a.ino()).unwrap();
        let rb = move_recorded(&second, b.dev(), b.ino()).unwrap();
        assert_ne!(ra.destination, rb.destination);
        fs::rename(&ra.destination, f.0.join("restored-first")).unwrap();
        fs::rename(&rb.destination, f.0.join("restored-second")).unwrap();
        for phase in [1, 2] {
            let f = Fixture::new();
            let source = f.file("Foo/cache.db");
            let outside = f.file("Taxes/cache.db");
            let metadata = fs::symlink_metadata(&source).unwrap();
            let swap = || {
                fs::rename(f.0.join("Foo"), f.0.join("Parked")).unwrap();
                symlink(f.0.join("Taxes"), f.0.join("Foo")).unwrap();
            };
            let mut hook = |current| {
                if current == phase {
                    swap();
                }
            };
            let pin =
                macos::prepare(&source, metadata.dev(), metadata.ino(), Some(&mut hook)).unwrap();
            if phase == 2 {
                swap();
            }
            let receipt = pin.move_to_trash().unwrap();
            assert!(receipt.matches(metadata.dev(), metadata.ino()));
            // Restore only the exact native receipt before assertions/fixture disposal.
            fs::rename(&receipt.destination, f.0.join("restored")).unwrap();
            assert!(outside.exists());
            assert!(!f.0.join("Parked/cache.db").exists());
        }
        let f = Fixture::new();
        let target = f.file("target");
        let link = f.0.join("link");
        symlink(&target, &link).unwrap();
        let metadata = fs::symlink_metadata(&link).unwrap();
        // A platform without an opaque symlink identity refuses instead of following its target.
        match move_recorded(&link, metadata.dev(), metadata.ino()) {
            Ok(receipt) => {
                fs::rename(&receipt.destination, f.0.join("restored-link")).unwrap();
                assert!(fs::symlink_metadata(f.0.join("restored-link"))
                    .unwrap()
                    .file_type()
                    .is_symlink());
            }
            Err(_) => assert!(fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()),
        }
        assert!(target.exists());
    }
}
