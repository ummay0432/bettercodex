use super::*;
use crate::skills::SkillCatalog;
use crossterm::event::KeyModifiers;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::fs;
use uuid::Uuid;

fn test_skills() -> (PathBuf, Vec<Skill>) {
    let root = std::env::temp_dir().join(format!(
        "bettercodex-skills-view-{}-{}",
        std::process::id(),
        Uuid::new_v4()
    ));
    let cwd = root.join("repo");
    fs::create_dir_all(cwd.join(".git")).unwrap();
    for (directory, name, description, implicit) in [
        ("alpha", "Alpha Skill", "Handle alpha work", true),
        ("beta", "Beta Skill", "Handle beta work", false),
    ] {
        let skill = cwd.join(".bcodex/skills").join(directory);
        fs::create_dir_all(skill.join("agents")).unwrap();
        fs::write(
            skill.join("SKILL.md"),
            format!(
                "---\nname: {directory}\ndescription: {description}\n---\n\nFollow the workflow.\n"
            ),
        )
        .unwrap();
        fs::write(
            skill.join("agents/openai.yaml"),
            format!(
                "interface:\n  display_name: {name}\npolicy:\n  allow_implicit_invocation: {implicit}\n"
            ),
        )
        .unwrap();
    }
    let skills = SkillCatalog::load(&cwd)
        .skills()
        .iter()
        .filter(|skill| skill.path().starts_with(&root))
        .cloned()
        .collect();
    (root, skills)
}

#[test]
fn key_actions_toggle_enabled_and_implicit_settings_independently() {
    let (root, skills) = test_skills();
    let mut view = SkillsView::new();

    assert_eq!(
        view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &skills),
        SkillsViewAction::Update {
            path: skills[0].path().to_path_buf(),
            update: SkillUpdate::Enabled(false),
        }
    );
    assert_eq!(
        view.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &skills),
        SkillsViewAction::None
    );
    assert_eq!(
        view.handle_key(
            KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE),
            &skills
        ),
        SkillsViewAction::Update {
            path: skills[1].path().to_path_buf(),
            update: SkillUpdate::AllowImplicitInvocation(true),
        }
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rendered_view_shows_both_controls_and_the_explicit_only_state() {
    let (root, skills) = test_skills();
    let view = SkillsView::new();
    let rendered = render(&view, &skills, 100);

    assert!(rendered.contains("Skills"), "{rendered}");
    assert!(rendered.contains("[x] enabled"), "{rendered}");
    assert!(rendered.contains("[x] implicit"), "{rendered}");
    assert!(rendered.contains("[ ] implicit"), "{rendered}");
    assert!(rendered.contains("Alpha Skill"), "{rendered}");
    assert!(rendered.contains("Beta Skill"), "{rendered}");
    assert!(rendered.contains("i implicit"), "{rendered}");

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn empty_view_explains_where_to_install_the_first_skill() {
    let view = SkillsView::new();
    let rendered = render(&view, &[], 100);

    assert!(rendered.contains("No skills installed."), "{rendered}");
    assert!(rendered.contains(".bcodex/skills"), "{rendered}");
    assert!(rendered.contains("BCODEX_HOME"), "{rendered}");
}

fn render(view: &SkillsView, skills: &[Skill], width: u16) -> String {
    let height = view.preferred_height(skills);
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| {
            let area = frame.area();
            view.render(frame, area, skills);
        })
        .unwrap();
    render_buffer(terminal.backend().buffer())
}

fn render_buffer(buffer: &ratatui::buffer::Buffer) -> String {
    let area = buffer.area;
    (area.y..area.bottom())
        .map(|y| {
            (area.x..area.right())
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}
