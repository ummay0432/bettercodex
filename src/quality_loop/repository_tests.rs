use super::*;
use std::time::Duration;
use std::time::Instant;

const PAYLOAD: &str = "payload.txt";

struct Fixture {
    root: PathBuf,
    repository: PathBuf,
    run_root: PathBuf,
    worktree: Worktree,
}

impl Fixture {
    fn new(contents: &[u8]) -> Self {
        let root = std::env::temp_dir().join(format!("bcodex-snapshot-test-{}", Uuid::new_v4()));
        let repository = root.join("repository");
        let run_root = root.join("run");
        std::fs::create_dir_all(&repository).expect("create repository fixture");
        let status = Command::new("git")
            .current_dir(&repository)
            .args(["init", "--quiet"])
            .status()
            .expect("start git init");
        assert!(status.success());
        std::fs::write(repository.join(PAYLOAD), contents).expect("write repository fixture");
        let status = Command::new("git")
            .current_dir(&repository)
            .args(["add", PAYLOAD])
            .status()
            .expect("start git add");
        assert!(status.success());
        let worktree = Worktree::discover(&repository).expect("discover fixture worktree");
        Self {
            root,
            repository,
            run_root,
            worktree,
        }
    }

    fn capture(&self) -> RepositorySnapshot {
        self.worktree
            .capture(&self.run_root, &[])
            .expect("capture fixture")
    }

    fn capture_until_fully_cached(&self) -> RepositorySnapshot {
        let baseline = self.capture();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let snapshot = self.capture();
            assert_eq!(snapshot.state, baseline.state);
            assert_eq!(snapshot.entries, baseline.entries);
            // The fixture contains one worktree file plus the Git index.
            if self.worktree.snapshot_cache_hits() == 2 {
                return snapshot;
            }
            assert!(
                Instant::now() < deadline,
                "filesystem timestamps never advanced beyond the cache's racy window"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn payload_digest(snapshot: &RepositorySnapshot) -> &str {
        snapshot.entries[PAYLOAD]
            .digest
            .as_deref()
            .expect("payload file has a digest")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn cache_rehashes_same_size_file_when_mtime_is_restored() {
    let fixture = Fixture::new(b"original");
    let cached = fixture.capture_until_fully_cached();
    let path = fixture.repository.join(PAYLOAD);
    let original_modified = std::fs::metadata(&path)
        .expect("inspect original payload")
        .modified()
        .expect("read original mtime");

    std::fs::write(&path, b"modified").expect("modify payload");
    File::open(&path)
        .expect("open modified payload")
        .set_times(std::fs::FileTimes::new().set_modified(original_modified))
        .expect("restore payload mtime");
    let modified = fixture.capture();

    assert_ne!(
        Fixture::payload_digest(&cached),
        Fixture::payload_digest(&modified)
    );
    // The unchanged index is reusable, but ctime forces the payload through
    // the content path even though its size and mtime match the cache entry.
    assert_eq!(fixture.worktree.snapshot_cache_hits(), 1);
}

#[test]
fn cache_still_detects_corrupt_content_addressed_blob() {
    let fixture = Fixture::new(b"snapshot payload");
    let cached = fixture.capture_until_fully_cached();
    let digest = Fixture::payload_digest(&cached);
    let blob = blob_path(&fixture.run_root, digest).expect("resolve payload blob");
    let original_modified = std::fs::metadata(&blob)
        .expect("inspect payload blob")
        .modified()
        .expect("read blob mtime");
    let mut corrupt = std::fs::read(&blob).expect("read payload blob");
    corrupt[0] ^= 0xff;
    std::fs::write(&blob, corrupt).expect("corrupt payload blob");
    File::open(&blob)
        .expect("open corrupt payload blob")
        .set_times(std::fs::FileTimes::new().set_modified(original_modified))
        .expect("restore blob mtime");

    let error = fixture
        .worktree
        .capture(&fixture.run_root, &[])
        .expect_err("corrupt blob must fail capture");
    assert!(
        format!("{error:#}").contains("content-addressed snapshot blob is corrupt"),
        "unexpected error: {error:#}"
    );
}
