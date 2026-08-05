use super::*;
use std::fs;
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
    assert!(catalog.instructions_message(353_400).is_none());

    let selected = SkillSelection::new("private-workflow", skill.path());
    let injection = catalog.explicit_injections("use $private-workflow", &[selected]);
    assert_eq!(injection.items.len(), 1);
    assert!(text_of(&injection.items[0]).contains("Follow this workflow."));

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

    let catalog_message = catalog.instructions_message(353_400).unwrap();
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
    let message = catalog.instructions_message(10_000).unwrap();
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

    let roots = discovery_roots(&nested);
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
