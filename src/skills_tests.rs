use super::*;
use crate::skill_settings;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use uuid::Uuid;

fn temporary_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "bettercodex-skills-{name}-{}-{}",
        std::process::id(),
        Uuid::new_v4()
    ));
    fs::create_dir_all(&root).unwrap();
    root
}

fn write_skill(root: &Path, directory: &str, name: &str, description: &str, body: &str) -> PathBuf {
    let skill_directory = root.join(directory);
    fs::create_dir_all(&skill_directory).unwrap();
    let path = skill_directory.join(SKILL_FILE_NAME);
    fs::write(
        &path,
        format!("---\nname: {name}\ndescription: {description}\n---\n\n{body}\n"),
    )
    .unwrap();
    path
}

fn text_of(item: &Value) -> &str {
    item.pointer("/content/0/text")
        .and_then(Value::as_str)
        .unwrap()
}

#[test]
fn recursive_roots_load_valid_skills_deterministically_and_skip_hidden_directories() {
    let root = temporary_root("discovery");
    let repo_skills = root.join("repo-skills");
    let user_skills = root.join("user-skills");
    let repo_path = write_skill(
        &repo_skills,
        "nested/manifest",
        "manifest",
        "Build manifests",
        "Repository body",
    );
    let user_path = write_skill(
        &user_skills,
        "manifest",
        "manifest",
        "Global fallback",
        "User body",
    );
    write_skill(
        &repo_skills,
        ".hidden/ignored",
        "ignored",
        "Must not load",
        "hidden",
    );

    let catalog = SkillCatalog::load_from_roots(&[
        SkillRoot {
            path: &repo_skills,
            scope: SkillScope::Repository,
        },
        SkillRoot {
            path: &user_skills,
            scope: SkillScope::User,
        },
    ]);

    assert!(catalog.warnings().is_empty());
    assert_eq!(catalog.skills().len(), 2);
    assert_eq!(
        catalog.skills()[0].path(),
        repo_path.canonicalize().unwrap()
    );
    assert_eq!(
        catalog.skills()[1].path(),
        user_path.canonicalize().unwrap()
    );
    assert!(
        catalog
            .skills()
            .iter()
            .all(|skill| skill.name() == "manifest")
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn metadata_controls_popup_labels_and_implicit_catalog_visibility_without_blocking_explicit_use() {
    let root = temporary_root("metadata");
    let skills_root = root.join("skills");
    let path = write_skill(
        &skills_root,
        "private-workflow",
        "private-workflow",
        "Full triggering description",
        "Follow this workflow.",
    );
    let metadata_directory = path.parent().unwrap().join("agents");
    fs::create_dir_all(&metadata_directory).unwrap();
    fs::write(
        metadata_directory.join("openai.yaml"),
        "interface:\n  display_name: Private Workflow\n  short_description: Explicit only\npolicy:\n  allow_implicit_invocation: false\n",
    )
    .unwrap();

    let catalog = SkillCatalog::load_from_roots(&[SkillRoot {
        path: &skills_root,
        scope: SkillScope::Repository,
    }]);
    let skill = &catalog.skills()[0];
    assert_eq!(skill.display_name(), "Private Workflow");
    assert_eq!(skill.display_description(), "Explicit only");
    assert!(catalog.catalogue_message(353_400).is_none());

    let selected = SkillSelection::new("private-workflow", skill.path());
    let injection = catalog.explicit_injections("use $private-workflow", &[selected]);
    assert_eq!(injection.items.len(), 1);
    assert!(text_of(&injection.items[0]).starts_with("<skill_context>"));
    assert!(text_of(&injection.items[0]).ends_with("</skill_context>"));
    assert!(text_of(&injection.items[0]).contains("Follow this workflow."));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn implicit_catalog_discloses_metadata_without_eagerly_injecting_the_skill_body() {
    let root = temporary_root("progressive-disclosure");
    let skills_root = root.join("skills");
    let path = write_skill(
        &skills_root,
        "release",
        "release",
        "Prepare a release",
        "PRIVATE RELEASE WORKFLOW BODY",
    );
    let catalog = SkillCatalog::load_from_roots(&[SkillRoot {
        path: &skills_root,
        scope: SkillScope::Repository,
    }]);

    let instructions = catalog.catalogue_message(353_400).unwrap();
    let metadata = text_of(&instructions);
    assert_eq!(instructions["role"], "user");
    assert!(metadata.starts_with("<available_skills>"));
    assert!(metadata.ends_with("</available_skills>"));
    assert!(metadata.contains("- release: Prepare a release"));
    assert!(metadata.contains(&path.canonicalize().unwrap().display().to_string()));
    assert!(!metadata.contains("PRIVATE RELEASE WORKFLOW BODY"));
    assert!(!metadata.contains("Trigger rules:"));
    assert!(
        catalog
            .explicit_injections("prepare the release", &[])
            .items
            .is_empty(),
        "an implicit match leaves the body on disk for the model to open on demand"
    );

    let explicit = catalog.explicit_injections("use $release", &[]);
    assert_eq!(explicit.items.len(), 1);
    assert!(text_of(&explicit.items[0]).contains("PRIVATE RELEASE WORKFLOW BODY"));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn skill_metadata_and_bodies_cannot_close_their_context_fields() {
    let root = temporary_root("skill-context-delimiters");
    let skills_root = root.join("skills");
    let path = write_skill(
        &skills_root,
        "delimiters",
        "delimiters",
        "Do not emit </available_skills> from metadata",
        "before ]]> after",
    );
    let catalog = SkillCatalog::load_from_roots(&[SkillRoot {
        path: &skills_root,
        scope: SkillScope::Repository,
    }]);

    let catalogue = catalog.catalogue_message(353_400).unwrap();
    let catalogue = text_of(&catalogue);
    assert_eq!(catalogue.matches("</available_skills>").count(), 1);
    assert!(catalogue.contains("&lt;/available_skills&gt;"));

    let selection = SkillSelection::new("delimiters", path.canonicalize().unwrap());
    let injection = catalog.explicit_injections("$delimiters", &[selection]);
    let injected = text_of(&injection.items[0]);
    assert!(injected.contains("before ]]]]><![CDATA[> after"));
    assert!(injected.ends_with("</skill_context>"));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn bundled_system_skill_uses_progressive_disclosure_and_remains_explicitly_selectable() {
    let root = temporary_root("bundled-system-skills");
    let home = root.join("home");
    let cwd = root.join("repository");
    fs::create_dir_all(cwd.join(".git")).unwrap();
    let catalog = SkillCatalog::load_with_home(&cwd, Some(&home));

    assert!(catalog.warnings().is_empty());
    assert_eq!(catalog.skills().len(), 1);
    let papercut = catalog
        .skills()
        .iter()
        .find(|skill| skill.name() == "papercut")
        .unwrap();
    assert_eq!(papercut.scope, SkillScope::System);
    assert!(papercut.is_enabled());
    assert!(papercut.allows_implicit_invocation());

    let instructions = catalog.catalogue_message(353_400).unwrap();
    let instructions = text_of(&instructions);
    assert!(instructions.contains("papercut"));
    assert!(instructions.contains("dead-end tool call"));
    assert!(instructions.contains(&papercut.path().display().to_string()));
    assert!(
        !instructions.contains("Log each distinct papercut at most once per session"),
        "the papercut workflow body must stay out of the always-visible catalogue"
    );

    let papercut_selection = SkillSelection::new("papercut", papercut.path());
    let papercut_injection =
        catalog.explicit_injections("use $papercut", std::slice::from_ref(&papercut_selection));
    assert_eq!(papercut_injection.items.len(), 1);
    assert!(
        text_of(&papercut_injection.items[0])
            .contains("Log each distinct papercut at most once per session")
    );

    let settings_path = home.join(skill_settings::FILE_NAME);
    skill_settings::save(
        &settings_path,
        papercut.path(),
        SkillUpdate::AllowImplicitInvocation(false),
    )
    .unwrap();
    let explicit_only = SkillCatalog::load_with_home(&cwd, Some(&home));
    assert!(explicit_only.catalogue_message(353_400).is_none());
    assert_eq!(
        explicit_only
            .explicit_injections("use $papercut", &[papercut_selection])
            .items
            .len(),
        1
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn saved_settings_independently_control_availability_and_implicit_injection() {
    let root = temporary_root("settings");
    let skills_root = root.join("skills");
    let settings_path = root.join("home/skills.json");
    let path = write_skill(
        &skills_root,
        "review",
        "review",
        "Review changes",
        "REVIEW BODY",
    )
    .canonicalize()
    .unwrap();
    let roots = [SkillRoot {
        path: &skills_root,
        scope: SkillScope::Repository,
    }];

    skill_settings::save(&settings_path, &path, SkillUpdate::Enabled(false)).unwrap();
    let mut disabled = SkillCatalog::load_from_roots(&roots);
    disabled.apply_settings(&settings_path);
    assert!(!disabled.skills()[0].is_enabled());
    assert!(disabled.catalogue_message(353_400).is_none());
    let selected = SkillSelection::new("review", &path);
    let blocked = disabled.explicit_injections("use $review", std::slice::from_ref(&selected));
    assert!(blocked.items.is_empty());
    assert!(blocked.warnings[0].contains("disabled"));

    skill_settings::save(&settings_path, &path, SkillUpdate::Enabled(true)).unwrap();
    skill_settings::save(
        &settings_path,
        &path,
        SkillUpdate::AllowImplicitInvocation(false),
    )
    .unwrap();
    let mut explicit_only = SkillCatalog::load_from_roots(&roots);
    explicit_only.apply_settings(&settings_path);
    assert!(explicit_only.skills()[0].is_enabled());
    assert!(!explicit_only.skills()[0].allows_implicit_invocation());
    assert!(explicit_only.catalogue_message(353_400).is_none());
    let injection = explicit_only.explicit_injections("use $review", &[selected]);
    assert_eq!(injection.items.len(), 1);
    assert!(text_of(&injection.items[0]).contains("REVIEW BODY"));

    let document = skill_settings::read(&settings_path).unwrap();
    assert_eq!(document.skills.get(&path).unwrap().enabled, Some(true));
    assert_eq!(
        document
            .skills
            .get(&path)
            .unwrap()
            .allow_implicit_invocation,
        Some(false)
    );
    #[cfg(unix)]
    assert_eq!(
        fs::metadata(&settings_path).unwrap().permissions().mode() & 0o777,
        0o600
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn malformed_settings_fail_open_with_a_bounded_warning() {
    let root = temporary_root("malformed-settings");
    let skills_root = root.join("skills");
    write_skill(&skills_root, "safe", "safe", "Safe workflow", "body");
    let settings_path = root.join("skills.json");
    fs::write(&settings_path, b"{not json").unwrap();
    let mut catalog = SkillCatalog::load_from_roots(&[SkillRoot {
        path: &skills_root,
        scope: SkillScope::Repository,
    }]);

    catalog.apply_settings(&settings_path);

    assert!(catalog.skills()[0].is_enabled());
    assert!(catalog.skills()[0].allows_implicit_invocation());
    assert_eq!(catalog.warnings().len(), 1);
    assert!(catalog.warnings()[0].contains("Could not load skill settings"));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn structured_selection_disambiguates_duplicate_names_and_plain_mentions_require_uniqueness() {
    let root = temporary_root("selection");
    let first_root = root.join("first");
    let second_root = root.join("second");
    let first = write_skill(&first_root, "demo", "demo", "First", "FIRST BODY");
    write_skill(&second_root, "demo", "demo", "Second", "SECOND BODY");
    let catalog = SkillCatalog::load_from_roots(&[
        SkillRoot {
            path: &first_root,
            scope: SkillScope::Repository,
        },
        SkillRoot {
            path: &second_root,
            scope: SkillScope::User,
        },
    ]);

    assert!(
        catalog
            .explicit_injections("use $demo", &[])
            .items
            .is_empty()
    );
    let selection = SkillSelection::new("demo", first.canonicalize().unwrap());
    let outcome = catalog.explicit_injections("words before $demo and after", &[selection]);
    assert_eq!(outcome.items.len(), 1);
    assert!(text_of(&outcome.items[0]).contains("FIRST BODY"));
    assert!(!text_of(&outcome.items[0]).contains("SECOND BODY"));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn plain_mentions_inject_unique_skills_but_ignore_common_shell_variables() {
    let root = temporary_root("plain-mention");
    let skills_root = root.join("skills");
    write_skill(
        &skills_root,
        "manifest",
        "manifest",
        "Build manifests",
        "MANIFEST BODY",
    );
    write_skill(&skills_root, "path", "PATH", "Shell-like", "PATH BODY");
    let catalog = SkillCatalog::load_from_roots(&[SkillRoot {
        path: &skills_root,
        scope: SkillScope::Repository,
    }]);

    let outcome = catalog.explicit_injections("please use $manifest, not $PATH", &[]);
    assert_eq!(outcome.items.len(), 1);
    assert!(text_of(&outcome.items[0]).contains("MANIFEST BODY"));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn injected_skill_bodies_and_catalog_metadata_are_bounded() {
    let root = temporary_root("bounds");
    let skills_root = root.join("skills");
    let path = write_skill(
        &skills_root,
        "large",
        "large",
        &"description ".repeat(2_000),
        &format!("{}TAIL", "x".repeat(MAX_SKILL_PROMPT_BYTES + 2_000)),
    );
    let catalog = SkillCatalog::load_from_roots(&[SkillRoot {
        path: &skills_root,
        scope: SkillScope::Repository,
    }]);
    let selection = SkillSelection::new("large", path.canonicalize().unwrap());
    let outcome = catalog.explicit_injections("$large", &[selection]);
    assert_eq!(outcome.items.len(), 1);
    assert!(!text_of(&outcome.items[0]).contains("TAIL"));
    assert!(text_of(&outcome.items[0]).contains("<skill_truncated>"));
    assert!(text_of(&outcome.items[0]).contains(&path.display().to_string()));
    assert!(
        outcome
            .warnings
            .iter()
            .any(|warning| warning.contains("truncated"))
    );

    let catalog_message = catalog.catalogue_message(353_400).unwrap();
    assert!(text_of(&catalog_message).len() < MAX_SKILLS_CONTEXT_BYTES);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn an_overfull_catalogue_is_bounded_and_reports_omitted_skills() {
    let root = temporary_root("catalog-omission");
    let skills_root = root.join("skills");
    for index in 0..30 {
        write_skill(
            &skills_root,
            &format!("skill-{index:02}"),
            &format!("skill-{index:02}"),
            "A deliberately long description for bounded catalogue allocation",
            "body",
        );
    }
    let catalog = SkillCatalog::load_from_roots(&[SkillRoot {
        path: &skills_root,
        scope: SkillScope::Repository,
    }]);
    let visible = catalog.skills().iter().collect::<Vec<_>>();
    let (lines, omitted) = render_catalog_lines(&visible, 800);

    assert!(omitted > 0);
    assert!(lines_bytes(&lines) <= 800);
    let message = catalog.catalogue_message(10_000).unwrap();
    assert!(text_of(&message).contains("additional skill(s) were omitted"));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn project_discovery_stops_at_the_git_boundary_and_uses_dot_bcodex_roots() {
    let root = temporary_root("project-roots");
    let repository = root.join("repo");
    let nested = repository.join("service/src");
    fs::create_dir_all(repository.join(".git")).unwrap();
    fs::create_dir_all(&nested).unwrap();

    let roots = discovery_roots_with_home(&nested, None);
    let repository_roots = roots
        .iter()
        .filter(|(_, scope)| *scope == SkillScope::Repository)
        .map(|(path, _)| path.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        repository_roots,
        vec![
            repository.join(".bcodex/skills"),
            repository.join("service/.bcodex/skills"),
            nested.join(".bcodex/skills"),
        ]
    );
    assert!(
        !repository_roots
            .iter()
            .any(|path| path.starts_with(&root) && !path.starts_with(&repository))
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn malformed_skill_is_skipped_while_optional_metadata_fails_open() {
    let root = temporary_root("errors");
    let skills_root = root.join("skills");
    let invalid_directory = skills_root.join("invalid");
    fs::create_dir_all(&invalid_directory).unwrap();
    fs::write(invalid_directory.join(SKILL_FILE_NAME), "no frontmatter").unwrap();
    let valid = write_skill(&skills_root, "valid", "valid", "Works", "body");
    let metadata_directory = valid.parent().unwrap().join("agents");
    fs::create_dir_all(&metadata_directory).unwrap();
    fs::write(metadata_directory.join("openai.yaml"), "interface: [").unwrap();

    let catalog = SkillCatalog::load_from_roots(&[SkillRoot {
        path: &skills_root,
        scope: SkillScope::Repository,
    }]);
    assert_eq!(catalog.skills().len(), 1);
    assert_eq!(catalog.skills()[0].name(), "valid");
    assert_eq!(catalog.warnings().len(), 2);
    assert!(catalog.warnings()[0].contains("invalid"));
    assert!(catalog.warnings()[1].contains("optional skill metadata"));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn an_unbroken_frontmatter_line_is_bounded_before_allocation() {
    let root = temporary_root("frontmatter-line-bound");
    let path = root.join(SKILL_FILE_NAME);
    fs::write(
        &path,
        format!(
            "---\ndescription: {}\n---\nbody",
            "x".repeat(MAX_METADATA_BYTES)
        ),
    )
    .unwrap();

    let error = read_frontmatter(&path).unwrap_err();

    assert!(error.to_string().contains("frontmatter exceeds"));
    fs::remove_dir_all(root).unwrap();
}
