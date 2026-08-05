use super::*;
use crate::skills::SkillCatalog;
use std::fs;
use std::path::PathBuf;
use uuid::Uuid;

fn catalog() -> (PathBuf, SkillCatalog) {
    let root = std::env::temp_dir().join(format!(
        "bettercodex-skill-popup-{}-{}",
        std::process::id(),
        Uuid::new_v4()
    ));
    let repository = root.join("repo");
    fs::create_dir_all(repository.join(".git")).unwrap();
    for (name, description) in [
        ("manifest", "Write a MANIFEST.md routing map"),
        ("research-docs", "Research official documentation"),
    ] {
        let directory = repository.join(".bcodex/skills").join(name);
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n\nbody\n"),
        )
        .unwrap();
    }
    let loaded = SkillCatalog::load(&repository);
    (root, loaded)
}

#[test]
fn popup_targets_a_skill_mention_after_arbitrary_words() {
    let (root, catalog) = catalog();
    let text = "$manifest test test test $mani";
    let mut popup = SkillPopup::default();
    popup.sync(
        text,
        text.len(),
        std::slice::from_ref(&(0..9)),
        catalog.skills(),
    );

    assert!(popup.is_active());
    let (range, skill) = popup.selected_skill(catalog.skills()).unwrap();
    assert_eq!(range, "$manifest test test test ".len()..text.len());
    assert_eq!(skill.name(), "manifest");

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn selected_bindings_do_not_reopen_and_shell_variables_do_not_trigger() {
    let (root, catalog) = catalog();
    let mut popup = SkillPopup::default();
    popup.sync(
        "$manifest",
        "$manifest".len(),
        std::slice::from_ref(&(0..9)),
        catalog.skills(),
    );
    assert!(!popup.is_active());

    popup.sync("echo $HOME", "echo $HOME".len(), &[], catalog.skills());
    assert!(!popup.is_active());

    popup.sync("echo $", "echo $".len(), &[], catalog.skills());
    assert!(popup.is_active());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn dismissal_is_scoped_to_the_current_token_and_navigation_wraps() {
    let (root, catalog) = catalog();
    let text = "$man $res";
    let mut popup = SkillPopup::default();
    popup.sync(text, 4, &[], catalog.skills());
    assert!(popup.is_active());
    popup.dismiss();
    popup.sync(text, 4, &[], catalog.skills());
    assert!(!popup.is_active());

    popup.sync(text, text.len(), &[], catalog.skills());
    assert!(popup.is_active());
    assert_eq!(
        popup.selected_skill(catalog.skills()).unwrap().1.name(),
        "research-docs"
    );
    popup.move_up();
    popup.move_down();
    assert_eq!(
        popup.selected_skill(catalog.skills()).unwrap().1.name(),
        "research-docs"
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rendered_popup_matches_the_codex_bottom_pane_shape_and_selection_style() {
    let (root, catalog) = catalog();
    let mut popup = SkillPopup::default();
    popup.sync("$", 1, &[], catalog.skills());
    let lines = popup.lines(catalog.skills());
    let rendered = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    assert!(rendered[0].contains("[Skill]"));
    assert_eq!(rendered[rendered.len() - 2], "");
    assert!(
        rendered
            .last()
            .unwrap()
            .contains("Press enter to insert or esc to close")
    );
    assert!(lines[0].spans.iter().all(|span| {
        span.style.fg == Some(Color::Cyan) && span.style.add_modifier.contains(Modifier::BOLD)
    }));

    fs::remove_dir_all(root).unwrap();
}
