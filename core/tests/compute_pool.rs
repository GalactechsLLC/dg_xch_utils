use dg_xch_core::compute::{self, Phase};
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn concurrent_batches_share_the_worker_budget_and_keep_result_order() {
    compute::configure(2).unwrap();
    assert_eq!(compute::install_validation(rayon::current_num_threads), 1);
    let active = AtomicUsize::new(0);
    let maximum = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                let results = compute::map(Phase::Body, &[1, 2, 3, 4], |value| {
                    let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                    maximum.fetch_max(count, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    active.fetch_sub(1, Ordering::SeqCst);
                    value * 2
                });
                assert_eq!(results, [2, 4, 6, 8]);
            });
        }
    });
    assert!(maximum.load(Ordering::SeqCst) <= 2);
    assert_eq!(
        compute::counters(Phase::Body)
            .pending
            .load(Ordering::Relaxed),
        0
    );
    assert_eq!(
        compute::counters(Phase::Body)
            .active
            .load(Ordering::Relaxed),
        0
    );
    let result = std::panic::catch_unwind(|| {
        compute::map(Phase::Archive, &[1, 2, 3, 4], |_| -> usize {
            panic!("injected job failure");
        });
    });
    assert!(result.is_err());
    assert_eq!(
        compute::counters(Phase::Archive)
            .pending
            .load(Ordering::Relaxed),
        0
    );
    assert_eq!(
        compute::counters(Phase::Archive)
            .active
            .load(Ordering::Relaxed),
        0
    );
    assert_eq!(compute::map(Phase::Archive, &[7], |value| value * 2), [14]);
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = std::sync::Mutex::new(release_rx);
    let (archive_tx, archive_rx) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            compute::map(Phase::Body, &[0], |_| {
                started_tx.send(()).unwrap();
                release_rx.lock().unwrap().recv().unwrap();
            });
        });
        let started = started_rx.recv_timeout(std::time::Duration::from_secs(5));
        scope.spawn(|| {
            let result = compute::map(Phase::Archive, &[7], |value| value * 2);
            let _ = archive_tx.send(result);
        });
        let archived = archive_rx.recv_timeout(std::time::Duration::from_secs(5));
        release_tx.send(()).unwrap();
        assert!(started.is_ok());
        assert_eq!(archived.unwrap(), [14]);
    });
    assert_eq!(compute::worker_count(), 2);
}
