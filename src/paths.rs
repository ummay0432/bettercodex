//! Locations for bettercodex-owned operator state.

use std::path::PathBuf;

/// Returns the current user's home directory on supported Unix targets.
///
/// This preserves `dirs::home_dir`'s `$HOME` precedence and `getpwuid_r`
/// fallback without compiling the crate's unrelated platform-directory APIs.
pub(crate) fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .or_else(passwd_home_dir)
}

#[cfg(unix)]
fn passwd_home_dir() -> Option<PathBuf> {
    use std::ffi::CStr;
    use std::ffi::OsString;
    use std::mem::MaybeUninit;
    use std::os::unix::ffi::OsStringExt;

    let suggested_buffer_len = unsafe { libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) };
    let buffer_len = usize::try_from(suggested_buffer_len)
        .ok()
        .filter(|len| *len > 0)
        .unwrap_or(512);
    let mut buffer = vec![0_u8; buffer_len];
    let mut passwd = MaybeUninit::<libc::passwd>::uninit();

    loop {
        let mut result = std::ptr::null_mut();
        let status = unsafe {
            libc::getpwuid_r(
                libc::getuid(),
                passwd.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if status == 0 {
            if result.is_null() {
                return None;
            }
            let passwd = unsafe { passwd.assume_init_ref() };
            if passwd.pw_dir.is_null() {
                return None;
            }
            let bytes = unsafe { CStr::from_ptr(passwd.pw_dir) }.to_bytes();
            return (!bytes.is_empty()).then(|| PathBuf::from(OsString::from_vec(bytes.to_vec())));
        }
        if status != libc::ERANGE {
            return None;
        }
        let new_len = buffer.len().checked_mul(2)?;
        if new_len > 1024 * 1024 {
            return None;
        }
        buffer.resize(new_len, 0);
    }
}

#[cfg(not(unix))]
fn passwd_home_dir() -> Option<PathBuf> {
    None
}

pub(crate) fn bettercodex_home() -> Option<PathBuf> {
    std::env::var_os("BCODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".bcodex")))
}
