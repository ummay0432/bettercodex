use super::*;
use std::io::Write;
use uuid::Uuid;

#[test]
fn supported_pasted_image_paths_attach_but_other_files_remain_text() {
    let directory =
        std::env::temp_dir().join(format!("bettercodex-paste-image-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&directory).unwrap();
    let image = directory.join("screen shot.png");
    let text = directory.join("notes.txt");
    std::fs::write(&image, b"\x89PNG\r\n\x1a\nfixture").unwrap();
    std::fs::write(&text, b"not an image").unwrap();

    let quoted = format!("\"{}\"", image.display());
    assert!(image_from_pasted_path(&quoted).unwrap().is_ok());
    assert!(image_from_pasted_path(&text.display().to_string()).is_none());
    assert!(image_from_pasted_path("ordinary pasted prose").is_none());

    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn file_urls_are_normalized_before_image_detection() {
    let path = std::env::temp_dir().join(format!("bettercodex-paste-{}.png", Uuid::new_v4()));
    std::fs::write(&path, b"\x89PNG\r\n\x1a\nfixture").unwrap();
    let url = url::Url::from_file_path(&path).unwrap();

    assert!(image_from_pasted_path(url.as_str()).unwrap().is_ok());

    std::fs::remove_file(path).unwrap();
}

#[test]
fn oversized_pasted_images_are_rejected_without_loading_the_file() {
    let path = std::env::temp_dir().join(format!("bettercodex-paste-{}.png", Uuid::new_v4()));
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(b"\x89PNG\r\n\x1a\n").unwrap();
    file.set_len(crate::input::MAX_TOTAL_IMAGE_BYTES as u64 + 1)
        .unwrap();
    drop(file);

    let error = image_from_pasted_path(&path.display().to_string())
        .expect("supported image signature")
        .unwrap_err();
    assert!(error.contains("50 MiB input limit"), "{error}");

    std::fs::remove_file(path).unwrap();
}
