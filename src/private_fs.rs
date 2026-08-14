//! Small filesystem portability boundary for private state and safe replacement.

use std::fs::File;
use std::fs::Metadata;
use std::fs::OpenOptions;
use std::io;
use std::path::Path;

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
