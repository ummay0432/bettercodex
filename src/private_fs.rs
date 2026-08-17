//! Small filesystem portability boundary for private state and descriptor-anchored replacement.

use serde::Deserialize;
use serde::Serialize;
use std::ffi::CStr;
use std::ffi::CString;
use std::ffi::OsStr;
use std::fs::File;
use std::fs::Metadata;
use std::fs::OpenOptions;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Deserialize, Hash, PartialEq, Eq, Serialize)]
pub(crate) struct FileObjectIdentity {
    device: u64,
    inode: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct FileSnapshot {
    object: FileObjectIdentity,
    bytes: u64,
    mode: u32,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl FileSnapshot {
    pub(crate) fn object_identity(self) -> FileObjectIdentity {
        self.object
    }

    pub(crate) fn byte_len(self) -> u64 {
        self.bytes
    }

    pub(crate) fn ordinary_mode(self) -> u32 {
        self.mode & 0o777
    }

    /// Compare metadata that should survive linking or renaming this inode unchanged.
    ///
    /// Status-change time is intentionally excluded because creating or removing a hard link and
    /// renaming a directory entry may update it without changing the file's contents or mode.
    pub(crate) fn same_content_state(self, other: Self) -> bool {
        self.object == other.object
            && self.bytes == other.bytes
            && self.mode == other.mode
            && self.modified_seconds == other.modified_seconds
            && self.modified_nanoseconds == other.modified_nanoseconds
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FileEntryMetadata {
    snapshot: FileSnapshot,
}

impl FileEntryMetadata {
    pub(crate) fn snapshot(self) -> FileSnapshot {
        self.snapshot
    }

    pub(crate) fn object_identity(self) -> FileObjectIdentity {
        self.snapshot.object_identity()
    }

    pub(crate) fn is_file(self) -> bool {
        self.snapshot.mode & mode_bits(libc::S_IFMT) == mode_bits(libc::S_IFREG)
    }

    pub(crate) fn is_directory(self) -> bool {
        self.snapshot.mode & mode_bits(libc::S_IFMT) == mode_bits(libc::S_IFDIR)
    }
}

// `mode_t` is `u32` on Linux and `u16` on Apple targets.
#[allow(clippy::unnecessary_cast)]
fn mode_bits(mode: libc::mode_t) -> u32 {
    mode as u32
}

pub(crate) fn file_object_identity(metadata: &Metadata) -> FileObjectIdentity {
    FileObjectIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

pub(crate) fn file_snapshot(metadata: &Metadata) -> FileSnapshot {
    FileSnapshot {
        object: file_object_identity(metadata),
        bytes: metadata.len(),
        mode: metadata.mode(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
    }
}

/// A final path component anchored to an open descriptor for its resolved parent directory.
///
/// The descriptor prevents staging, replacement, and cleanup from being redirected if a parent
/// pathname is rebound. `parent_path_is_current` is still only a best-effort TOCTOU check: POSIX
/// rename has no compare-and-replace primitive for an expected inode.
pub(crate) struct AnchoredPath {
    directory: DirectoryHandle,
    parent_path: PathBuf,
    path: PathBuf,
    name: CString,
    parent_identity: FileObjectIdentity,
}

impl AnchoredPath {
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        let (parent_path, name) = split_parent_and_name(path)?;
        let directory = DirectoryHandle::open(&parent_path)?;
        Self::from_directory(path, parent_path, name, directory)
    }

    pub(crate) fn create_parent_directories(path: &Path) -> io::Result<Self> {
        let (parent_path, name) = split_parent_and_name(path)?;
        let start = if parent_path.is_absolute() {
            Path::new("/")
        } else {
            Path::new(".")
        };
        let mut directory = DirectoryHandle::open(start)?;
        for component in parent_path.components() {
            match component {
                Component::RootDir | Component::CurDir => {}
                Component::ParentDir => {
                    directory = directory.open_parent_directory()?;
                }
                Component::Normal(component) => {
                    directory = match directory.open_child_directory_following(component) {
                        Ok(directory) => directory,
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {
                            match directory.create_directory(component, 0o777) {
                                Ok(()) => {
                                    let created = directory
                                        .entry_metadata(component)?
                                        .filter(|metadata| metadata.is_directory())
                                        .ok_or_else(|| {
                                            io::Error::other(
                                                "new parent path component is not a directory",
                                            )
                                        })?;
                                    let opened = directory.open_child_directory(component)?;
                                    if opened.identity()? != created.object_identity() {
                                        return Err(io::Error::other(
                                            "new parent path component changed while it was opened",
                                        ));
                                    }
                                    opened
                                }
                                // Match `create_dir_all`: another writer may create the component
                                // after our failed open. Existing directory symlinks remain valid
                                // parent routes, so reopen with the ordinary following behavior.
                                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                                    directory.open_child_directory_following(component)?
                                }
                                Err(error) => return Err(error),
                            }
                        }
                        Err(error) => return Err(error),
                    };
                }
                Component::Prefix(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "unsupported filesystem path prefix",
                    ));
                }
            }
        }
        Self::from_directory(path, parent_path, name, directory)
    }

    fn from_directory(
        path: &Path,
        parent_path: PathBuf,
        name: &OsStr,
        directory: DirectoryHandle,
    ) -> io::Result<Self> {
        let metadata = directory.file.metadata()?;
        if !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                format!("`{}` is not a directory", parent_path.display()),
            ));
        }
        Ok(Self {
            parent_identity: file_object_identity(&metadata),
            directory,
            parent_path,
            path: path.to_path_buf(),
            name: component_to_cstring(name)?,
        })
    }

    pub(crate) fn parent_identity(&self) -> FileObjectIdentity {
        self.parent_identity
    }

    pub(crate) fn parent_path_is_current(&self) -> io::Result<bool> {
        let metadata = std::fs::metadata(&self.parent_path)?;
        Ok(metadata.is_dir() && file_object_identity(&metadata) == self.parent_identity)
    }

    pub(crate) fn entry_metadata(&self) -> io::Result<Option<FileEntryMetadata>> {
        self.directory.entry_metadata_cstr(&self.name)
    }

    pub(crate) fn open_for_read(&self) -> io::Result<File> {
        self.directory.open_file(
            &self.name,
            libc::O_RDONLY | libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0,
        )
    }

    pub(crate) fn create_directory(&self, name: &OsStr, mode: u32) -> io::Result<()> {
        self.directory.create_directory(name, mode)
    }

    pub(crate) fn open_child_directory(&self, name: &OsStr) -> io::Result<DirectoryHandle> {
        self.directory.open_child_directory(name)
    }

    pub(crate) fn child_metadata(&self, name: &OsStr) -> io::Result<Option<FileEntryMetadata>> {
        self.directory.entry_metadata(name)
    }

    pub(crate) fn remove_directory(&self, name: &OsStr) -> io::Result<()> {
        self.directory.remove_directory(name)
    }

    pub(crate) fn link_from(
        &self,
        source: &DirectoryHandle,
        source_name: &OsStr,
    ) -> io::Result<()> {
        let source_name = component_to_cstring(source_name)?;
        // SAFETY: both descriptors remain open for the call and both names are valid C strings.
        let result = unsafe {
            libc::linkat(
                source.file.as_raw_fd(),
                source_name.as_ptr(),
                self.directory.file.as_raw_fd(),
                self.name.as_ptr(),
                0,
            )
        };
        cvt_unit(result)
    }

    pub(crate) fn rename_from(
        &self,
        source: &DirectoryHandle,
        source_name: &OsStr,
    ) -> io::Result<()> {
        let source_name = component_to_cstring(source_name)?;
        // SAFETY: both descriptors remain open for the call and both names are valid C strings.
        let result = unsafe {
            libc::renameat(
                source.file.as_raw_fd(),
                source_name.as_ptr(),
                self.directory.file.as_raw_fd(),
                self.name.as_ptr(),
            )
        };
        cvt_unit(result)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn parent_path(&self) -> &Path {
        &self.parent_path
    }
}

pub(crate) struct DirectoryHandle {
    file: File,
}

impl DirectoryHandle {
    pub(crate) fn metadata(&self) -> io::Result<Metadata> {
        self.file.metadata()
    }

    pub(crate) fn identity(&self) -> io::Result<FileObjectIdentity> {
        self.metadata()
            .map(|metadata| file_object_identity(&metadata))
    }

    pub(crate) fn entry_metadata(&self, name: &OsStr) -> io::Result<Option<FileEntryMetadata>> {
        let name = component_to_cstring(name)?;
        self.entry_metadata_cstr(&name)
    }

    fn open(path: &Path) -> io::Result<Self> {
        let path = path_to_cstring(path)?;
        // SAFETY: `path` is a valid C string and the returned descriptor is uniquely owned.
        let descriptor = unsafe { libc::open(path.as_ptr(), directory_open_flags(false)) };
        file_from_descriptor(descriptor).map(|file| Self { file })
    }

    fn open_child_directory(&self, name: &OsStr) -> io::Result<Self> {
        self.open_child_directory_with_flags(name, true)
    }

    fn open_child_directory_following(&self, name: &OsStr) -> io::Result<Self> {
        self.open_child_directory_with_flags(name, false)
    }

    fn open_parent_directory(&self) -> io::Result<Self> {
        let name = c"..";
        // SAFETY: the directory descriptor and static C string remain valid for the call.
        let descriptor = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                directory_open_flags(false),
            )
        };
        file_from_descriptor(descriptor).map(|file| Self { file })
    }

    fn open_child_directory_with_flags(&self, name: &OsStr, nofollow: bool) -> io::Result<Self> {
        let name = component_to_cstring(name)?;
        // SAFETY: the directory descriptor and C string remain valid for the call.
        let descriptor = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                directory_open_flags(nofollow),
            )
        };
        file_from_descriptor(descriptor).map(|file| Self { file })
    }

    pub(crate) fn create_file(&self, name: &OsStr, mode: u32) -> io::Result<File> {
        let name = component_to_cstring(name)?;
        self.open_file(
            &name,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            mode,
        )
    }

    pub(crate) fn remove_file(&self, name: &OsStr) -> io::Result<()> {
        let name = component_to_cstring(name)?;
        // SAFETY: the directory descriptor and C string remain valid for the call.
        let result = unsafe { libc::unlinkat(self.file.as_raw_fd(), name.as_ptr(), 0) };
        cvt_unit(result)
    }

    fn entry_metadata_cstr(&self, name: &CStr) -> io::Result<Option<FileEntryMetadata>> {
        let mut raw = MaybeUninit::<libc::stat>::uninit();
        // SAFETY: `raw` points to writable storage and the descriptor and C string remain valid.
        let result = unsafe {
            libc::fstatat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                raw.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result == -1 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::NotFound {
                return Ok(None);
            }
            return Err(error);
        }
        // SAFETY: a successful `fstatat` initialized `raw`.
        let raw = unsafe { raw.assume_init() };
        Ok(Some(FileEntryMetadata {
            snapshot: snapshot_from_stat(&raw),
        }))
    }

    fn open_file(&self, name: &CStr, flags: libc::c_int, mode: u32) -> io::Result<File> {
        // C variadics promote Apple's 16-bit `mode_t` to `int`.
        #[cfg(target_vendor = "apple")]
        let mode = mode as libc::c_int;
        #[cfg(not(target_vendor = "apple"))]
        let mode = mode as libc::mode_t;

        // SAFETY: the directory descriptor and C string remain valid, and the returned descriptor
        // is uniquely transferred into `File`.
        let descriptor = unsafe { libc::openat(self.file.as_raw_fd(), name.as_ptr(), flags, mode) };
        file_from_descriptor(descriptor)
    }

    fn create_directory(&self, name: &OsStr, mode: u32) -> io::Result<()> {
        let name = component_to_cstring(name)?;
        // SAFETY: the directory descriptor and C string remain valid for the call.
        let result =
            unsafe { libc::mkdirat(self.file.as_raw_fd(), name.as_ptr(), mode as libc::mode_t) };
        cvt_unit(result)
    }

    fn remove_directory(&self, name: &OsStr) -> io::Result<()> {
        let name = component_to_cstring(name)?;
        // SAFETY: the directory descriptor and C string remain valid for the call.
        let result =
            unsafe { libc::unlinkat(self.file.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) };
        cvt_unit(result)
    }
}

// libc exposes these fields with different integer aliases on Linux and Apple targets.
#[allow(clippy::unnecessary_cast)]
fn snapshot_from_stat(metadata: &libc::stat) -> FileSnapshot {
    FileSnapshot {
        object: FileObjectIdentity {
            device: metadata.st_dev as u64,
            inode: metadata.st_ino as u64,
        },
        bytes: u64::try_from(metadata.st_size).unwrap_or(0),
        mode: metadata.st_mode as u32,
        modified_seconds: metadata.st_mtime as i64,
        modified_nanoseconds: metadata.st_mtime_nsec as i64,
        changed_seconds: metadata.st_ctime as i64,
        changed_nanoseconds: metadata.st_ctime_nsec as i64,
    }
}

fn split_parent_and_name(path: &Path) -> io::Result<(PathBuf, &OsStr)> {
    let bytes = path.as_os_str().as_bytes();
    let final_bytes = bytes
        .rsplit(|byte| *byte == b'/')
        .next()
        .unwrap_or_default();
    if final_bytes.is_empty() || final_bytes == b"." || final_bytes == b".." {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path `{}` has no file final component", path.display()),
        ));
    }
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path `{}` has no parent directory", path.display()),
        )
    })?;
    let parent = if parent.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        parent.to_path_buf()
    };
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path `{}` has no final component", path.display()),
        )
    })?;
    Ok((parent, name))
}

fn component_to_cstring(component: &OsStr) -> io::Result<CString> {
    if component.as_bytes().contains(&b'/')
        || component == OsStr::new(".")
        || component == OsStr::new("..")
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid filesystem component",
        ));
    }
    CString::new(component.as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "filesystem path contains a NUL byte",
        )
    })
}

fn path_to_cstring(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "filesystem path contains a NUL byte",
        )
    })
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn directory_open_flags(nofollow: bool) -> libc::c_int {
    libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | if nofollow { libc::O_NOFOLLOW } else { 0 }
}

#[cfg(target_vendor = "apple")]
fn directory_open_flags(nofollow: bool) -> libc::c_int {
    libc::O_SEARCH | libc::O_CLOEXEC | if nofollow { libc::O_NOFOLLOW } else { 0 }
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
fn directory_open_flags(nofollow: bool) -> libc::c_int {
    libc::O_RDONLY
        | libc::O_DIRECTORY
        | libc::O_CLOEXEC
        | if nofollow { libc::O_NOFOLLOW } else { 0 }
}

fn file_from_descriptor(descriptor: libc::c_int) -> io::Result<File> {
    if descriptor == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful `open` or `openat` returns a fresh descriptor owned by this function.
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

fn cvt_unit(result: libc::c_int) -> io::Result<()> {
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub(crate) fn configure_private_file(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
}

pub(crate) fn configure_private_file_nofollow(options: &mut OpenOptions, nonblocking: bool) {
    configure_private_file(options);
    use std::os::unix::fs::OpenOptionsExt;
    let mut flags = libc::O_NOFOLLOW;
    if nonblocking {
        flags |= libc::O_NONBLOCK;
    }
    options.custom_flags(flags);
}

pub(crate) fn create_private_directory_all(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true).mode(0o700).create(path)
}

pub(crate) fn protect_file(file: &File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
}

/// True for symbolic links.
pub(crate) fn is_link(metadata: &Metadata) -> bool {
    metadata.file_type().is_symlink()
}

pub(crate) fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

pub(crate) fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    std::fs::rename(source, destination)
}
