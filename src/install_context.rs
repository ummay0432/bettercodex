use std::env;
use std::ffi::OsString;
use std::path::Path;

const PACKAGE_PATH_DIRNAME: &str = "bcodex-path";
const RG_EXECUTABLE_NAME: &str = if cfg!(windows) { "rg.exe" } else { "rg" };

/// Makes private package tools available to agent subprocesses without adding them to the user's
/// persistent PATH. This must run before the agent creates worker threads or a child supervisor.
pub(crate) fn prepare_runtime_path() {
    let current_executable = env::current_exe().ok();
    let Some(updated_path) =
        runtime_path_for_executable(current_executable.as_deref(), env::var_os("PATH"))
    else {
        return;
    };

    // SAFETY: run_agent_command calls this before the managed-session supervisor or Tokio runtime
    // can create another thread, matching upstream Codex's early package-PATH initialization.
    unsafe {
        env::set_var("PATH", updated_path);
    }
}

fn runtime_path_for_executable(
    executable: Option<&Path>,
    existing_path: Option<OsString>,
) -> Option<OsString> {
    let path_dir = executable?.parent()?.join(PACKAGE_PATH_DIRNAME);
    if !path_dir.join(RG_EXECUTABLE_NAME).is_file() {
        return None;
    }
    if existing_path
        .as_ref()
        .and_then(|value| env::split_paths(value).next())
        .is_some_and(|entry| entry == path_dir)
    {
        return None;
    }
    Some(path_env_with_entry(&path_dir, existing_path))
}

fn path_env_with_entry(path_entry: &Path, existing_path: Option<OsString>) -> OsString {
    #[cfg(unix)]
    const PATH_SEPARATOR: &str = ":";
    #[cfg(windows)]
    const PATH_SEPARATOR: &str = ";";

    let capacity = path_entry.as_os_str().len()
        + existing_path
            .as_ref()
            .map_or(0, |existing_path| 1 + existing_path.len());
    let mut path = OsString::with_capacity(capacity);
    path.push(path_entry);
    if let Some(existing_path) = existing_path {
        path.push(PATH_SEPARATOR);
        path.push(existing_path);
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn create() -> Self {
            let directory = env::temp_dir().join(format!(
                "bettercodex-install-context.{}",
                uuid::Uuid::new_v4()
            ));
            fs::create_dir(&directory).unwrap();
            Self(directory)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn package_path_is_absent_without_the_private_ripgrep() {
        let fixture = TestDirectory::create();
        let executable = fixture.0.join(if cfg!(windows) {
            "bcodex.exe"
        } else {
            "bcodex"
        });

        assert_eq!(
            runtime_path_for_executable(Some(&executable), Some(OsString::from("existing"))),
            None
        );
    }

    #[test]
    fn package_path_precedes_the_existing_path() {
        let fixture = TestDirectory::create();
        let path_dir = fixture.0.join(PACKAGE_PATH_DIRNAME);
        fs::create_dir(&path_dir).unwrap();
        fs::write(path_dir.join(RG_EXECUTABLE_NAME), b"fixture").unwrap();
        let executable = fixture.0.join(if cfg!(windows) {
            "bcodex.exe"
        } else {
            "bcodex"
        });
        let existing = fixture.0.join("existing");

        let updated =
            runtime_path_for_executable(Some(&executable), Some(existing.as_os_str().to_owned()))
                .unwrap();

        assert_eq!(
            env::split_paths(&updated).collect::<Vec<PathBuf>>(),
            vec![path_dir, existing]
        );
    }

    #[test]
    fn package_path_is_not_duplicated_when_already_first() {
        let fixture = TestDirectory::create();
        let path_dir = fixture.0.join(PACKAGE_PATH_DIRNAME);
        fs::create_dir(&path_dir).unwrap();
        fs::write(path_dir.join(RG_EXECUTABLE_NAME), b"fixture").unwrap();
        let executable = fixture.0.join(if cfg!(windows) {
            "bcodex.exe"
        } else {
            "bcodex"
        });

        assert_eq!(
            runtime_path_for_executable(Some(&executable), Some(path_dir.as_os_str().to_owned()),),
            None
        );
    }
}
