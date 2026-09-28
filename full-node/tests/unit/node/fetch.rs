use super::*;

#[tokio::test(start_paused = true)]
async fn retries_are_paced_bounded_and_cancelled_by_rebase() {
    let metrics = Arc::new(dg_xch_node::SyncMetrics::default());
    let queue = Arc::new(BlockQueue::new(100, 1024, metrics));
    for (failures, millis) in [(1, 250), (2, 500), (3, 1000), (4, 2000), (u32::MAX, 2000)] {
        assert_eq!(fetch_retry_delay(failures), Duration::from_millis(millis));
    }
    let started = tokio::time::Instant::now();
    wait_fetch_retry(&queue, queue.current_gen(), fetch_retry_delay(1)).await;
    assert_eq!(started.elapsed(), Duration::from_millis(250));
    let waiting_queue = queue.clone();
    let generation = queue.current_gen();
    let waiting = tokio::spawn(async move {
        wait_fetch_retry(&waiting_queue, generation, Duration::from_secs(2)).await;
    });
    tokio::task::yield_now().await;
    let started = tokio::time::Instant::now();
    queue.rebase(101);
    waiting.await.unwrap();
    assert_eq!(started.elapsed(), Duration::ZERO);
}

#[tokio::test]
async fn direct_tip_fetches_do_not_pollute_readahead_misses() {
    let metrics = Arc::new(dg_xch_node::SyncMetrics::default());
    let mut readahead = dg_xch_node::sync::WindowReadahead::new(metrics.clone(), REQUEST_TIMEOUT);
    assert!(
        take_prefetched(&mut readahead, 9255836, 9255837, 9255837)
            .await
            .is_none()
    );
    assert_eq!(metrics.readahead_misses.load(Ordering::Relaxed), 0);
    assert!(
        take_prefetched(&mut readahead, 100, 131, 200)
            .await
            .is_none()
    );
    assert_eq!(metrics.readahead_misses.load(Ordering::Relaxed), 1);
}

struct TipSource(FullBlock);

#[async_trait::async_trait]
impl BlockRangeSource for TipSource {
    fn peer_id(&self) -> u64 {
        1
    }

    fn is_closed(&self) -> bool {
        false
    }

    async fn fetch_range(&self, start: u32, end: u32) -> Result<Vec<FullBlock>, SyncError> {
        assert_eq!((start, end), (self.0.height(), self.0.height()));
        Ok(vec![self.0.clone()])
    }
}

#[tokio::test]
async fn prefetched_tip_window_still_counts_as_a_hit() {
    let block: FullBlock = serde_json::from_str(include_str!(
        "../../../../node/tests/fixtures/full_block_5000000.json"
    ))
    .unwrap();
    let height = block.height();
    let metrics = Arc::new(dg_xch_node::SyncMetrics::default());
    let mut readahead = dg_xch_node::sync::WindowReadahead::new(metrics.clone(), REQUEST_TIMEOUT);
    readahead.fill(&[Arc::new(TipSource(block))], height, height, FETCH_BATCH);
    assert_eq!(readahead.inflight(), 1);
    assert_eq!(
        take_prefetched(&mut readahead, height, height, height)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(metrics.readahead_hits.load(Ordering::Relaxed), 1);
    assert_eq!(metrics.readahead_misses.load(Ordering::Relaxed), 0);
}
