use super::*;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use uuid::Uuid;

fn temporary_root(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "bettercodex-operator-settings-{name}-{}-{}",
        std::process::id(),
        Uuid::new_v4()
    ))
}

#[test]
fn missing_settings_keep_automatic_tmux_on() {
    let root = temporary_root("default");
    assert_eq!(read(&root.join(FILE_NAME)).unwrap(), TmuxMode::On);
}

#[test]
fn tmux_mode_round_trips_atomically_with_private_permissions() {
    let root = temporary_root("round-trip");
    let path = root.join("state/settings.json");

    save(&path, TmuxMode::Off).unwrap();
    assert_eq!(read(&path).unwrap(), TmuxMode::Off);
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "{\n  \"version\": 1,\n  \"tmux\": false\n}\n"
    );
    #[cfg(unix)]
    {
        assert_eq!(
            fs::metadata(root.join("state"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    save(&path, TmuxMode::On).unwrap();
    assert_eq!(read(&path).unwrap(), TmuxMode::On);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn malformed_unknown_and_future_settings_are_rejected() {
    let root = temporary_root("invalid");
    fs::create_dir_all(&root).unwrap();
    let path = root.join(FILE_NAME);

    fs::write(&path, "{not json").unwrap();
    assert!(
        read(&path)
            .unwrap_err()
            .to_string()
            .contains("invalid JSON")
    );

    fs::write(&path, r#"{"version":1,"tmux":true,"other":false}"#).unwrap();
    let error = read(&path).unwrap_err();
    assert!(format!("{error:#}").contains("unknown field"));

    fs::write(&path, r#"{"version":2,"tmux":false}"#).unwrap();
    assert!(
        read(&path)
            .unwrap_err()
            .to_string()
            .contains("unsupported settings version 2")
    );

    fs::remove_dir_all(root).unwrap();
}
