//! Bounded macOS directory metadata batches. This module never reads file contents.
//! ABI: local SDK sys/attr.h, sys/unistd.h and getattrlistbulk(2). Attributes are
//! aligned to four bytes, including u64/off_t; records start on eight-byte boundaries.
//! Only ordinary-file metadata replaces stat. Directories, links, unsupported
//! fields and per-entry failures retain the caller's guarded stat/error handling.

#![cfg(target_os = "macos")]

use std::ffi::{CString, OsString};
use std::io;
use std::marker::PhantomData;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

const WINDOW_BYTES: usize = 128 * 1024;
const ATTR_CMN_ERROR: u32 = 0x2000_0000;
const SF_DATALESS: u32 = 0x4000_0000;
const VREG: u32 = 1;
#[cfg(test)]
const VDIR: u32 = 2;
const IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES: i32 = 3;
const IOPOL_SCOPE_THREAD: i32 = 1;
const IOPOL_MATERIALIZE_DATALESS_FILES_OFF: i32 = 1;
const COMMON_FIELDS_BYTES: usize = 56;
const REQUIRED_COMMON: u32 = libc::ATTR_CMN_NAME
    | libc::ATTR_CMN_DEVID
    | libc::ATTR_CMN_OBJTYPE
    | libc::ATTR_CMN_FLAGS
    | libc::ATTR_CMN_FILEID;
const REQUIRED_FILE: u32 = libc::ATTR_FILE_LINKCOUNT | libc::ATTR_FILE_ALLOCSIZE;

unsafe extern "C" {
    fn getiopolicy_np(iotype: libc::c_int, scope: libc::c_int) -> libc::c_int;
    fn setiopolicy_np(iotype: libc::c_int, scope: libc::c_int, policy: libc::c_int) -> libc::c_int;
}

#[derive(Debug)]
pub enum BulkError {
    /// Only raised before any batch was consumed, so an independent guarded
    /// readdir fallback cannot count entries twice.
    Unsupported(io::Error),
    /// No filesystem call is made when materialization cannot be disabled.
    PolicySetup(io::Error),
    Io(io::Error),
    Cancelled,
}

impl std::fmt::Display for BulkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported(error) => write!(f, "批量目录属性不受支持：{error}"),
            Self::PolicySetup(error) => write!(f, "无法禁止云盘内容下载：{error}"),
            Self::Io(error) => write!(f, "{error}"),
            Self::Cancelled => f.write_str("目录读取已停止"),
        }
    }
}

impl std::error::Error for BulkError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileMetadata {
    /// ATTR_FILE_ALLOCSIZE includes every fork, preserving stat's block accounting.
    pub allocated_bytes: u64,
    pub device: u64,
    pub inode: u64,
    pub flags: u32,
    pub link_count: u32,
}

#[derive(Debug)]
pub struct BulkEntry {
    pub name: OsString,
    /// Some only when all required ordinary-file fields are valid. Never construct
    /// fs::Metadata from this information: it does not contain all stat fields.
    pub metadata: Option<FileMetadata>,
}

/// Thread-bound guard: moving its restoration onto another thread is forbidden.
struct MaterializationGuard {
    previous: i32,
    _thread_bound: PhantomData<Rc<()>>,
}

impl MaterializationGuard {
    fn enter() -> Result<Self, BulkError> {
        // SAFETY: these calls use the SDK's scalar-only ABI and the current thread.
        let previous = unsafe {
            getiopolicy_np(
                IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
                IOPOL_SCOPE_THREAD,
            )
        };
        if previous < 0 {
            return Err(BulkError::PolicySetup(io::Error::last_os_error()));
        }
        // SAFETY: the OFF policy does not permit file materialization or change permissions.
        let result = unsafe {
            setiopolicy_np(
                IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
                IOPOL_SCOPE_THREAD,
                IOPOL_MATERIALIZE_DATALESS_FILES_OFF,
            )
        };
        if result != 0 {
            return Err(BulkError::PolicySetup(io::Error::last_os_error()));
        }
        Ok(Self {
            previous,
            _thread_bound: PhantomData,
        })
    }
}

impl Drop for MaterializationGuard {
    fn drop(&mut self) {
        // SAFETY: !Send keeps this guard on its creating thread. On restoration
        // failure the safer OFF policy remains in effect rather than enabling downloads.
        let _ = unsafe {
            setiopolicy_np(
                IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
                IOPOL_SCOPE_THREAD,
                self.previous,
            )
        };
    }
}

/// Use on the thread that actually executes stat/readdir fallback. Setting this
/// policy only on a parent scan thread does not protect separately spawned workers.
pub fn without_materialization<T>(
    operation: impl FnOnce() -> io::Result<T>,
) -> Result<T, BulkError> {
    let _guard = MaterializationGuard::enter()?;
    operation().map_err(BulkError::Io)
}

pub struct BulkDirectory {
    fd: OwnedFd,
    // u64 backing guarantees the alignment required by the native API. Parsing
    // uses byte copies, never aligned references into packed attribute records.
    window: Vec<u64>,
    consumed: bool,
    finished: bool,
}

impl BulkDirectory {
    pub fn open(path: &Path) -> Result<Self, BulkError> {
        let path = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| BulkError::Io(invalid_data("目录路径包含 NUL")))?;
        let fd = without_materialization(|| {
            // SAFETY: path is NUL-terminated; returned ownership is immediately
            // transferred to OwnedFd. Reject links in any component, including ancestors.
            let fd = unsafe {
                libc::open(
                    path.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW_ANY,
                )
            };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: open returned a new owned descriptor.
            let fd = unsafe { OwnedFd::from_raw_fd(fd) };
            check_directory(fd.as_raw_fd())?;
            Ok(fd)
        })?;
        Ok(Self {
            fd,
            window: vec![0; WINDOW_BYTES / std::mem::size_of::<u64>()],
            consumed: false,
            finished: false,
        })
    }

    /// A fresh bounded vector per batch, not an entire-directory buffer. EDEADLK
    /// and per-entry errors must be reported as unknown/partial, never retried with
    /// materialization enabled. Do not mix readdir with this descriptor's offset.
    pub fn next_batch(
        &mut self,
        cancel: &AtomicBool,
    ) -> Result<Option<Vec<io::Result<BulkEntry>>>, BulkError> {
        if cancel.load(Ordering::Relaxed) {
            self.finished = true;
            return Err(BulkError::Cancelled);
        }
        if self.finished {
            return Ok(None);
        }
        let mut requested = libc::attrlist {
            bitmapcount: 5,
            reserved: 0,
            commonattr: REQUIRED_COMMON | libc::ATTR_CMN_RETURNED_ATTRS | ATTR_CMN_ERROR,
            volattr: 0,
            dirattr: 0,
            fileattr: REQUIRED_FILE,
            forkattr: 0,
        };
        let result = without_materialization(|| {
            check_directory(self.fd.as_raw_fd())?;
            // SAFETY: attrlist uses libc's SDK-matching layout. The 128 KiB buffer
            // is writable and eight-byte aligned; the OS returns a bounded entry count.
            let count = unsafe {
                libc::getattrlistbulk(
                    self.fd.as_raw_fd(),
                    (&mut requested as *mut libc::attrlist).cast(),
                    self.window.as_mut_ptr().cast(),
                    WINDOW_BYTES,
                    u64::from(libc::FSOPT_PACK_INVAL_ATTRS),
                )
            };
            if count < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(count as usize)
            }
        });
        let count = match result {
            Ok(0) => {
                self.finished = true;
                return Ok(None);
            }
            Ok(count) => count,
            Err(BulkError::Io(error)) if !self.consumed && unsupported(&error) => {
                self.finished = true;
                return Err(BulkError::Unsupported(error));
            }
            Err(error) => {
                self.finished = true;
                return Err(error);
            }
        };
        // SAFETY: the window is initialized u64 storage; reading its byte view is
        // valid for the full allocation. Each record and variable reference is checked.
        let bytes =
            unsafe { std::slice::from_raw_parts(self.window.as_ptr().cast::<u8>(), WINDOW_BYTES) };
        let entries = match parse_batch(bytes, count, cancel) {
            Ok(entries) => entries,
            Err(error) => {
                self.finished = true;
                return Err(BulkError::Io(error));
            }
        };
        if cancel.load(Ordering::Relaxed) {
            self.finished = true;
            return Err(BulkError::Cancelled);
        }
        self.consumed = true;
        Ok(Some(entries))
    }
}

fn unsupported(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::ENOTSUP | libc::EINVAL | libc::ENOSYS)
    )
}

fn check_directory(fd: libc::c_int) -> io::Result<()> {
    let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: fstat writes the complete stat structure on success only.
    if unsafe { libc::fstat(fd, metadata.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fstat succeeded above.
    let metadata = unsafe { metadata.assume_init() };
    if metadata.st_mode & libc::S_IFMT != libc::S_IFDIR {
        return Err(io::Error::from_raw_os_error(libc::ENOTDIR));
    }
    if metadata.st_flags & SF_DATALESS != 0 {
        return Err(io::Error::from_raw_os_error(libc::EDEADLK));
    }
    Ok(())
}

fn invalid_data(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl Cursor<'_> {
    fn take<const N: usize>(&mut self) -> io::Result<[u8; N]> {
        let end = self
            .offset
            .checked_add(N)
            .ok_or_else(|| invalid_data("属性长度溢出"))?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| invalid_data("属性记录被截断"))?;
        self.offset = end;
        Ok(value.try_into().expect("checked fixed-size slice"))
    }
    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_ne_bytes(self.take()?))
    }
    fn i32(&mut self) -> io::Result<i32> {
        Ok(i32::from_ne_bytes(self.take()?))
    }
    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_ne_bytes(self.take()?))
    }
    fn i64(&mut self) -> io::Result<i64> {
        Ok(i64::from_ne_bytes(self.take()?))
    }
}

fn parse_record(record: &[u8]) -> io::Result<io::Result<BulkEntry>> {
    let mut cursor = Cursor {
        bytes: record,
        offset: 4,
    };
    let common = cursor.u32()?;
    let _volume = cursor.u32()?;
    let _directory = cursor.u32()?;
    let file = cursor.u32()?;
    let _fork = cursor.u32()?;
    let error = cursor.u32()?;
    if error != 0 {
        return Ok(Err(io::Error::from_raw_os_error(error as i32)));
    }
    if common & libc::ATTR_CMN_NAME == 0 || record.len() < COMMON_FIELDS_BYTES {
        return Err(invalid_data("目录条目没有有效名称或公共属性"));
    }
    let reference_at = cursor.offset;
    let displacement = cursor.i32()?;
    let length = cursor.u32()? as usize;
    let name_start = reference_at
        .checked_add_signed(displacement as isize)
        .ok_or_else(|| invalid_data("名称偏移越界"))?;
    let name_end = name_start
        .checked_add(length)
        .ok_or_else(|| invalid_data("名称长度溢出"))?;
    let name = record
        .get(name_start..name_end)
        .ok_or_else(|| invalid_data("名称超出属性记录"))?;
    if name_start < COMMON_FIELDS_BYTES
        || name.len() < 2
        || name.last() != Some(&0)
        || name[..name.len() - 1].contains(&0)
        || name.contains(&b'/')
        || name == b".\0"
        || name == b"..\0"
    {
        return Err(invalid_data("目录条目名称无效"));
    }
    let name = OsString::from_vec(name[..name.len() - 1].to_vec());
    let device = cursor.i32()? as u64;
    let kind = cursor.u32()?;
    let flags = cursor.u32()?;
    let inode = cursor.u64()?;
    let metadata = if kind == VREG
        && common & REQUIRED_COMMON == REQUIRED_COMMON
        && file & REQUIRED_FILE == REQUIRED_FILE
    {
        let link_count = cursor.u32()?;
        let allocation = cursor.i64()?;
        if allocation < 0 || link_count == 0 || name_start < cursor.offset {
            None
        } else {
            Some(FileMetadata {
                allocated_bytes: allocation as u64,
                device,
                inode,
                flags,
                link_count,
            })
        }
    } else {
        // The API omits directory/file attribute groups according to object kind,
        // even with FSOPT_PACK_INVAL_ATTRS. No fixed all-kinds struct is safe here.
        None
    };
    Ok(Ok(BulkEntry { name, metadata }))
}

fn parse_batch(
    bytes: &[u8],
    count: usize,
    cancel: &AtomicBool,
) -> io::Result<Vec<io::Result<BulkEntry>>> {
    if count > bytes.len() / 32 {
        return Err(invalid_data("目录条目数量超出缓冲容量"));
    }
    let mut entries = Vec::with_capacity(count);
    let mut offset = 0_usize;
    for _ in 0..count {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let header = bytes
            .get(offset..offset + 4)
            .ok_or_else(|| invalid_data("目录记录头被截断"))?;
        let length = u32::from_ne_bytes(header.try_into().expect("four-byte header")) as usize;
        if length < 32 || !length.is_multiple_of(8) {
            return Err(invalid_data("目录记录长度或对齐无效"));
        }
        let end = offset
            .checked_add(length)
            .ok_or_else(|| invalid_data("目录记录长度溢出"))?;
        let record = bytes
            .get(offset..end)
            .ok_or_else(|| invalid_data("目录记录超出缓冲容量"))?;
        entries.push(parse_record(record)?);
        offset = end;
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};
    use std::fs;
    use std::os::darwin::fs::MetadataExt as DarwinMetadataExt;
    use std::os::unix::fs::{symlink, MetadataExt};
    use std::sync::atomic::AtomicU64;

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);
    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "mac-sweep-bulk-{}-{}",
                std::process::id(),
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

    fn record(kind: u32, common: u32, file: u32, name: &[u8]) -> Vec<u8> {
        let mut result = vec![0_u8; COMMON_FIELDS_BYTES];
        result[4..8].copy_from_slice(&common.to_ne_bytes());
        result[16..20].copy_from_slice(&file.to_ne_bytes());
        result[36..40].copy_from_slice(&7_i32.to_ne_bytes());
        result[40..44].copy_from_slice(&kind.to_ne_bytes());
        result[48..56].copy_from_slice(&42_u64.to_ne_bytes());
        if kind != VDIR {
            result.extend_from_slice(&2_u32.to_ne_bytes());
            result.extend_from_slice(&4096_i64.to_ne_bytes());
        }
        let start = result.len();
        result[28..32].copy_from_slice(&((start - 28) as i32).to_ne_bytes());
        result[32..36].copy_from_slice(&(name.len() as u32).to_ne_bytes());
        result.extend_from_slice(name);
        result.resize(result.len().next_multiple_of(8), 0);
        let length = result.len() as u32;
        result[..4].copy_from_slice(&length.to_ne_bytes());
        result
    }

    #[test]
    fn parser_handles_four_byte_u64_alignment_and_kind_specific_fields() {
        let regular = record(VREG, REQUIRED_COMMON, REQUIRED_FILE, b"file\0");
        let entry = parse_record(&regular).unwrap().unwrap();
        assert_eq!(entry.name, "file");
        assert_eq!(
            entry.metadata.unwrap(),
            FileMetadata {
                allocated_bytes: 4096,
                device: 7,
                inode: 42,
                flags: 0,
                link_count: 2
            }
        );
        let directory = record(VDIR, REQUIRED_COMMON, 0, b"dir\0");
        assert_eq!(parse_record(&directory).unwrap().unwrap().metadata, None);
        let link = record(5, REQUIRED_COMMON, REQUIRED_FILE, b"link\0");
        assert_eq!(parse_record(&link).unwrap().unwrap().metadata, None);
        let missing = record(
            VREG,
            REQUIRED_COMMON & !libc::ATTR_CMN_FLAGS,
            REQUIRED_FILE,
            b"missing\0",
        );
        assert_eq!(parse_record(&missing).unwrap().unwrap().metadata, None);
        let invalid_file = record(VREG, REQUIRED_COMMON, 0, b"invalid\0");
        assert_eq!(parse_record(&invalid_file).unwrap().unwrap().metadata, None);
    }

    #[test]
    fn parser_rejects_out_of_record_names_invalid_paths_and_bad_record_lengths() {
        for name in [b"../x\0".as_slice(), b".\0", b"..\0", b"a\0b\0", b"empty"] {
            assert!(parse_record(&record(VREG, REQUIRED_COMMON, REQUIRED_FILE, name)).is_err());
        }
        let mut bad = record(VREG, REQUIRED_COMMON, REQUIRED_FILE, b"file\0");
        bad[28..32].copy_from_slice(&i32::MAX.to_ne_bytes());
        assert!(parse_record(&bad).is_err());
        bad[28..32].copy_from_slice(&(-28_i32).to_ne_bytes());
        assert!(parse_record(&bad).is_err());
        let mut truncated = record(VREG, REQUIRED_COMMON, REQUIRED_FILE, b"file\0");
        truncated[..4].copy_from_slice(&u32::MAX.to_ne_bytes());
        assert!(parse_batch(&truncated, 1, &AtomicBool::new(false)).is_err());
        assert!(parse_batch(&truncated, usize::MAX, &AtomicBool::new(false)).is_err());
    }

    #[test]
    fn parser_preserves_non_utf8_and_reports_dataless_errors_without_fake_metadata() {
        let bytes = record(VREG, REQUIRED_COMMON, REQUIRED_FILE, b"raw-\xff\0");
        let entry = parse_record(&bytes).unwrap().unwrap();
        assert_eq!(entry.name.as_bytes(), b"raw-\xff");
        let mut error = vec![0_u8; 32];
        error[..4].copy_from_slice(&32_u32.to_ne_bytes());
        error[4..8]
            .copy_from_slice(&(ATTR_CMN_ERROR | libc::ATTR_CMN_RETURNED_ATTRS).to_ne_bytes());
        error[24..28].copy_from_slice(&(libc::EDEADLK as u32).to_ne_bytes());
        let parsed = parse_batch(&error, 1, &AtomicBool::new(false)).unwrap();
        assert_eq!(
            parsed
                .into_iter()
                .next()
                .unwrap()
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EDEADLK)
        );
    }

    #[test]
    fn policy_is_restored_after_success_and_failure_and_e_deadlk_is_not_unsupported() {
        // SAFETY: scalar-only query of this test's own thread policy.
        let before = unsafe {
            getiopolicy_np(
                IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
                IOPOL_SCOPE_THREAD,
            )
        };
        without_materialization(|| {
            // SAFETY: query of the current thread while the scoped guard is alive.
            assert_eq!(
                unsafe {
                    getiopolicy_np(
                        IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
                        IOPOL_SCOPE_THREAD,
                    )
                },
                IOPOL_MATERIALIZE_DATALESS_FILES_OFF
            );
            Ok(())
        })
        .unwrap();
        assert!(
            without_materialization::<()>(|| Err(io::Error::from_raw_os_error(libc::EDEADLK)))
                .is_err()
        );
        // SAFETY: query of the same current thread after both guards were dropped.
        assert_eq!(
            unsafe {
                getiopolicy_np(
                    IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
                    IOPOL_SCOPE_THREAD,
                )
            },
            before
        );
        assert!(!unsupported(&io::Error::from_raw_os_error(libc::EDEADLK)));
        assert!(unsupported(&io::Error::from_raw_os_error(libc::ENOTSUP)));
    }

    #[test]
    fn real_bulk_metadata_matches_stat_for_allocation_identity_and_hard_links() {
        let fixture = Fixture::new();
        let regular = fixture.0.join("regular");
        fs::write(&regular, vec![1_u8; 4096]).unwrap();
        let sparse = fs::File::create(fixture.0.join("sparse")).unwrap();
        sparse.set_len(1024 * 1024).unwrap();
        fs::hard_link(&regular, fixture.0.join("hardlink")).unwrap();
        symlink("regular", fixture.0.join("symlink")).unwrap();
        fs::create_dir(fixture.0.join("folder")).unwrap();
        let resource = fixture.0.join("resource");
        fs::write(&resource, vec![1_u8; 4096]).unwrap();
        let resource_path = CString::new(resource.as_os_str().as_bytes()).unwrap();
        let fork_name = CString::new("com.apple.ResourceFork").unwrap();
        let fork = vec![2_u8; 8192];
        // SAFETY: valid owned fixture pathname and an initialized byte payload.
        assert_eq!(
            unsafe {
                libc::setxattr(
                    resource_path.as_ptr(),
                    fork_name.as_ptr(),
                    fork.as_ptr().cast(),
                    fork.len(),
                    0,
                    0,
                )
            },
            0
        );
        let source = CString::new(regular.as_os_str().as_bytes()).unwrap();
        let clone = CString::new(fixture.0.join("clone").as_os_str().as_bytes()).unwrap();
        // SAFETY: clone reads/writes only files within this newly created fixture.
        assert_eq!(
            unsafe { libc::clonefile(source.as_ptr(), clone.as_ptr(), 0) },
            0
        );
        let baseline: HashMap<_, _> = fs::read_dir(&fixture.0)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (entry.file_name(), entry.metadata().unwrap())
            })
            .collect();
        let mut directory = BulkDirectory::open(&fixture.0).unwrap();
        let mut entries = HashMap::new();
        while let Some(batch) = directory.next_batch(&AtomicBool::new(false)).unwrap() {
            for entry in batch {
                let entry = entry.unwrap();
                assert!(entries.insert(entry.name.clone(), entry.metadata).is_none());
            }
        }
        assert_eq!(entries.len(), baseline.len());
        let mut seen = HashSet::new();
        let mut bulk_total = 0_u64;
        let mut stat_total = 0_u64;
        for (name, stat) in &baseline {
            let duplicate =
                stat.is_file() && stat.nlink() > 1 && !seen.insert((stat.dev(), stat.ino()));
            let allocation = if duplicate { 0 } else { stat.blocks() * 512 };
            stat_total += allocation;
            if let Some(metadata) = entries.get(name).unwrap() {
                assert!(stat.is_file());
                assert_eq!(metadata.allocated_bytes, stat.blocks() * 512, "{name:?}");
                assert_eq!(metadata.device, stat.dev());
                assert_eq!(metadata.inode, stat.ino());
                assert_eq!(metadata.link_count as u64, stat.nlink());
                assert_eq!(metadata.flags, stat.st_flags());
                bulk_total += if duplicate {
                    0
                } else {
                    metadata.allocated_bytes
                };
            } else {
                assert!(
                    !stat.is_file(),
                    "ordinary fixture must use its valid bulk metadata"
                );
                bulk_total += allocation;
            }
        }
        assert_eq!(bulk_total, stat_total);
        assert_eq!(entries.get(std::ffi::OsStr::new("symlink")).unwrap(), &None);
        assert_eq!(entries.get(std::ffi::OsStr::new("folder")).unwrap(), &None);
    }

    #[test]
    fn batches_are_bounded_cancel_between_reads_and_never_recurse() {
        let fixture = Fixture::new();
        for number in 0..4000 {
            fs::write(fixture.0.join(format!("file-{number:04}")), b"data").unwrap();
        }
        fs::create_dir(fixture.0.join("nested")).unwrap();
        fs::write(fixture.0.join("nested/never-enumerated"), b"keep").unwrap();
        let cancel = AtomicBool::new(false);
        let mut directory = BulkDirectory::open(&fixture.0).unwrap();
        let first = directory.next_batch(&cancel).unwrap().unwrap();
        assert!(!first.is_empty() && first.len() < 4000);
        cancel.store(true, Ordering::Relaxed);
        assert!(matches!(
            directory.next_batch(&cancel),
            Err(BulkError::Cancelled)
        ));
        let mut directory = BulkDirectory::open(&fixture.0).unwrap();
        let mut count = 0;
        while let Some(batch) = directory.next_batch(&AtomicBool::new(false)).unwrap() {
            for entry in batch {
                assert_ne!(entry.unwrap().name, "never-enumerated");
                count += 1;
            }
        }
        assert_eq!(count, 4001);
    }

    #[test]
    fn open_rejects_symlink_roots_and_symlink_ancestors() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.0.join("real")).unwrap();
        fs::create_dir(fixture.0.join("real/nested")).unwrap();
        symlink("real", fixture.0.join("link")).unwrap();
        assert!(BulkDirectory::open(&fixture.0.join("link")).is_err());
        assert!(BulkDirectory::open(&fixture.0.join("link/nested")).is_err());
    }
}
