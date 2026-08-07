use super::*;
use std::ffi::OsStr;
use std::fs::File;
use std::os::fd::IntoRawFd;
use std::os::unix::fs::MetadataExt;

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
fn tmux_requires_a_capable_terminal_only_when_requested() {
    assert!(ensure_attachable_terminal(None).is_err());
    assert!(ensure_attachable_terminal(Some(OsStr::new(""))).is_err());
    assert!(ensure_attachable_terminal(Some(OsStr::new("dumb"))).is_err());
    assert!(ensure_attachable_terminal(Some(OsStr::new("xterm-256color"))).is_ok());
}

#[test]
fn tmux_creation_runs_only_the_private_relay_in_a_durable_c_session() {
    let arguments = tmux_create_arguments(
        "c2",
        Path::new("/opt/bcodex"),
        Path::new("/work tree;"),
        "2f746d702f736f636b6574",
        (151, 47),
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
            "--",
            "/opt/bcodex",
            "--internal-tmux-relay",
            "2f746d702f736f636b6574",
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

#[cfg(target_os = "linux")]
#[test]
fn relay_reexecution_uses_the_live_linux_process_image() {
    let executable = relay_executable().unwrap();
    assert_eq!(executable, linux_process_executable(std::process::id()));

    let relay_metadata = executable.metadata().unwrap();
    let current_metadata = std::env::current_exe().unwrap().metadata().unwrap();
    assert_eq!(
        (relay_metadata.dev(), relay_metadata.ino()),
        (current_metadata.dev(), current_metadata.ino())
    );
}

#[test]
fn relay_paths_round_trip_non_utf8_bytes_and_are_narrowly_validated() {
    let path = PathBuf::from(OsString::from_vec(
        b"/tmp/.bettercodex-tmux-relay-test/socket"
            .iter()
            .copied()
            .chain([0xff])
            .collect(),
    ));
    assert_eq!(decode_path(&encode_path(&path)).unwrap(), path);

    assert!(validate_relay_path(Path::new("relative/socket")).is_err());
    assert!(validate_relay_path(Path::new("/tmp/unrelated/socket")).is_err());
    assert!(
        validate_relay_path(Path::new(
            "/tmp/.bettercodex-tmux-relay-test/not-the-socket"
        ))
        .is_err()
    );
    assert!(validate_relay_path(Path::new("/tmp/.bettercodex-tmux-relay-test/socket")).is_ok());
}

#[test]
fn relay_descriptor_handoff_transfers_one_owned_file_descriptor() {
    let (sender, receiver) = UnixStream::pair().unwrap();
    let source = File::open("/dev/null").unwrap();
    let source_metadata = source.metadata().unwrap();

    send_fd(&sender, source.as_raw_fd()).unwrap();
    let received = receive_fd(&receiver).unwrap();
    let received_metadata = File::from(received).metadata().unwrap();

    assert_eq!(
        (received_metadata.dev(), received_metadata.ino()),
        (source_metadata.dev(), source_metadata.ino())
    );
}

#[test]
fn pseudoterminal_starts_at_the_requested_size() {
    let pty = open_pty((151, 47)).unwrap();
    let mut size = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: the slave descriptor is open and size is writable.
    assert_ne!(
        unsafe { libc::ioctl(pty.slave.as_raw_fd(), libc::TIOCGWINSZ as _, &raw mut size,) },
        -1
    );
    assert_eq!((size.ws_col, size.ws_row), (151, 47));
}

#[test]
fn worker_requests_the_exact_tmux_id_and_commits_only_after_supervisor_ack() {
    let (mut supervisor, worker) = UnixStream::pair().unwrap();
    let (relay, relay_peer) = UnixStream::pair().unwrap();
    let prepared = PreparedTmuxSession {
        session_id: "$17".to_string(),
        session_name: "c17".to_string(),
        relay,
        committed: true,
    };
    let transfer = std::thread::spawn(move || {
        let mut handoff = WorkerHandoff {
            control: Some(worker),
        };
        handoff.transfer(&prepared).unwrap();
        handoff.control.is_none()
    });

    let received_relay = receive_fd(&supervisor).unwrap();
    assert_eq!(read_control_line(&supervisor).unwrap(), "$17");
    supervisor.write_all(&[HANDOFF_COMMITTED_TAG]).unwrap();

    assert!(transfer.join().unwrap());
    drop(received_relay);
    drop(relay_peer);
    assert!(validate_session_id("c17").is_err());
}

#[test]
fn rejected_handoff_preserves_the_supervisor_channel_and_valid_utf8_detail() {
    let (mut supervisor, worker) = UnixStream::pair().unwrap();
    let (relay, _relay_peer) = UnixStream::pair().unwrap();
    let prepared = PreparedTmuxSession {
        session_id: "$18".to_string(),
        session_name: "c18".to_string(),
        relay,
        committed: true,
    };
    let transfer = std::thread::spawn(move || {
        let mut handoff = WorkerHandoff {
            control: Some(worker),
        };
        let error = handoff.transfer(&prepared).unwrap_err();
        (format!("{error:#}"), handoff.control.is_some())
    });

    drop(receive_fd(&supervisor).unwrap());
    assert_eq!(read_control_line(&supervisor).unwrap(), "$18");
    reject_handoff(&mut supervisor, &anyhow!("é".repeat(300))).unwrap();

    let (detail, channel_preserved) = transfer.join().unwrap();
    assert!(channel_preserved);
    assert!(!detail.is_empty());
    assert!(detail.chars().all(|character| character == 'é'));
    assert!(detail.len() <= MAX_CONTROL_MESSAGE_BYTES);
}

#[test]
fn supervisor_passes_the_existing_pty_master_to_the_tmux_relay() {
    let pty = open_pty((80, 24)).unwrap();
    let (supervisor_relay, mut tmux_relay) = UnixStream::pair().unwrap();
    let relay_fd = unsafe { OwnedFd::from_raw_fd(supervisor_relay.into_raw_fd()) };
    let relay = std::thread::spawn(move || {
        let master = receive_fd(&tmux_relay).unwrap();
        tmux_relay.write_all(&[RELAY_READY_TAG]).unwrap();
        write_all_fd(master.as_raw_fd(), b"still-live\n").unwrap();
    });

    complete_relay_handoff(relay_fd, pty.master.as_raw_fd()).unwrap();
    let mut slave = File::from(pty.slave);
    let mut message = [0_u8; 11];
    slave.read_exact(&mut message).unwrap();

    relay.join().unwrap();
    assert_eq!(&message, b"still-live\n");
}

#[test]
fn tmux_client_exit_returns_to_a_clean_shell_surface() {
    let mut terminal = vt100::Parser::new(24, 80, 0);
    terminal
        .write_all(
            b"chat transcript\r\nagent response\x1b[20;1H> /tmux\x1b[22;3H/tmux  move this live session into tmux",
        )
        .unwrap();
    assert!(terminal.screen().contents().contains("chat transcript"));
    assert!(
        terminal
            .screen()
            .contents()
            .contains("move this live session")
    );

    run_with_terminal_cleanup(&mut terminal, |terminal| {
        terminal
            .write_all(b"\x1b[?1049htmux session\x1b[?1049l[exited]\r\n\r\n")
            .unwrap();
    });

    assert_eq!(terminal.screen().contents(), "");
    assert_eq!(terminal.screen().cursor_position(), (0, 0));
    terminal
        .write_all(b"sysadmin@srv-atlas:~/monorepo/bettercodex$ ")
        .unwrap();
    assert_eq!(
        terminal.screen().contents(),
        "sysadmin@srv-atlas:~/monorepo/bettercodex$ "
    );
}

#[test]
fn relay_command_is_private_and_strictly_parsed() {
    assert!(run_relay_command(&[]).is_none());
    assert!(run_relay_command(&["--help".to_string()]).is_none());
    assert!(
        run_relay_command(&[RELAY_COMMAND.to_string()])
            .unwrap()
            .is_err()
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
