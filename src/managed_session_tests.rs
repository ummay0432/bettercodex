use super::*;
use std::ffi::OsStr;

#[test]
fn canonical_c_names_reserve_slots() {
    assert_eq!(session_slot("c1"), Some(1));
    assert_eq!(session_slot("c42"), Some(42));
    for name in ["c", "c0", "c01", "codex1", "C1", "stock-sync", "c1x"] {
        assert_eq!(session_slot(name), None, "{name}");
    }
}

#[test]
fn first_gap_is_reused() {
    assert_eq!(first_free_slot(&BTreeSet::new()).unwrap(), 1);
    assert_eq!(first_free_slot(&BTreeSet::from([1, 3, 4])).unwrap(), 2);
    assert_eq!(first_free_slot(&BTreeSet::from([1, 2, 3])).unwrap(), 4);
}

#[test]
fn tmux_is_not_started_for_an_unsuitable_terminal() {
    assert!(ensure_attachable_terminal(None).is_err());
    assert!(ensure_attachable_terminal(Some(OsStr::new(""))).is_err());
    assert!(ensure_attachable_terminal(Some(OsStr::new("dumb"))).is_err());
    assert!(ensure_attachable_terminal(Some(OsStr::new("xterm-256color"))).is_ok());
}

#[test]
fn tmux_creation_is_detached_sized_and_self_cleaning() {
    let arguments = tmux_create_arguments(
        "c2",
        Path::new("/opt/bcodex"),
        Path::new("/work tree;"),
        &[
            "resume".to_string(),
            "session id".to_string(),
            "do it;".to_string(),
            "keep \\;".to_string(),
        ],
        Some((151, 47)),
        &[OsString::from("BCODEX_HOME=/state dir;")],
    );
    assert_eq!(
        arguments,
        [
            "new-session",
            "-d",
            "-P",
            "-F",
            "#{session_id}",
            "-s",
            "c2",
            "-n",
            "bcodex",
            "-c",
            "/work tree\\;",
            "-x",
            "151",
            "-y",
            "47",
            "-e",
            "BCODEX_HOME=/state dir\\;",
            "--",
            "/opt/bcodex",
            "resume",
            "session id",
            "do it\\;",
            "keep \\\\;",
            ";",
            "set-option",
            "-t",
            "c2",
            "destroy-unattached",
            "off",
            ";",
            "set-option",
            "-t",
            "c2",
            "detach-on-destroy",
            "on",
            ";",
            "set-window-option",
            "-t",
            "c2",
            "remain-on-exit",
            "off",
        ]
        .map(OsString::from)
    );
}

#[test]
fn caffeinate_wraps_and_marks_the_reexecuted_agent() {
    let command = caffeinate_command(
        Path::new("/Applications/bcodex"),
        &["resume".to_string(), "abc".to_string()],
    );
    assert_eq!(command.get_program(), OsStr::new("/usr/bin/caffeinate"));
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        ["-i", "-s", "/Applications/bcodex", "resume", "abc"].map(OsStr::new)
    );
    assert!(
        command
            .get_envs()
            .any(|(name, value)| { name == CAFFEINATE_MARKER && value == Some(OsStr::new("1")) })
    );
}

#[test]
fn tmux_literal_escapes_only_a_parser_sensitive_trailing_semicolon() {
    assert_eq!(tmux_literal(OsStr::new("plain")), OsStr::new("plain"));
    assert_eq!(
        tmux_literal(OsStr::new("semi;colon")),
        OsStr::new("semi;colon")
    );
    assert_eq!(
        tmux_literal(OsStr::new("trailing;")),
        OsStr::new("trailing\\;")
    );
    assert_eq!(
        tmux_literal(OsStr::new("already\\;")),
        OsStr::new("already\\\\;")
    );
}
