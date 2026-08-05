use super::*;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::symlink;
use uuid::Uuid;

#[test]
fn embedded_system_skill_is_materialized_privately_and_idempotently() {
    let home = std::env::temp_dir().join(format!(
        "bettercodex-system-skills-{}-{}",
        std::process::id(),
        Uuid::new_v4()
    ));

    let installed_root = install(&home).unwrap();
    assert_eq!(installed_root, root(&home));
    let expected: [(&str, &[u8]); 2] = [
        (
            "papercut/SKILL.md",
            include_bytes!("../bundled-skills/papercut/SKILL.md"),
        ),
        (
            "papercut/agents/openai.yaml",
            include_bytes!("../bundled-skills/papercut/agents/openai.yaml"),
        ),
    ];
    for (relative_path, contents) in expected {
        let path = installed_root.join(relative_path);
        assert_eq!(std::fs::read(&path).unwrap(), contents);
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    assert_eq!(install(&home).unwrap(), installed_root);
    let retired_skill = installed_root.join("retired/SKILL.md");
    std::fs::create_dir_all(retired_skill.parent().unwrap()).unwrap();
    std::fs::write(&retired_skill, "old bundled content").unwrap();
    std::fs::write(installed_root.join(MARKER_FILE_NAME), "stale fingerprint\n").unwrap();
    assert_eq!(install(&home).unwrap(), installed_root);
    assert!(!retired_skill.exists());

    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn system_skill_installation_refuses_to_replace_a_symlink() {
    let root = std::env::temp_dir().join(format!(
        "bettercodex-system-skills-symlink-{}-{}",
        std::process::id(),
        Uuid::new_v4()
    ));
    let home = root.join("home");
    let outside = root.join("outside");
    std::fs::create_dir_all(home.join("skills")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("keep"), "untouched").unwrap();
    symlink(&outside, root.join("home/skills/.system")).unwrap();

    let error = install(&home).unwrap_err();

    assert!(error.to_string().contains("not a regular directory"));
    assert_eq!(
        std::fs::read_to_string(outside.join("keep")).unwrap(),
        "untouched"
    );
    std::fs::remove_dir_all(root).unwrap();
}
