use std::{fs, io, path::Path};

use crate::{
    SourceError,
    lock::LockFile,
    security::{EntryKind, UntrustedEntry, VendorLimits, VendorTree},
};

/// Verifies tracked vendor content without constructing any network-capable dependency.
pub fn verify_vendor_offline(
    root: &Path,
    lock: &LockFile,
    limits: VendorLimits,
) -> Result<(), SourceError> {
    let mut installed = 0u64;
    for entry in &lock.sources {
        let expected = format!(
            ".agentforge/vendor/{}/{}",
            entry.kind.vendor_segment(),
            entry.id
        );
        if entry.vendor_path != expected {
            return Err(offline(
                &entry.vendor_path,
                "vendor path does not match kind/id",
            ));
        }
        let directory = root.join(
            entry
                .vendor_path
                .replace('/', std::path::MAIN_SEPARATOR_STR),
        );
        let tree = VendorTree::validate(read_tree(&directory)?, limits, installed)?;
        if tree.sha256 != entry.sha256 {
            return Err(offline(
                &entry.vendor_path,
                &format!(
                    "checksum mismatch: expected {}, actual {}",
                    entry.sha256, tree.sha256
                ),
            ));
        }
        if u64::try_from(tree.files.len()).unwrap_or(u64::MAX) != entry.file_count
            || tree.total_bytes != entry.total_bytes
        {
            return Err(offline(
                &entry.vendor_path,
                "file count or total size differs from lock",
            ));
        }
        if tree.executable_content != entry.executable_content {
            return Err(offline(
                &entry.vendor_path,
                "executable-content classification differs from lock",
            ));
        }
        installed = installed
            .checked_add(tree.total_bytes)
            .ok_or(SourceError::SizeOverflow)?;
    }
    Ok(())
}

fn read_tree(root: &Path) -> Result<Vec<UntrustedEntry>, SourceError> {
    if !root.is_dir() {
        return Err(offline(
            &root.display().to_string(),
            "vendor directory is missing",
        ));
    }
    let mut pending = vec![root.to_path_buf()];
    let mut entries = Vec::new();
    while let Some(directory) = pending.pop() {
        let children = fs::read_dir(&directory).map_err(|error| offline_io(&directory, error))?;
        for child in children {
            let path = child.map_err(|error| offline_io(&directory, error))?.path();
            let metadata = fs::symlink_metadata(&path).map_err(|error| offline_io(&path, error))?;
            let relative = path
                .strip_prefix(root)
                .map_err(|_| offline(&path.display().to_string(), "path escaped vendor root"))?
                .to_string_lossy()
                .replace('\\', "/");
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                entries.push(UntrustedEntry {
                    path: relative,
                    kind: EntryKind::Symlink,
                    mode: 0,
                    content: vec![],
                });
            } else if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() {
                if has_multiple_links(&path, &metadata).map_err(|error| offline_io(&path, error))? {
                    entries.push(UntrustedEntry {
                        path: relative,
                        kind: EntryKind::HardLink,
                        mode: 0,
                        content: vec![],
                    });
                } else {
                    let content = fs::read(&path).map_err(|error| offline_io(&path, error))?;
                    entries.push(UntrustedEntry {
                        path: relative,
                        kind: EntryKind::Regular,
                        mode: executable_mode(&metadata),
                        content,
                    });
                }
            } else {
                entries.push(UntrustedEntry {
                    path: relative,
                    kind: special_kind(&file_type),
                    mode: 0,
                    content: vec![],
                });
            }
        }
    }
    Ok(entries)
}

#[cfg(unix)]
fn has_multiple_links(_path: &Path, metadata: &fs::Metadata) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    Ok(metadata.nlink() > 1)
}
#[cfg(windows)]
fn has_multiple_links(path: &Path, _metadata: &fs::Metadata) -> io::Result<bool> {
    use std::{os::windows::ffi::OsStrExt, ptr};
    use windows_sys::Win32::{
        Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
        Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_FLAG_BACKUP_SEMANTICS,
            FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
            GetFileInformationByHandle, OPEN_EXISTING,
        },
    };
    let wide = path
        .as_os_str()
        .encode_wide()
        .chain([0])
        .collect::<Vec<_>>();
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let mut information = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
    let result = unsafe { GetFileInformationByHandle(handle, &mut information) };
    let query_error = (result == 0).then(io::Error::last_os_error);
    let close_result = unsafe { CloseHandle(handle) };
    if let Some(error) = query_error {
        return Err(error);
    }
    if close_result == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(information.nNumberOfLinks > 1)
}
#[cfg(not(any(unix, windows)))]
fn has_multiple_links(_path: &Path, _metadata: &fs::Metadata) -> io::Result<bool> {
    Ok(false)
}
#[cfg(unix)]
fn executable_mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode()
}
#[cfg(not(unix))]
fn executable_mode(_metadata: &fs::Metadata) -> u32 {
    0o644
}
#[cfg(unix)]
fn special_kind(file_type: &fs::FileType) -> EntryKind {
    use std::os::unix::fs::FileTypeExt;
    if file_type.is_fifo() {
        EntryKind::Fifo
    } else if file_type.is_socket() {
        EntryKind::Socket
    } else {
        EntryKind::Device
    }
}
#[cfg(not(unix))]
fn special_kind(_file_type: &fs::FileType) -> EntryKind {
    EntryKind::Device
}

fn offline(path: &str, reason: &str) -> SourceError {
    SourceError::OfflineVerification {
        path: path.into(),
        reason: reason.into(),
    }
}
fn offline_io(path: &Path, error: io::Error) -> SourceError {
    offline(&path.display().to_string(), &error.to_string())
}
