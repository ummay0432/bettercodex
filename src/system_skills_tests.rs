use super::*;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::symlink;
use uuid::Uuid;

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "bettercodex-{label}-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn embedded_system_skills_are_exact_private_and_idempotent() {
    let test_directory = TestDirectory::new("system-skills");
    let home = test_directory.0.join("home");

    let installed_root = install(&home).unwrap();
    assert_eq!(installed_root, root(&home));
    assert_eq!(
        std::fs::metadata(&installed_root)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    for embedded in EMBEDDED_FILES {
        let path = installed_root.join(embedded.relative_path);
        assert_eq!(std::fs::read(&path).unwrap(), embedded.contents);
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    assert_eq!(install(&home).unwrap(), installed_root);
}

#[test]
fn current_marker_does_not_hide_modified_missing_or_retired_files() {
    let test_directory = TestDirectory::new("system-skills-drift");
    let home = test_directory.0.join("home");
    let installed_root = install(&home).unwrap();
    let modified = installed_root.join(EMBEDDED_FILES[0].relative_path);
    let missing = installed_root.join(EMBEDDED_FILES[1].relative_path);
    let retired = installed_root.join("retired/SKILL.md");

    std::fs::write(&modified, "locally drifted contents").unwrap();
    std::fs::set_permissions(&modified, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::remove_file(&missing).unwrap();
    std::fs::create_dir_all(retired.parent().unwrap()).unwrap();
    std::fs::write(&retired, "retired embedded skill").unwrap();

    assert_eq!(install(&home).unwrap(), installed_root);
    assert_eq!(
        std::fs::read(&modified).unwrap(),
        EMBEDDED_FILES[0].contents
    );
    assert_eq!(
        std::fs::metadata(&modified)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::read(&missing).unwrap(),
        EMBEDDED_FILES[1].contents
    );
    assert!(!retired.exists());
}

#[test]
fn interrupted_directory_swap_is_recovered_before_verification() {
    let test_directory = TestDirectory::new("system-skills-recovery");
    let home = test_directory.0.join("home");
    let installed_root = install(&home).unwrap();
    let skills_root = home.join(SKILLS_DIRECTORY);
    let backup = skills_root.join(BACKUP_DIRECTORY_NAME);
    let staging = skills_root.join(STAGING_DIRECTORY_NAME);

    std::fs::rename(&installed_root, &backup).unwrap();
    std::fs::create_dir(&staging).unwrap();
    std::fs::write(staging.join("partial"), "incomplete replacement").unwrap();

    assert_eq!(install(&home).unwrap(), installed_root);
    assert!(!backup.exists());
    assert!(!staging.exists());
    assert_eq!(
        std::fs::read(installed_root.join(EMBEDDED_FILES[0].relative_path)).unwrap(),
        EMBEDDED_FILES[0].contents
    );
}

#[test]
fn system_skill_installation_refuses_to_replace_a_symlink() {
    let test_directory = TestDirectory::new("system-skills-symlink");
    let home = test_directory.0.join("home");
    let outside = test_directory.0.join("outside");
    std::fs::create_dir_all(home.join(SKILLS_DIRECTORY)).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("keep"), "untouched").unwrap();
    symlink(&outside, root(&home)).unwrap();

    let error = install(&home).unwrap_err();

    assert!(error.to_string().contains("not a regular directory"));
    assert_eq!(
        std::fs::read_to_string(outside.join("keep")).unwrap(),
        "untouched"
    );
}
