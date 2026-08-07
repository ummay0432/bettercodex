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
    let expected: [(&str, &[u8]); 22] = [
        (
            "manifest/SKILL.md",
            include_bytes!("../bundled-skills/manifest/SKILL.md"),
        ),
        (
            "manifest/agents/openai.yaml",
            include_bytes!("../bundled-skills/manifest/agents/openai.yaml"),
        ),
        (
            "manifest/references/exemplar-shopify-graphql-manifest.md",
            include_bytes!(
                "../bundled-skills/manifest/references/exemplar-shopify-graphql-manifest.md"
            ),
        ),
        (
            "openai-docs/LICENSE.txt",
            include_bytes!("../bundled-skills/openai-docs/LICENSE.txt"),
        ),
        (
            "openai-docs/SKILL.md",
            include_bytes!("../bundled-skills/openai-docs/SKILL.md"),
        ),
        (
            "openai-docs/agents/openai.yaml",
            include_bytes!("../bundled-skills/openai-docs/agents/openai.yaml"),
        ),
        (
            "openai-docs/assets/openai-small.svg",
            include_bytes!("../bundled-skills/openai-docs/assets/openai-small.svg"),
        ),
        (
            "openai-docs/assets/openai.png",
            include_bytes!("../bundled-skills/openai-docs/assets/openai.png"),
        ),
        (
            "openai-docs/references/codex-self-knowledge.md",
            include_bytes!("../bundled-skills/openai-docs/references/codex-self-knowledge.md"),
        ),
        (
            "openai-docs/references/latest-model.md",
            include_bytes!("../bundled-skills/openai-docs/references/latest-model.md"),
        ),
        (
            "openai-docs/references/mcp-diagnostics.md",
            include_bytes!("../bundled-skills/openai-docs/references/mcp-diagnostics.md"),
        ),
        (
            "openai-docs/references/model-migration.md",
            include_bytes!("../bundled-skills/openai-docs/references/model-migration.md"),
        ),
        (
            "openai-docs/references/model-selection.md",
            include_bytes!("../bundled-skills/openai-docs/references/model-selection.md"),
        ),
        (
            "openai-docs/references/official-docs.md",
            include_bytes!("../bundled-skills/openai-docs/references/official-docs.md"),
        ),
        (
            "openai-docs/references/prompting-guide.md",
            include_bytes!("../bundled-skills/openai-docs/references/prompting-guide.md"),
        ),
        (
            "openai-docs/references/upgrade-guide.md",
            include_bytes!("../bundled-skills/openai-docs/references/upgrade-guide.md"),
        ),
        (
            "openai-docs/references/upgrading-to-gpt-5p6-sol.md",
            include_bytes!("../bundled-skills/openai-docs/references/upgrading-to-gpt-5p6-sol.md"),
        ),
        (
            "openai-docs/scripts/fetch-codex-manual.mjs",
            include_bytes!("../bundled-skills/openai-docs/scripts/fetch-codex-manual.mjs"),
        ),
        (
            "openai-docs/scripts/resolve-latest-model-info",
            include_bytes!("../bundled-skills/openai-docs/scripts/resolve-latest-model-info"),
        ),
        (
            "openai-docs/scripts/resolve-latest-model-info.cjs",
            include_bytes!("../bundled-skills/openai-docs/scripts/resolve-latest-model-info.cjs"),
        ),
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
