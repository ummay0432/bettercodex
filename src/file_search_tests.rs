use super::*;
use std::sync::atomic::AtomicUsize;

#[test]
fn nested_paths_are_attributed_to_the_deepest_search_root() {
    let repository = PathBuf::from("repository");
    let crate_root = repository.join("crates");
    let path = crate_root.join("core").join("src").join("lib.rs");
    let roots = [SearchRoot::new(repository), SearchRoot::new(crate_root)];

    let result = get_file_path(&path, &roots)
        .map(|(root_index, relative_path)| (root_index, PathBuf::from(relative_path)));

    let expected_path = PathBuf::from("core").join("src").join("lib.rs");
    assert_eq!(result, Some((1, expected_path)));
}

#[test]
fn batched_indexing_preserves_every_entry_and_bounds_notifications() {
    const ENTRY_COUNT: usize = INDEX_BATCH_SIZE * 2 + 1;

    let notification_count = Arc::new(AtomicUsize::new(0));
    let notify_count = Arc::clone(&notification_count);
    let notify = Arc::new(move || {
        notify_count.fetch_add(1, Ordering::Relaxed);
    });
    let mut nucleo: Nucleo<IndexedEntry> =
        Nucleo::new(Config::DEFAULT.match_paths(), notify, Some(1), 1);

    {
        let mut batch = IndexBatch::new(nucleo.injector());
        for index in 0..ENTRY_COUNT {
            batch.push(IndexedEntry {
                relative_path: format!("src/file_{index}.rs").into_boxed_str(),
                root_index: 0,
                match_type: MatchType::File,
            });
        }

        assert_eq!(notification_count.load(Ordering::Relaxed), 2);
    }

    assert_eq!(
        notification_count.load(Ordering::Relaxed),
        ENTRY_COUNT.div_ceil(INDEX_BATCH_SIZE)
    );

    while nucleo.tick(10).running {}

    assert_eq!(nucleo.snapshot().item_count(), ENTRY_COUNT as u32);
    assert_eq!(nucleo.snapshot().matched_item_count(), ENTRY_COUNT as u32);
}

#[test]
fn matcher_tick_notifications_are_coalesced_until_the_tick_starts() {
    let (work_tx, work_rx) = unbounded();
    let tick_queued = Arc::new(AtomicBool::new(false));

    thread::scope(|scope| {
        for _ in 0..8 {
            let work_tx = work_tx.clone();
            let tick_queued = Arc::clone(&tick_queued);
            scope.spawn(move || {
                for _ in 0..128 {
                    queue_nucleo_tick(&tick_queued, &work_tx);
                }
            });
        }
    });

    assert_eq!(work_rx.try_iter().count(), 1);

    tick_queued.store(false, Ordering::Release);
    queue_nucleo_tick(&tick_queued, &work_tx);
    assert_eq!(work_rx.try_iter().count(), 1);
}
