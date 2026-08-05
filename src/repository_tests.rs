use super::*;
use uuid::Uuid;

#[test]
fn finds_the_nearest_directory_with_a_git_boundary() {
    let outer = std::env::temp_dir().join(format!("bettercodex-repository-{}", Uuid::new_v4()));
    let inner = outer.join("packages/inner");
    let cwd = inner.join("src/nested");
    std::fs::create_dir_all(outer.join(".git")).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::write(inner.join(".git"), "gitdir: ../../.git/worktrees/inner\n").unwrap();

    assert_eq!(find_root(&cwd), Some(inner));

    std::fs::remove_dir_all(outer).unwrap();
}

#[test]
fn returns_none_outside_a_git_worktree() {
    let cwd = std::env::temp_dir().join(format!("bettercodex-no-repository-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&cwd).unwrap();

    assert_eq!(find_root(&cwd), None);

    std::fs::remove_dir_all(cwd).unwrap();
}
