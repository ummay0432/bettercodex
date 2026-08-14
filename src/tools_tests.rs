use super::*;
use serde_json::json;

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "bettercodex-tools-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tiny_png() -> Vec<u8> {
    let image = image::ImageBuffer::from_pixel(1, 1, image::Rgba([10_u8, 20, 30, 255]));
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    bytes.into_inner()
}

fn jpeg_advertising_dimensions(width: u16, height: u16) -> Vec<u8> {
    let image = image::ImageBuffer::from_pixel(1, 1, image::Rgb([10_u8, 20, 30]));
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut bytes, image::ImageFormat::Jpeg)
        .unwrap();
    let mut bytes = bytes.into_inner();
    let frame = bytes
        .windows(2)
        .position(|marker| marker == [0xff, 0xc0])
        .expect("baseline JPEG frame header");
    bytes[frame + 5..frame + 7].copy_from_slice(&height.to_be_bytes());
    bytes[frame + 7..frame + 9].copy_from_slice(&width.to_be_bytes());
    bytes
}

fn test_runtime(cwd: PathBuf) -> ToolRuntime {
    ToolRuntime::new(cwd)
}

fn test_read(cwd: &Path, input: Value) -> Result<ToolResult> {
    read(cwd, input, &CancellationToken::new())
}

fn test_write(cwd: &Path, input: Value) -> Result<ToolResult> {
    write(cwd, input, &CancellationToken::new())
}

fn test_edit(cwd: &Path, input: Value) -> Result<ToolResult> {
    edit(cwd, input, &CancellationToken::new())
}

#[test]
fn parses_ordinary_function_calls() {
    let item = json!({
        "type": "function_call",
        "call_id": "call-1",
        "namespace": "functions",
        "name": "bash",
        "arguments": r#"{"command":"printf done"}"#,
    });
    assert_eq!(
        ToolCall::from_response_item(&item),
        Some(ToolCall {
            call_id: "call-1".to_string(),
            name: "bash".to_string(),
            arguments: r#"{"command":"printf done"}"#.to_string(),
        })
    );
    assert_eq!(
        ToolCall::from_response_item(&json!({
            "type": "function_call",
            "call_id": "call-2",
            "namespace": "",
            "name": "read",
            "arguments": r#"{"path":"SPEC.md"}"#,
        })),
        Some(ToolCall {
            call_id: "call-2".to_string(),
            name: "read".to_string(),
            arguments: r#"{"path":"SPEC.md"}"#.to_string(),
        })
    );
}

#[tokio::test]
async fn foreign_namespace_returns_an_unknown_tool_output_without_dispatching_locally() {
    let root = TemporaryDirectory::new("foreign-namespace");
    let call = ToolCall::from_response_item(&json!({
        "type": "function_call",
        "call_id": "call-foreign",
        "namespace": "foreign",
        "name": "write",
        "arguments": r#"{"path":"unexpected.txt","content":"unexpected"}"#,
    }))
    .unwrap();

    let result = call
        .execute(
            &test_runtime(root.0.clone()),
            TruncationPolicy::Tokens(MAX_MODEL_VISIBLE_TOOL_OUTPUT_TOKENS),
            None,
            CancellationToken::new(),
        )
        .await;

    assert_eq!(
        result.body,
        Value::String("unknown tool `foreign.write`".to_string())
    );
    assert!(!root.0.join("unexpected.txt").exists());
}

#[test]
fn emits_one_ordinary_function_output() {
    let call = ToolCall {
        call_id: "call-1".to_string(),
        name: "bash".to_string(),
        arguments: "{}".to_string(),
    };
    let result = ToolResult::bash_output(
        "done".to_string(),
        String::new(),
        0,
        TruncationPolicy::Tokens(MAX_MODEL_VISIBLE_TOOL_OUTPUT_TOKENS),
    )
    .unwrap();

    let (output, completion) = call.into_output_item(result);
    assert_eq!(
        output,
        json!({
            "type": "function_call_output",
            "call_id": "call-1",
            "output": r#"{"exit_code":0,"stderr":"","stdout":"done"}"#,
        })
    );
    assert_eq!(completion.call_id, "call-1");
    assert!(completion.error.is_none());
    assert!(completion.file_change.is_none());
}

#[test]
fn structured_output_stays_bounded_after_json_escaping() {
    let policy = TruncationPolicy::Tokens(32);
    let result = ToolResult::bash_output("\0".repeat(10_000), String::new(), 0, policy).unwrap();
    let body = result.body.as_str().unwrap();

    assert!(body.len() <= policy.byte_budget());
    let decoded: Value = serde_json::from_str(body).unwrap();
    assert_eq!(decoded["exit_code"], 0);
    assert!(decoded["stdout"].is_string());

    let dense_policy = TruncationPolicy::Tokens(128);
    let dense = ToolResult::bash_output(
        format!("head-marker{}tail-marker", "x".repeat(10_000)),
        String::new(),
        0,
        dense_policy,
    )
    .unwrap();
    let dense_body = dense.body.as_str().unwrap();
    let dense_output: Value = serde_json::from_str(dense_body).unwrap();
    let stdout = dense_output["stdout"].as_str().unwrap();
    assert!(dense_body.len() <= dense_policy.byte_budget());
    assert!(stdout.contains("head-marker"));
    assert!(stdout.ends_with("tail-marker"));
    assert!(stdout.len() > dense_policy.byte_budget() / 2);
}

#[tokio::test]
async fn bash_bounds_live_events_without_truncating_captured_output() {
    let root = TemporaryDirectory::new("bash-live-output");
    let runtime = test_runtime(root.0.clone());
    let emitted_bytes = MAX_FORWARDED_LIVE_OUTPUT_BYTES * 2;
    let call = ToolCall {
        call_id: "call-bash-output".to_string(),
        name: BASH_NAME.to_string(),
        arguments: json!({
            "command": format!(
                "printf warning >&2; printf head-marker; yes x | tr -d '\\n' | head -c {emitted_bytes}; printf tail-marker"
            )
        })
        .to_string(),
    };
    let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();

    let result = call
        .execute(
            &runtime,
            TruncationPolicy::Tokens(MAX_MODEL_VISIBLE_TOOL_OUTPUT_TOKENS),
            Some(events_tx),
            CancellationToken::new(),
        )
        .await;

    let mut forwarded_bytes = 0;
    let mut omission_events = 0;
    while let Ok(event) = events_rx.try_recv() {
        if let AgentEvent::ToolOutputDelta { chunk, .. } = event {
            if chunk.contains("additional live output omitted") {
                omission_events += 1;
            } else {
                forwarded_bytes += chunk.len();
            }
        }
    }
    assert_eq!(forwarded_bytes, MAX_FORWARDED_LIVE_OUTPUT_BYTES);
    assert_eq!(omission_events, 1);

    let body: Value = serde_json::from_str(result.body.as_str().unwrap()).unwrap();
    assert_eq!(body["exit_code"], 0);
    assert_eq!(body["stderr"], "warning");
    let stdout = body["stdout"].as_str().unwrap();
    assert!(stdout.contains("tail-marker"));
    assert!(
        stdout.len()
            > approx_bytes_for_tokens(MAX_MODEL_VISIBLE_TOOL_OUTPUT_TOKENS).saturating_mul(3) / 4
    );
}

#[tokio::test]
async fn bash_does_not_claim_omission_at_the_exact_live_output_budget() {
    let root = TemporaryDirectory::new("bash-exact-live-output");
    let runtime = test_runtime(root.0.clone());
    let call = ToolCall {
        call_id: "call-exact-bash-output".to_string(),
        name: BASH_NAME.to_string(),
        arguments: json!({
            "command": format!(
                "yes x | tr -d '\\n' | head -c {MAX_FORWARDED_LIVE_OUTPUT_BYTES}"
            )
        })
        .to_string(),
    };
    let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();

    call.execute(
        &runtime,
        TruncationPolicy::Tokens(MAX_MODEL_VISIBLE_TOOL_OUTPUT_TOKENS),
        Some(events_tx),
        CancellationToken::new(),
    )
    .await;

    let mut forwarded_bytes = 0_usize;
    let mut claimed_omission = false;
    while let Ok(event) = events_rx.try_recv() {
        if let AgentEvent::ToolOutputDelta { chunk, .. } = event {
            claimed_omission |= chunk.contains("additional live output omitted");
            if !claimed_omission {
                forwarded_bytes = forwarded_bytes.saturating_add(chunk.len());
            }
        }
    }
    assert_eq!(forwarded_bytes, MAX_FORWARDED_LIVE_OUTPUT_BYTES);
    assert!(!claimed_omission);
}

#[test]
fn edit_applies_multiple_unique_replacements_and_is_atomic_on_validation_failure() {
    let root = TemporaryDirectory::new("edit");
    let path = root.0.join("sample.txt");
    std::fs::write(&path, "alpha\nbeta\ngamma\n").unwrap();

    let result = test_edit(
        &root.0,
        json!({
            "path": "sample.txt",
            "edits": [
                {"oldText": "alpha", "newText": "one"},
                {"oldText": "gamma", "newText": "three"},
            ],
        }),
    )
    .unwrap();
    assert_eq!(
        result.file_change,
        Some(ToolFileChange {
            path: path.clone(),
            change: FileChange::Update {
                unified_diff: diffy::create_patch("alpha\nbeta\ngamma\n", "one\nbeta\nthree\n")
                    .to_string(),
                move_path: None,
            },
        })
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "one\nbeta\nthree\n"
    );

    let before = std::fs::read(&path).unwrap();
    let error = test_edit(
        &root.0,
        json!({
            "path": "sample.txt",
            "edits": [{"oldText": "e", "newText": "x"}],
        }),
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("match exactly once"));
    assert_eq!(std::fs::read(&path).unwrap(), before);

    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let error = edit(
        &root.0,
        json!({
            "path": "sample.txt",
            "edits": [{"oldText": "one", "newText": "cancelled"}],
        }),
        &cancellation,
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("edit was interrupted"));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(std::fs::read_dir(&root.0).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".tmp")
    }));

    let oversized = root.0.join("oversized.txt");
    std::fs::File::create(&oversized)
        .unwrap()
        .set_len((MAX_EDIT_FILE_BYTES as u64).saturating_add(1))
        .unwrap();
    let error = test_edit(
        &root.0,
        json!({
            "path": "oversized.txt",
            "edits": [{"oldText": "old", "newText": "new"}],
        }),
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("64 MiB edit limit"));
}

#[test]
fn edit_matches_normalized_text_and_preserves_bom_and_line_endings() {
    let root = TemporaryDirectory::new("edit-crlf");
    let path = root.0.join("sample.txt");
    std::fs::write(&path, "\u{feff}first\r\nsecond\r\nthird\r\n").unwrap();

    test_edit(
        &root.0,
        json!({
            "path": "sample.txt",
            "edits": [{"oldText": "first\n", "newText": "FIRST\n"}],
        }),
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "\u{feff}FIRST\r\nsecond\r\nthird\r\n"
    );

    std::fs::write(&path, "first\rsecond\rthird\r").unwrap();
    test_edit(
        &root.0,
        json!({
            "path": "sample.txt",
            "edits": [{"oldText": "first\n", "newText": "FIRST\n"}],
        }),
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "FIRST\rsecond\rthird\r"
    );

    std::fs::write(&path, "hello\r\nworld\r\n---\r\nhello\nworld\n").unwrap();
    let before = std::fs::read(&path).unwrap();
    let error = test_edit(
        &root.0,
        json!({
            "path": "sample.txt",
            "edits": [{"oldText": "hello\nworld\n", "newText": "replacement\n"}],
        }),
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("found multiple matches"));
    assert_eq!(std::fs::read(&path).unwrap(), before);

    let error = test_edit(
        &root.0,
        json!({
            "path": "sample.txt",
            "edits": [{"oldText": "---", "newText": "---"}],
        }),
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("did not change"));
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn write_reports_creation_then_replacement_as_structured_file_changes() {
    let root = TemporaryDirectory::new("write-file-change");
    let path = root.0.join("sample.txt");

    let created = test_write(
        &root.0,
        json!({"path": "sample.txt", "content": "alpha\nbeta\n"}),
    )
    .unwrap();
    assert_eq!(
        created.file_change,
        Some(ToolFileChange {
            path: path.clone(),
            change: FileChange::Add {
                content: "alpha\nbeta\n".to_string(),
            },
        })
    );

    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
    let replaced = test_write(
        &root.0,
        json!({"path": "sample.txt", "content": "alpha\ngamma\n"}),
    )
    .unwrap();
    assert_eq!(
        replaced.file_change,
        Some(ToolFileChange {
            path: path.clone(),
            change: FileChange::Update {
                unified_diff: diffy::create_patch("alpha\nbeta\n", "alpha\ngamma\n").to_string(),
                move_path: None,
            },
        })
    );
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o640
    );

    let link = root.0.join("sample-link.txt");
    std::os::unix::fs::symlink("sample.txt", &link).unwrap();
    test_write(
        &root.0,
        json!({"path": "sample-link.txt", "content": "through symlink\n"}),
    )
    .unwrap();
    assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "through symlink\n");

    let dangling_link = root.0.join("dangling-link.txt");
    let dangling_target = root.0.join("nested/dangling-target.txt");
    std::os::unix::fs::symlink("nested/dangling-target.txt", &dangling_link).unwrap();
    let created_through_link = test_write(
        &root.0,
        json!({"path": "dangling-link.txt", "content": "created through symlink\n"}),
    )
    .unwrap();
    assert!(
        std::fs::symlink_metadata(&dangling_link)
            .unwrap()
            .is_symlink()
    );
    assert_eq!(
        std::fs::read_to_string(&dangling_target).unwrap(),
        "created through symlink\n"
    );
    assert_eq!(
        created_through_link.file_change,
        Some(ToolFileChange {
            path: dangling_link,
            change: FileChange::Add {
                content: "created through symlink\n".to_string(),
            },
        })
    );
}

#[test]
fn write_omits_expensive_diff_preview_without_omitting_the_write() {
    let root = TemporaryDirectory::new("write-diff-budget");
    let path = root.0.join("many-short-lines.txt");
    let original = (0..MAX_FILE_CHANGE_DIFF_LINES / 2 + 1)
        .map(|index| format!("old-{index}\n"))
        .collect::<String>();
    let replacement = (0..MAX_FILE_CHANGE_DIFF_LINES / 2)
        .map(|index| format!("new-{index}\n"))
        .collect::<String>();
    std::fs::write(&path, original).unwrap();

    let result = test_write(
        &root.0,
        json!({"path": "many-short-lines.txt", "content": &replacement}),
    )
    .unwrap();

    assert!(result.file_change.is_none());
    assert_eq!(std::fs::read_to_string(path).unwrap(), replacement);
}

#[test]
fn atomic_file_tools_support_destinations_at_the_filename_length_limit() {
    let root = TemporaryDirectory::new("atomic-long-filename");
    // Linux and macOS filesystems commonly permit 255-byte basenames. Keep one byte of headroom
    // while still proving that the atomic helper does not prepend the destination's full name to
    // its temporary file.
    let name = format!("{}.txt", "x".repeat(250));
    let path = root.0.join(&name);

    test_write(&root.0, json!({"path": &name, "content": "before\n"})).unwrap();
    test_edit(
        &root.0,
        json!({
            "path": &name,
            "edits": [{"oldText": "before", "newText": "after"}],
        }),
    )
    .unwrap();

    assert_eq!(std::fs::read_to_string(path).unwrap(), "after\n");
}

#[tokio::test]
async fn write_completion_reports_file_changes_and_rejects_special_files() {
    let root = TemporaryDirectory::new("write-file-change-event");
    let runtime = test_runtime(root.0.clone());
    let call = ToolCall {
        call_id: "call-write".to_string(),
        name: WRITE_NAME.to_string(),
        arguments: json!({"path": "sample.txt", "content": "alpha\nbeta\n"}).to_string(),
    };
    let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();

    let _result = call
        .execute(
            &runtime,
            TruncationPolicy::Tokens(MAX_MODEL_VISIBLE_TOOL_OUTPUT_TOKENS),
            Some(events_tx),
            CancellationToken::new(),
        )
        .await;

    let completion = std::iter::from_fn(|| events_rx.try_recv().ok()).find_map(|event| {
        if let AgentEvent::ToolCompleted { file_change, .. } = event {
            Some(file_change)
        } else {
            None
        }
    });
    assert_eq!(
        completion,
        Some(Some(ToolFileChange {
            path: root.0.join("sample.txt"),
            change: FileChange::Add {
                content: "alpha\nbeta\n".to_string(),
            },
        }))
    );

    let socket_path = root.0.join("service.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
    let fifo_path = root.0.join("events.fifo");
    use std::os::unix::ffi::OsStrExt;
    let fifo_c_path = std::ffi::CString::new(fifo_path.as_os_str().as_bytes()).unwrap();
    // SAFETY: `fifo_c_path` is a valid, NUL-terminated pathname that remains alive for the call.
    assert_eq!(unsafe { libc::mkfifo(fifo_c_path.as_ptr(), 0o600) }, 0);

    for path in [&socket_path, &fifo_path] {
        let call = ToolCall {
            call_id: format!("call-write-{}", path.file_name().unwrap().to_string_lossy()),
            name: WRITE_NAME.to_string(),
            arguments: json!({"path": path, "content": "must not replace special file"})
                .to_string(),
        };
        let result = call
            .execute(
                &runtime,
                TruncationPolicy::Tokens(MAX_MODEL_VISIBLE_TOOL_OUTPUT_TOKENS),
                None,
                CancellationToken::new(),
            )
            .await;
        assert_eq!(
            result.body,
            Value::String(format!(
                "write path `{}` is not a regular file",
                path.display()
            ))
        );
    }

    use std::os::unix::fs::FileTypeExt;
    assert!(
        std::fs::symlink_metadata(socket_path)
            .unwrap()
            .file_type()
            .is_socket()
    );
    assert!(
        std::fs::symlink_metadata(fifo_path)
            .unwrap()
            .file_type()
            .is_fifo()
    );
}

#[tokio::test]
async fn read_bounds_output_and_reports_a_continuation_offset() {
    let root = TemporaryDirectory::new("read");
    std::fs::write(root.0.join("sample.txt"), "one\ntwo\nthree\n").unwrap();

    let result = test_read(
        &root.0,
        json!({"path": "sample.txt", "offset": 2, "limit": 1}),
    )
    .unwrap();
    let text = result.body.as_str().unwrap();
    assert!(text.starts_with("two\n"));
    assert!(text.contains(&format!("bounded at 1 line or {MAX_READ_BYTES} bytes")));
    assert!(text.contains("offset=3"));

    let call = ToolCall {
        call_id: "call-read-invalid-limit".to_string(),
        name: READ_NAME.to_string(),
        arguments: json!({"path": "sample.txt", "limit": MAX_READ_LINES + 1}).to_string(),
    };
    let result = call
        .execute(
            &test_runtime(root.0.clone()),
            TruncationPolicy::Tokens(MAX_MODEL_VISIBLE_TOOL_OUTPUT_TOKENS),
            None,
            CancellationToken::new(),
        )
        .await;
    assert_eq!(
        result.body,
        Value::String(format!(
            "read.limit must be no greater than {MAX_READ_LINES}"
        ))
    );

    let long_line = format!("{}\n", "x".repeat(99));
    std::fs::write(root.0.join("large.txt"), long_line.repeat(500)).unwrap();
    let result = test_read(&root.0, json!({"path": "large.txt"})).unwrap();
    let text = result.body.as_str().unwrap();
    assert!(text.len() <= approx_bytes_for_tokens(MAX_MODEL_VISIBLE_TOOL_OUTPUT_TOKENS));
    assert!(text.contains("Use offset="));
    assert!(!text.starts_with("Warning: truncated output"));

    let quoted = "\"".repeat(30_000);
    std::fs::write(root.0.join("quoted.txt"), &quoted).unwrap();
    let result = test_read(&root.0, json!({"path": "quoted.txt"})).unwrap();
    assert_eq!(result.body.as_str(), Some(quoted.as_str()));

    std::fs::write(root.0.join("invalid-utf8.txt"), [b'o', b'k', b'\n', 0xff]).unwrap();
    let error = test_read(&root.0, json!({"path": "invalid-utf8.txt"}))
        .err()
        .unwrap();
    assert!(error.to_string().contains("not valid UTF-8 text"));

    let overlong_after_prefix = format!("prefix\n{}\n", "x".repeat(MAX_READ_BYTES + 1));
    std::fs::write(
        root.0.join("overlong-after-prefix.txt"),
        overlong_after_prefix,
    )
    .unwrap();
    let result = test_read(&root.0, json!({"path": "overlong-after-prefix.txt"})).unwrap();
    let text = result.body.as_str().unwrap();
    assert!(text.starts_with("prefix\n"));
    assert!(text.contains("offset=2"));
    let error = test_read(
        &root.0,
        json!({"path": "overlong-after-prefix.txt", "offset": 2}),
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("requested line exceeds"));
}

#[test]
fn read_detects_images_from_bytes_and_enforces_media_specific_arguments() {
    let root = TemporaryDirectory::new("read-image");
    std::fs::write(root.0.join("visual.txt"), tiny_png()).unwrap();
    std::fs::write(root.0.join("plain.png"), "plain text").unwrap();
    std::fs::write(
        root.0.join("bitmap-header.txt"),
        "BM is ordinary UTF-8 text",
    )
    .unwrap();

    let result = test_read(&root.0, json!({"path": "visual.txt", "detail": "original"})).unwrap();
    let items = result.body.as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["type"], "input_image");
    assert_eq!(items[0]["detail"], "original");
    assert!(
        items[0]["image_url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,")
    );

    let image_options_error = test_read(&root.0, json!({"path": "visual.txt", "offset": 1}))
        .err()
        .unwrap();
    assert!(
        image_options_error
            .to_string()
            .contains("only supported for text files")
    );
    let text_options_error = test_read(&root.0, json!({"path": "plain.png", "detail": "high"}))
        .err()
        .unwrap();
    assert!(
        text_options_error
            .to_string()
            .contains("only supported for image files")
    );
    let bitmap_text = test_read(&root.0, json!({"path": "bitmap-header.txt"})).unwrap();
    assert_eq!(
        bitmap_text.body,
        Value::String("BM is ordinary UTF-8 text".to_string())
    );
}

#[test]
fn read_rejects_images_whose_decoded_buffer_exceeds_the_processing_limit() {
    let root = TemporaryDirectory::new("oversized-decoded-image");
    std::fs::write(
        root.0.join("oversized-decoded.jpg"),
        jpeg_advertising_dimensions(20_000, 20_000),
    )
    .unwrap();

    let error = test_read(&root.0, json!({"path": "oversized-decoded.jpg"}))
        .err()
        .unwrap();
    assert_eq!(error.to_string(), READ_IMAGE_INVALID_MESSAGE);
}

#[test]
fn read_rejects_oversized_images_before_loading_them() {
    let root = TemporaryDirectory::new("oversized-image");
    let path = root.0.join("oversized.png");
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(&tiny_png()).unwrap();
    file.set_len(u64::try_from(crate::input::MAX_TOTAL_IMAGE_BYTES).unwrap() + 1)
        .unwrap();

    let error = test_read(&root.0, json!({"path": "oversized.png"}))
        .err()
        .unwrap();
    assert!(error.to_string().contains("50 MiB read image limit"));
}
