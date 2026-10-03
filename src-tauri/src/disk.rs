//! Filesystem capacity is a separate measurement from traversable file sizes.
//! In APFS, the Data volume shares a container with System, Preboot, VM, etc.

use serde::Serialize;
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageSummary {
    /// APFS container capacity, or the filesystem capacity on other filesystems.
    pub total_bytes: u64,
    /// APFS container free space. This does not promise immediate purgeable reclamation.
    pub available_bytes: u64,
    pub used_bytes: u64,
    /// The selected volume's physical allocation, when macOS reports it.
    pub volume_used_bytes: Option<u64>,
    /// Known other volumes in the same APFS container; excludes unknown overhead.
    pub other_volumes_bytes: Option<u64>,
    pub filesystem: String,
    pub mount_point: String,
    /// False for an ordinary folder even though its containing volume has capacity.
    pub is_volume_root: bool,
    pub source: String,
    pub warnings: Vec<String>,
}

/// Read capacity independently of the directory traversal. Never changes a disk.
pub fn storage_summary(path: &Path) -> Result<StorageSummary, String> {
    let mut summary = filesystem_summary(path)?;
    #[cfg(target_os = "macos")]
    {
        use std::ffi::OsStr;
        let mount = Path::new(&summary.mount_point);
        match diskutil_plist(&[OsStr::new("info"), OsStr::new("-plist"), mount.as_os_str()]) {
            Ok(info) => {
                let apfs = info
                    .as_dictionary()
                    .and_then(|dictionary| string(dictionary, "FilesystemType"))
                    .is_some_and(|kind| kind.eq_ignore_ascii_case("apfs"));
                let list = if apfs {
                    match diskutil_plist(&[
                        OsStr::new("apfs"),
                        OsStr::new("list"),
                        OsStr::new("-plist"),
                    ]) {
                        Ok(list) => Some(list),
                        Err(_) => {
                            summary.warnings.push(
                                "无法读取其他 APFS 卷的占用；这部分空间仍包含在磁盘总占用中。"
                                    .into(),
                            );
                            None
                        }
                    }
                } else {
                    None
                };
                if !apply_diskutil(&mut summary, &info, list.as_ref()) {
                    summary.warnings.push(
                        "macOS 未返回完整的磁盘容量信息，当前使用文件系统报告的容量。".into(),
                    );
                }
            }
            Err(_) => summary.warnings.push(
                "macOS 磁盘容量信息暂不可用，当前使用文件系统报告的容量，无法区分其他卷与快照。"
                    .into(),
            ),
        }
    }
    Ok(summary)
}

fn filesystem_summary(path: &Path) -> Result<StorageSummary, String> {
    let c_path =
        CString::new(path.as_os_str().as_bytes()).map_err(|_| "路径包含无效字符。".to_string())?;
    let mut status = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: c_path is terminated; the kernel initializes status on success.
    if unsafe { libc::statvfs(c_path.as_ptr(), status.as_mut_ptr()) } != 0 {
        return Err(format!(
            "无法读取磁盘容量：{}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: statvfs succeeded.
    let status = unsafe { status.assume_init() };
    let block_size = if status.f_frsize > 0 {
        status.f_frsize
    } else {
        status.f_bsize
    };
    let total = (status.f_blocks as u64).saturating_mul(block_size);
    let available = (status.f_bavail as u64)
        .saturating_mul(block_size)
        .min(total);
    let (filesystem, mount_point) = volume_context(&c_path)?;
    let is_volume_root = same_directory(path, Path::new(&mount_point));
    Ok(StorageSummary {
        total_bytes: total,
        available_bytes: available,
        used_bytes: total.saturating_sub(available),
        volume_used_bytes: None,
        other_volumes_bytes: None,
        filesystem,
        mount_point,
        is_volume_root,
        source: "statvfs".into(),
        warnings: Vec::new(),
    })
}

fn same_directory(path: &Path, mount: &Path) -> bool {
    match (path.canonicalize(), mount.canonicalize()) {
        (Ok(path), Ok(mount)) => path == mount,
        _ => path == mount,
    }
}

#[cfg(target_os = "macos")]
fn volume_context(path: &CString) -> Result<(String, String), String> {
    let mut status = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: path is terminated; the kernel initializes status on success.
    if unsafe { libc::statfs(path.as_ptr(), status.as_mut_ptr()) } != 0 {
        return Err(format!(
            "无法确定所在磁盘：{}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: statfs succeeded. Read only inside the fixed arrays, including a
    // defensive terminator check rather than assuming the kernel filled one.
    let status = unsafe { status.assume_init() };
    let decode = |value: &[libc::c_char]| {
        let bytes: Vec<u8> = value
            .iter()
            .take_while(|&&byte| byte != 0)
            .map(|&byte| byte as u8)
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
    };
    Ok((decode(&status.f_fstypename), decode(&status.f_mntonname)))
}

#[cfg(not(target_os = "macos"))]
fn volume_context(_: &CString) -> Result<(String, String), String> {
    Ok(("unknown".into(), "/".into()))
}

fn integer(dictionary: &plist::Dictionary, key: &str) -> Option<u64> {
    dictionary.get(key)?.as_unsigned_integer()
}

fn string<'a>(dictionary: &'a plist::Dictionary, key: &str) -> Option<&'a str> {
    dictionary.get(key)?.as_string()
}

fn valid_capacity(total: Option<u64>, free: Option<u64>) -> Option<(u64, u64)> {
    let (total, free) = (total?, free?);
    (total > 0 && free <= total).then_some((total, free))
}

/// Parse only published capacity fields. Logical VolumeSize is not a volume's
/// allocation (and current macOS reports zero for APFS VolumeSize).
fn apply_diskutil(
    summary: &mut StorageSummary,
    info: &plist::Value,
    apfs_list: Option<&plist::Value>,
) -> bool {
    let Some(info) = info.as_dictionary() else {
        return false;
    };
    let is_apfs =
        string(info, "FilesystemType").is_some_and(|kind| kind.eq_ignore_ascii_case("apfs"));
    let initial_capacity = if is_apfs {
        valid_capacity(
            integer(info, "APFSContainerSize"),
            integer(info, "APFSContainerFree"),
        )
    } else {
        valid_capacity(
            integer(info, "VolumeTotalSpace")
                .or_else(|| integer(info, "VolumeSize").filter(|size| *size > 0))
                .or_else(|| integer(info, "TotalSize")),
            integer(info, "VolumeFreeSpace").or_else(|| integer(info, "FreeSpace")),
        )
    };
    let container = if is_apfs {
        apfs_list
            .and_then(plist::Value::as_dictionary)
            .and_then(|list| list.get("Containers"))
            .and_then(plist::Value::as_array)
            .and_then(|containers| {
                containers
                    .iter()
                    .filter_map(plist::Value::as_dictionary)
                    .find(|container| {
                        string(container, "ContainerReference")
                            == string(info, "APFSContainerReference")
                            && string(info, "APFSContainerReference").is_some()
                    })
            })
    } else {
        None
    };
    // Prefer one coherent APFS-list observation of container and volumes.
    let capacity = container
        .and_then(|container| {
            valid_capacity(
                integer(container, "CapacityCeiling"),
                integer(container, "CapacityFree"),
            )
        })
        .or(initial_capacity);
    let Some((total, free)) = capacity else {
        return false;
    };
    summary.total_bytes = total;
    summary.available_bytes = free;
    summary.used_bytes = total - free;
    summary.filesystem = string(info, "FilesystemType")
        .or_else(|| string(info, "FilesystemName"))
        .unwrap_or(&summary.filesystem)
        .into();
    summary.source = if is_apfs {
        "diskutil-apfs"
    } else {
        "diskutil-volume"
    }
    .into();
    summary.volume_used_bytes = if is_apfs {
        integer(info, "CapacityInUse").or_else(|| integer(info, "VolumeUsedSpace"))
    } else {
        Some(total - free)
    };
    summary.other_volumes_bytes = None;

    if let Some(volumes) = container
        .and_then(|container| container.get("Volumes"))
        .and_then(plist::Value::as_array)
    {
        let selected = volumes.iter().position(|value| {
            value
                .as_dictionary()
                .is_some_and(|volume| matches_volume(info, volume))
        });
        if let Some(selected) = selected {
            summary.volume_used_bytes = volumes[selected]
                .as_dictionary()
                .and_then(|volume| integer(volume, "CapacityInUse"))
                .or(summary.volume_used_bytes);
            let other = volumes
                .iter()
                .enumerate()
                .filter(|(index, _)| *index != selected)
                .try_fold(0u64, |sum, (_, value)| {
                    sum.checked_add(integer(value.as_dictionary()?, "CapacityInUse")?)
                });
            // Never make unrelated or corrupt metadata look like a precise
            // breakdown. Container overhead stays in the unclassified residual.
            if let Some(other) = other.filter(|other| *other <= summary.used_bytes) {
                summary.other_volumes_bytes = Some(other);
            } else {
                summary
                    .warnings
                    .push("其他 APFS 卷的容量信息不完整，无法细分这部分占用。".into());
            }
        } else {
            summary
                .warnings
                .push("无法匹配当前 APFS 卷，其他卷的占用暂未细分。".into());
        }
    } else if is_apfs && apfs_list.is_some() {
        summary
            .warnings
            .push("当前 APFS 容器的卷信息不完整，其他卷的占用暂未细分。".into());
    }
    if summary
        .volume_used_bytes
        .is_some_and(|volume| volume > summary.used_bytes)
        || summary
            .volume_used_bytes
            .zip(summary.other_volumes_bytes)
            .is_some_and(|(volume, other)| volume.saturating_add(other) > summary.used_bytes)
    {
        summary.other_volumes_bytes = None;
        summary
            .warnings
            .push("磁盘在读取容量期间发生变化，其他卷的占用暂未细分。".into());
    }
    true
}

fn matches_volume(info: &plist::Dictionary, volume: &plist::Dictionary) -> bool {
    if string(info, "VolumeUUID")
        .zip(string(volume, "APFSVolumeUUID"))
        .is_some_and(|(left, right)| left == right)
    {
        return true;
    }
    let Some(device) = string(info, "DeviceIdentifier") else {
        return false;
    };
    let Some(candidate) = string(volume, "DeviceIdentifier") else {
        return false;
    };
    if device == candidate {
        return true;
    }
    // Booted macOS mounts disk3s1s1, a snapshot of disk3s1. Only strip the
    // snapshot suffix when diskutil explicitly confirms this is a snapshot.
    info.get("APFSSnapshot").and_then(plist::Value::as_boolean) == Some(true)
        && device.rsplit_once('s').is_some_and(|(base, suffix)| {
            base == candidate
                && !suffix.is_empty()
                && suffix.bytes().all(|byte| byte.is_ascii_digit())
        })
}

#[cfg(target_os = "macos")]
fn diskutil_plist(args: &[&std::ffi::OsStr]) -> Result<plist::Value, String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    const OUTPUT_LIMIT: usize = 4 * 1024 * 1024;
    const TIME_LIMIT: Duration = Duration::from_secs(3);
    let mut child = Command::new("/usr/sbin/diskutil")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "无法启动 macOS 磁盘信息工具。".to_string())?;
    let mut stdout = child.stdout.take().ok_or("无法读取磁盘信息输出。")?;
    let (sender, receiver) = mpsc::sync_channel(1);
    // Drain output concurrently to avoid waiting for a process blocked on a
    // full pipe. Retain a bounded buffer; discard excess until it exits.
    std::thread::spawn(move || {
        let mut output = Vec::new();
        let mut chunk = [0u8; 8192];
        let mut oversized = false;
        let result = loop {
            match stdout.read(&mut chunk) {
                Ok(0) => {
                    break if oversized {
                        Err("磁盘信息输出过大。".into())
                    } else {
                        Ok(output)
                    }
                }
                Ok(count) => {
                    let retain = count.min(OUTPUT_LIMIT.saturating_sub(output.len()));
                    output.extend_from_slice(&chunk[..retain]);
                    oversized |= retain < count;
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break Err::<Vec<u8>, String>("无法读取磁盘信息。".into()),
            }
        };
        let _ = sender.send(result);
    });
    let deadline = Instant::now() + TIME_LIMIT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(15)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("读取 macOS 磁盘容量超时或失败。".into());
            }
        }
    };
    if !status.success() {
        return Err("macOS 未能提供磁盘容量信息。".into());
    }
    let output = receiver
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|_| "读取磁盘信息超时。".to_string())??;
    plist::Value::from_reader(std::io::Cursor::new(output))
        .map_err(|_| "macOS 磁盘信息格式无效。".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dictionary(entries: &[(&str, plist::Value)]) -> plist::Value {
        plist::Value::Dictionary(
            entries
                .iter()
                .map(|(key, value)| ((*key).to_string(), value.clone()))
                .collect(),
        )
    }

    fn baseline() -> StorageSummary {
        StorageSummary {
            total_bytes: 1000,
            available_bytes: 400,
            used_bytes: 600,
            volume_used_bytes: None,
            other_volumes_bytes: None,
            filesystem: "apfs".into(),
            mount_point: "/System/Volumes/Data".into(),
            is_volume_root: true,
            source: "statvfs".into(),
            warnings: Vec::new(),
        }
    }

    fn info() -> plist::Value {
        dictionary(&[
            ("FilesystemType", "apfs".into()),
            ("APFSContainerReference", "disk3".into()),
            ("APFSContainerSize", 1000u64.into()),
            ("APFSContainerFree", 100u64.into()),
            ("VolumeSize", 0u64.into()),
            ("CapacityInUse", 800u64.into()),
            ("DeviceIdentifier", "disk3s5".into()),
            ("VolumeUUID", "data-uuid".into()),
        ])
    }

    fn list(volumes: Vec<plist::Value>) -> plist::Value {
        dictionary(&[(
            "Containers",
            plist::Value::Array(vec![dictionary(&[
                ("ContainerReference", "disk3".into()),
                ("CapacityCeiling", 1000u64.into()),
                ("CapacityFree", 80u64.into()),
                ("Volumes", plist::Value::Array(volumes)),
            ])]),
        )])
    }

    fn volume(device: &str, size: u64) -> plist::Value {
        dictionary(&[
            ("DeviceIdentifier", device.into()),
            ("CapacityInUse", size.into()),
        ])
    }

    #[test]
    fn apfs_counts_container_and_other_volumes_without_hiding_overhead() {
        let mut summary = baseline();
        assert!(apply_diskutil(
            &mut summary,
            &info(),
            Some(&list(vec![volume("disk3s5", 800), volume("disk3s1", 100)]))
        ));
        assert_eq!(summary.total_bytes, 1000);
        assert_eq!(summary.available_bytes, 80);
        assert_eq!(summary.used_bytes, 920);
        assert_eq!(summary.volume_used_bytes, Some(800));
        assert_eq!(summary.other_volumes_bytes, Some(100));
        // 20 bytes of container overhead remains unclassified, not falsely
        // attributed to System or Data and not silently removed from the total.
        assert_eq!(
            summary.used_bytes
                - summary.volume_used_bytes.unwrap()
                - summary.other_volumes_bytes.unwrap(),
            20
        );
        assert_eq!(summary.source, "diskutil-apfs");
    }

    #[test]
    fn partial_apfs_metadata_does_not_invent_other_volume_sizes() {
        let mut summary = baseline();
        assert!(apply_diskutil(&mut summary, &info(), None));
        assert_eq!(summary.used_bytes, 900);
        assert_eq!(summary.volume_used_bytes, Some(800));
        assert_eq!(summary.other_volumes_bytes, None);
    }

    #[test]
    fn snapshot_device_matches_only_with_explicit_snapshot_metadata() {
        let mut info = info().into_dictionary().unwrap();
        info.insert("DeviceIdentifier".into(), "disk3s1s1".into());
        let candidate = volume("disk3s1", 100).into_dictionary().unwrap();
        assert!(!matches_volume(&info, &candidate));
        info.insert("APFSSnapshot".into(), true.into());
        assert!(matches_volume(&info, &candidate));
        let wrong = volume("disk3s10", 100).into_dictionary().unwrap();
        assert!(!matches_volume(&info, &wrong));
    }

    #[test]
    fn snapshot_selected_volume_is_excluded_from_other_volumes() {
        let mut info = info().into_dictionary().unwrap();
        info.insert("DeviceIdentifier".into(), "disk3s1s1".into());
        info.insert("APFSSnapshot".into(), true.into());
        let mut summary = baseline();
        assert!(apply_diskutil(
            &mut summary,
            &plist::Value::Dictionary(info),
            Some(&list(vec![volume("disk3s5", 800), volume("disk3s1", 100)]))
        ));
        assert_eq!(summary.volume_used_bytes, Some(100));
        assert_eq!(summary.other_volumes_bytes, Some(800));
    }

    #[test]
    fn ordinary_folder_stays_a_folder_with_a_volume_capacity_summary() {
        let mut summary = baseline();
        summary.is_volume_root = false;
        assert!(apply_diskutil(&mut summary, &info(), None));
        assert!(!summary.is_volume_root);
    }

    #[test]
    fn missing_or_inconsistent_fields_keep_breakdown_unknown() {
        let malformed = dictionary(&[("DeviceIdentifier", "disk3s1".into())]);
        let mut summary = baseline();
        assert!(apply_diskutil(
            &mut summary,
            &info(),
            Some(&list(vec![volume("disk3s5", 800), malformed]))
        ));
        assert_eq!(summary.other_volumes_bytes, None);
        assert!(!summary.warnings.is_empty());
        let mut summary = baseline();
        assert!(apply_diskutil(
            &mut summary,
            &info(),
            Some(&list(vec![volume("disk3s5", 800), volume("disk3s1", 900)]))
        ));
        assert_eq!(summary.other_volumes_bytes, None);
    }

    #[test]
    fn invalid_capacity_does_not_replace_filesystem_fallback() {
        let invalid = dictionary(&[
            ("FilesystemType", "apfs".into()),
            ("APFSContainerSize", 100u64.into()),
            ("APFSContainerFree", 200u64.into()),
        ]);
        let mut summary = baseline();
        assert!(!apply_diskutil(&mut summary, &invalid, None));
        assert_eq!(summary.used_bytes, 600);
        assert_eq!(summary.source, "statvfs");
    }

    #[test]
    fn non_apfs_uses_reported_volume_capacity() {
        let info = dictionary(&[
            ("FilesystemType", "hfs".into()),
            ("VolumeTotalSpace", 2000u64.into()),
            ("VolumeFreeSpace", 300u64.into()),
        ]);
        let mut summary = baseline();
        assert!(apply_diskutil(&mut summary, &info, None));
        assert_eq!(summary.total_bytes, 2000);
        assert_eq!(summary.used_bytes, 1700);
        assert_eq!(summary.volume_used_bytes, Some(1700));
        assert_eq!(summary.other_volumes_bytes, None);
        assert_eq!(summary.source, "diskutil-volume");
    }
}
