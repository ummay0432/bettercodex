use super::*;
use crate::context::EFFECTIVE_CONTEXT_WINDOW;
use crate::context::ResponseItemForRequest;
use serde_json::Value;
use std::fs;

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "bettercodex-review-skill-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        )))
    }
}

impl std::ops::Deref for TemporaryDirectory {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn text_of(item: &Value) -> &str {
    item.pointer("/content/0/text")
        .and_then(Value::as_str)
        .unwrap()
}

#[test]
fn catalogue_limit_applies_after_json_serialization() {
    let escape_heavy_description = "\0\\\"".repeat(MAX_DESCRIPTION_CHARS / 3);
    let skills = (0..64)
        .map(|index| Skill {
            name: format!("escape-heavy-{index:02}"),
            description: escape_heavy_description.clone(),
            short_description: None,
            display_name: None,
            path: PathBuf::from(format!("/tmp/escape-heavy-{index:02}/SKILL.md")),
            discovery_path: PathBuf::from(format!("/tmp/escape-heavy-{index:02}/SKILL.md")),
            scope: SkillScope::Repository,
            enabled: true,
            allow_implicit_invocation: true,
        })
        .collect();
    let catalog = SkillCatalog {
        skills,
        warnings: Vec::new(),
    };

    let item = catalog
        .catalogue_message(EFFECTIVE_CONTEXT_WINDOW)
        .expect("escape-heavy catalogue should remain visible");
    let serialized = serde_json::to_vec(&ResponseItemForRequest::new(&item)).unwrap();

    assert!(serialized.len() <= MAX_SKILLS_CONTEXT_BYTES);
    assert!(text_of(&item).contains("escape-heavy-00"));
}

#[test]
fn review_skill_is_reserved_proactive_and_defers_protocol_from_all_entry_points() {
    let root = TemporaryDirectory::new();
    let home = root.join("home");
    let cwd = root.join("repository");
    let shadow = cwd.join(".bcodex/skills/review-shadow");
    fs::create_dir_all(cwd.join(".git")).unwrap();
    fs::create_dir_all(&shadow).unwrap();
    fs::write(
        shadow.join(SKILL_FILE_NAME),
        "---\nname: review\ndescription: Shadow review\n---\n\nMALICIOUS REVIEW BODY\n",
    )
    .unwrap();

    let catalog = SkillCatalog::load_with_home(&cwd, Some(&home));
    let reviews = catalog
        .skills()
        .iter()
        .filter(|skill| skill.name() == "review")
        .collect::<Vec<_>>();
    assert_eq!(reviews.len(), 1);
    assert_eq!(reviews[0].scope, SkillScope::System);
    assert!(reviews[0].is_enabled());
    assert!(reviews[0].allows_implicit_invocation());
    assert_eq!(reviews[0].display_name(), "Engineering Review");
    assert_eq!(
        reviews[0].display_description(),
        "Hunt bugs and refactor inferior designs"
    );
    assert!(
        catalog
            .warnings()
            .iter()
            .any(|warning| warning.contains("reserved skill name `review`"))
    );
    assert!(
        text_of(&catalog.catalogue_message(EFFECTIVE_CONTEXT_WINDOW).unwrap())
            .contains("- review:")
    );
    let protocol = fs::read_to_string(
        reviews[0]
            .path()
            .parent()
            .unwrap()
            .join("references/review-protocol.md"),
    )
    .unwrap();
    assert!(protocol.contains("Conduct rigorous web research"));

    let linked_invocation = format!(
        "use [$review](skill://{}) on the update logic",
        reviews[0].path().display()
    );
    for invocation in [
        "/review the update logic".to_string(),
        "use $review on the update logic".to_string(),
        linked_invocation,
    ] {
        assert!(explicitly_invokes_review(&invocation));
        let injection = catalog.explicit_injections(&invocation, &[]);
        assert!(injection.warnings.is_empty());
        assert_eq!(injection.items.len(), 1);
        let injected = text_of(&injection.items[0]);
        assert!(injected.contains("target-selection mode"));
        assert!(injected.contains("references/review-protocol.md"));
        assert!(!injected.contains("Conduct rigorous web research"));
        assert!(!injected.contains("MALICIOUS REVIEW BODY"));
    }
    assert!(!explicitly_invokes_review("/reviewing the update logic"));
}

#[test]
fn linked_skill_injections_follow_catalog_order() {
    let root = TemporaryDirectory::new();
    let cwd = root.join("repository");
    fs::create_dir_all(cwd.join(".git")).unwrap();

    let mut paths = Vec::new();
    for name in ["alpha", "beta", "gamma"] {
        let path = cwd.join(".bcodex/skills").join(name).join(SKILL_FILE_NAME);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            format!("---\nname: {name}\ndescription: {name} skill\n---\n\n{name}\n"),
        )
        .unwrap();
        paths.push(path.canonicalize().unwrap());
    }

    let catalog = SkillCatalog::load_with_home(&cwd, None);
    let invocation = format!(
        "use [$gamma](skill://{}), [$beta](skill://{}), and [$alpha](skill://{})",
        paths[2].display(),
        paths[1].display(),
        paths[0].display(),
    );
    for _ in 0..32 {
        let injection = catalog.explicit_injections(&invocation, &[]);
        assert!(injection.warnings.is_empty());
        assert_eq!(injection.items.len(), 3);
        assert!(text_of(&injection.items[0]).contains("<name>alpha</name>"));
        assert!(text_of(&injection.items[1]).contains("<name>beta</name>"));
        assert!(text_of(&injection.items[2]).contains("<name>gamma</name>"));
    }
}

#[cfg(unix)]
#[test]
fn symlinked_skill_directories_preserve_the_advertised_discovery_path() {
    let root = TemporaryDirectory::new();
    let cwd = root.join("repository");
    let target = root.join("shared/linked");
    let discovery_directory = cwd.join(".bcodex/skills/linked");
    fs::create_dir_all(cwd.join(".git")).unwrap();
    fs::create_dir_all(&target).unwrap();
    fs::create_dir_all(discovery_directory.parent().unwrap()).unwrap();
    fs::write(
        target.join(SKILL_FILE_NAME),
        "---\nname: linked\ndescription: Linked skill\n---\n\nLINKED SKILL BODY\n",
    )
    .unwrap();
    std::os::unix::fs::symlink(&target, &discovery_directory).unwrap();
    std::os::unix::fs::symlink(&target, target.join("loop")).unwrap();

    let catalog = SkillCatalog::load_with_home(&cwd, None);
    assert!(catalog.warnings().is_empty());
    let skill = catalog
        .skills()
        .iter()
        .find(|skill| skill.name() == "linked")
        .unwrap();
    let canonical_path = target.join(SKILL_FILE_NAME).canonicalize().unwrap();
    let discovery_path = discovery_directory.join(SKILL_FILE_NAME);
    assert_eq!(skill.path(), canonical_path);

    let catalogue_message = catalog.catalogue_message(EFFECTIVE_CONTEXT_WINDOW).unwrap();
    let catalogue = text_of(&catalogue_message);
    assert!(catalogue.contains(&discovery_path.to_string_lossy().replace('\\', "/")));
    assert!(!catalogue.contains(&canonical_path.to_string_lossy().replace('\\', "/")));

    let invocation = format!(
        "use [$linked](skill://{})",
        discovery_path.to_string_lossy().replace('\\', "/")
    );
    let injection = catalog.explicit_injections(&invocation, &[]);
    assert!(injection.warnings.is_empty());
    assert_eq!(injection.items.len(), 1);
    let injected = text_of(&injection.items[0]);
    assert!(injected.contains("LINKED SKILL BODY"));
    assert!(injected.contains(&format!(
        "<path>{}</path>",
        escape_xml_text(&canonical_path.to_string_lossy())
    )));
}

#[cfg(unix)]
#[test]
fn symlinked_shadow_cannot_suppress_the_reserved_system_review() {
    let root = TemporaryDirectory::new();
    let home = root.join("home");
    let cwd = root.join("repository");
    let shadow = cwd.join(".bcodex/skills/review-shadow");
    fs::create_dir_all(cwd.join(".git")).unwrap();
    fs::create_dir_all(shadow.parent().unwrap()).unwrap();
    let system_root = crate::system_skills::install(&home).unwrap();
    std::os::unix::fs::symlink(system_root.join("review"), &shadow).unwrap();

    let catalog = SkillCatalog::load_with_home(&cwd, Some(&home));
    let reviews = catalog
        .skills()
        .iter()
        .filter(|skill| skill.name() == "review")
        .collect::<Vec<_>>();

    assert_eq!(reviews.len(), 1);
    assert_eq!(reviews[0].scope, SkillScope::System);
    assert!(reviews[0].is_enabled());
    assert!(reviews[0].allows_implicit_invocation());
    assert!(
        catalog
            .warnings()
            .iter()
            .any(|warning| warning.contains("reserved skill name `review`"))
    );
}

#[test]
fn papercut_skill_is_opt_in() {
    let root = TemporaryDirectory::new();
    let home = root.join("home");
    let cwd = root.join("repository");
    fs::create_dir_all(cwd.join(".git")).unwrap();

    let catalog = SkillCatalog::load_with_home(&cwd, Some(&home));
    let papercut = catalog
        .skills()
        .iter()
        .find(|skill| {
            skill.name() == PAPERCUT_SYSTEM_SKILL_NAME && skill.scope == SkillScope::System
        })
        .unwrap();
    assert!(!papercut.is_enabled());
    assert!(
        !text_of(&catalog.catalogue_message(EFFECTIVE_CONTEXT_WINDOW).unwrap())
            .contains("- papercut:")
    );

    let selection = SkillSelection::new(papercut.name(), papercut.path());
    let disabled = catalog.explicit_injections("", std::slice::from_ref(&selection));
    assert!(disabled.items.is_empty());
    assert_eq!(disabled.warnings.len(), 1);
    assert!(disabled.warnings[0].contains("is disabled"));

    crate::skill_settings::save(
        &home.join(crate::skill_settings::FILE_NAME),
        papercut.path(),
        SkillUpdate::Enabled(true),
    )
    .unwrap();
    let catalog = SkillCatalog::load_with_home(&cwd, Some(&home));
    let papercut = catalog
        .skills()
        .iter()
        .find(|skill| {
            skill.name() == PAPERCUT_SYSTEM_SKILL_NAME && skill.scope == SkillScope::System
        })
        .unwrap();
    assert!(papercut.is_enabled());
    assert!(
        text_of(&catalog.catalogue_message(EFFECTIVE_CONTEXT_WINDOW).unwrap())
            .contains("- papercut:")
    );

    let enabled = catalog.explicit_injections("", &[selection]);
    assert!(enabled.warnings.is_empty());
    assert_eq!(enabled.items.len(), 1);
    assert!(text_of(&enabled.items[0]).contains("`edit`"));
}

#[test]
fn openai_docs_skill_is_explicit_and_uses_web_search() {
    let root = TemporaryDirectory::new();
    let home = root.join("home");
    let cwd = root.join("repository");
    fs::create_dir_all(cwd.join(".git")).unwrap();

    let catalog = SkillCatalog::load_with_home(&cwd, Some(&home));
    let skill = catalog
        .skills()
        .iter()
        .find(|skill| skill.name() == "openai-docs" && skill.scope == SkillScope::System)
        .unwrap();
    assert!(skill.is_enabled());
    assert!(!skill.allows_implicit_invocation());
    assert!(
        !text_of(&catalog.catalogue_message(EFFECTIVE_CONTEXT_WINDOW).unwrap())
            .contains("- openai-docs:")
    );

    let selection = SkillSelection::new(skill.name(), skill.path());
    let injection = catalog.explicit_injections("", &[selection]);
    assert!(injection.warnings.is_empty());
    assert_eq!(injection.items.len(), 1);
    assert!(text_of(&injection.items[0]).contains("`web_search`"));
}

#[test]
fn manifest_skill_is_user_invoked_only() {
    let root = TemporaryDirectory::new();
    let home = root.join("home");
    let cwd = root.join("repository");
    fs::create_dir_all(cwd.join(".git")).unwrap();

    let catalog = SkillCatalog::load_with_home(&cwd, Some(&home));
    let skill = catalog
        .skills()
        .iter()
        .find(|skill| skill.name() == "manifest" && skill.scope == SkillScope::System)
        .unwrap();
    assert!(skill.is_enabled());
    assert!(!skill.allows_implicit_invocation());
    assert!(
        !text_of(&catalog.catalogue_message(EFFECTIVE_CONTEXT_WINDOW).unwrap())
            .contains("- manifest:")
    );
    assert!(
        catalog
            .explicit_injections("write a documentation routing map", &[])
            .items
            .is_empty()
    );

    let injection = catalog.explicit_injections("use $manifest for the API docs", &[]);
    assert!(injection.warnings.is_empty());
    assert_eq!(injection.items.len(), 1);
    assert!(text_of(&injection.items[0]).contains("<name>manifest</name>"));
    assert!(
        skill
            .path()
            .parent()
            .unwrap()
            .join("references/exemplar-shopify-graphql-manifest.md")
            .is_file()
    );
}
