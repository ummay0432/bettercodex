use super::*;
use crate::context::EFFECTIVE_CONTEXT_WINDOW;
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
fn review_skill_is_reserved_proactive_and_defers_protocol_from_both_entry_points() {
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

    for invocation in [
        "/review the update logic",
        "use $review on the update logic",
    ] {
        let injection = catalog.explicit_injections(invocation, &[]);
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
fn papercut_skill_and_tool_are_opt_in() {
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
    assert!(!catalog.tool_configuration().papercut_enabled());
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
    assert!(catalog.tool_configuration().papercut_enabled());
    assert!(
        text_of(&catalog.catalogue_message(EFFECTIVE_CONTEXT_WINDOW).unwrap())
            .contains("- papercut:")
    );

    let enabled = catalog.explicit_injections("", &[selection]);
    assert!(enabled.warnings.is_empty());
    assert_eq!(enabled.items.len(), 1);
    assert!(text_of(&enabled.items[0]).contains("tools.log_papercut"));
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
    assert!(text_of(&injection.items[0]).contains("tools.web__run"));
}
