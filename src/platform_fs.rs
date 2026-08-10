//! Small filesystem portability boundary for private state and safe replacement.

use std::fs::File;
use std::fs::Metadata;
use std::fs::OpenOptions;
use std::io;
use std::path::Path;

pub(crate) fn configure_private_file(options: &mut OpenOptions) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(windows)]
    {
        let _ = options;
    }
}

pub(crate) fn configure_private_file_nofollow(options: &mut OpenOptions, nonblocking: bool) {
    configure_private_file(options);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut flags = libc::O_NOFOLLOW;
        if nonblocking {
            flags |= libc::O_NONBLOCK;
        }
        options.custom_flags(flags);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        let _ = nonblocking;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
}

pub(crate) fn create_private_directory_all(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true).mode(0o700).create(path)
    }
    #[cfg(windows)]
    {
        std::fs::create_dir_all(path)
    }
}

pub(crate) fn protect_file(file: &File) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
    }
    #[cfg(windows)]
    {
        let _ = file;
        Ok(())
    }
}

/// True for symbolic links and for Windows junctions/other reparse points.
pub(crate) fn is_link(metadata: &Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(unix)]
    {
        false
    }
}

pub(crate) fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()
    }
    #[cfg(windows)]
    {
        // The write-through replacement below provides the relevant durability
        // guarantee; FlushFileBuffers is not supported for directory handles.
        let _ = path;
        Ok(())
    }
}

pub(crate) fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::fs::rename(source, destination)
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::MOVEFILE_REPLACE_EXISTING;
        use windows_sys::Win32::Storage::FileSystem::MOVEFILE_WRITE_THROUGH;
        use windows_sys::Win32::Storage::FileSystem::MoveFileExW;

        let source = source
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let destination = destination
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let result = unsafe {
            MoveFileExW(
                source.as_ptr(),
                destination.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if result == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}
