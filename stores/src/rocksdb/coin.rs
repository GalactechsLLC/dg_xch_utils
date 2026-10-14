use super::*;
use crate::{BlockStore, CoinStore};
use async_trait::async_trait;
use dg_xch_core::blockchain::coin_record::CoinRecord;
#[cfg(feature = "coin-index")]
use dg_xch_core::protocols::wallet::{CoinState, CoinStateFilters};

fn coin(view: &View<'_>, id: &Bytes32) -> Result<Option<CoinRecord>, StoreError> {
    view.get(&key(COIN, id))?
        .map(|bytes| decode(&bytes))
        .transpose()
}

fn service_indexes(view: &View<'_>) -> Result<bool, StoreError> {
    #[cfg(feature = "coin-index")]
    return match view.get(SERVICE_INDEXES)?.as_deref() {
        None | Some(b"1") => Ok(true),
        Some(b"0") => Ok(false),
        _ => Err(StoreError::Corrupt(
            "invalid rocksdb service index state".into(),
        )),
    };
    #[cfg(not(feature = "coin-index"))]
    {
        let _ = view;
        Ok(false)
    }
}

fn coin_indexes(id: &Bytes32, record: &CoinRecord, services: bool) -> Vec<Vec<u8>> {
    let mut keys = vec![index(
        CONFIRMED,
        record.confirmed_block_index.to_be_bytes(),
        id,
    )];
    if record.spent_block_index > 0 {
        keys.push(index(SPENT, record.spent_block_index.to_be_bytes(), id));
    }
    #[cfg(feature = "coin-index")]
    if services {
        keys.push(index(PUZZLE, record.coin.puzzle_hash, id));
        keys.push(index(PARENT, record.coin.parent_coin_info, id));
    }
    #[cfg(not(feature = "coin-index"))]
    let _ = services;
    keys
}

type CoinWrites = Vec<(Vec<u8>, Option<Vec<u8>>)>;

fn coin_writes(
    id: &Bytes32,
    old: Option<&CoinRecord>,
    value: Option<&CoinRecord>,
    services: bool,
) -> Result<CoinWrites, StoreError> {
    let old_keys = old
        .map(|r| coin_indexes(id, r, services))
        .unwrap_or_default();
    let new_keys = value
        .map(|r| coin_indexes(id, r, services))
        .unwrap_or_default();
    let mut writes = Vec::with_capacity(old_keys.len() + new_keys.len() + 1);
    for k in &old_keys {
        if !new_keys.contains(k) {
            writes.push((k.clone(), None));
        }
    }
    for k in new_keys {
        if !old_keys.contains(&k) {
            writes.push((k, Some(Vec::new())));
        }
    }
    writes.push((
        key(COIN, id),
        value.map(|r| r.to_bytes(VERSION)).transpose()?,
    ));
    Ok(writes)
}

fn write_coin(
    view: &mut View<'_>,
    id: &Bytes32,
    value: Option<CoinRecord>,
) -> Result<(), StoreError> {
    let old = coin(view, id)?;
    let services = service_indexes(view)?;
    view.pending
        .extend(coin_writes(id, old.as_ref(), value.as_ref(), services)?);
    Ok(())
}

fn prepare_changes(
    changes: &[crate::types::OwnedCoinChanges],
    workers: &rayon::ThreadPool,
) -> crate::types::PreparedCoinWindow {
    use rayon::prelude::*;
    let hashed: Vec<Vec<_>> = workers.install(|| {
        changes
            .par_iter()
            .map(|change| {
                let chunks: Vec<Vec<_>> = change
                    .additions
                    .par_chunks(512)
                    .map(|records| {
                        dg_xch_core::compute::measure(
                            dg_xch_core::compute::Phase::CoinPrepare,
                            || {
                                records
                                    .iter()
                                    .map(|record| {
                                        (
                                            record.coin.name(),
                                            CoinRecord {
                                                confirmed_block_index: change.height,
                                                spent_block_index: 0,
                                                spent: false,
                                                timestamp: change.timestamp,
                                                ..*record
                                            },
                                        )
                                    })
                                    .collect()
                            },
                        )
                    })
                    .collect();
                chunks.into_iter().flatten().collect()
            })
            .collect()
    });
    let mut additions = HashMap::new();
    let mut removals = HashMap::new();
    for (change, records) in changes.iter().zip(hashed) {
        for (id, record) in records {
            removals.remove(&id);
            additions.insert(id, record);
        }
        for id in &change.removals {
            if let Some(record) = additions.get_mut(id) {
                record.spent_block_index = change.height;
                record.spent = change.height != 0;
            } else {
                removals.insert(*id, change.height);
            }
        }
    }
    let hints = if cfg!(feature = "hint") {
        let mut pairs: Vec<_> = changes
            .iter()
            .flat_map(|c| c.hints.iter().copied())
            .collect();
        pairs.sort_unstable_by_key(|(hint, id)| (hint.bytes(), id.bytes()));
        pairs.dedup();
        pairs
    } else {
        Vec::new()
    };
    crate::types::PreparedCoinWindow::Rocksdb {
        additions: additions.into_iter().collect(),
        removals: removals.into_iter().collect(),
        hints,
    }
}

fn apply_prepared(
    view: &mut View<'_>,
    prepared: crate::types::PreparedCoinWindow,
    workers: &rayon::ThreadPool,
) -> Result<(), StoreError> {
    use rayon::prelude::*;
    let crate::types::PreparedCoinWindow::Rocksdb {
        additions,
        removals,
        hints: pairs,
    } = prepared
    else {
        return Err(StoreError::Batch(
            "coins prepared by another backend".into(),
        ));
    };
    let services = service_indexes(view)?;
    // Every id occurs once. Read the current batch overlay here, never a pre-writer snapshot.
    let writes: Result<Vec<Vec<_>>, StoreError> = workers.install(|| {
        additions
            .par_chunks(512)
            .map(|chunk| {
                dg_xch_core::compute::measure(dg_xch_core::compute::Phase::CoinPrepare, || {
                    let mut writes = Vec::new();
                    let keys: Vec<_> = chunk.iter().map(|(id, _)| key(COIN, id)).collect();
                    for ((id, value), bytes) in chunk.iter().zip(view.get_many(&keys)?) {
                        let old = bytes.as_deref().map(decode::<CoinRecord>).transpose()?;
                        writes.extend(coin_writes(id, old.as_ref(), Some(value), services)?);
                    }
                    Ok(writes)
                })
            })
            .chain(removals.par_chunks(512).map(|chunk| {
                dg_xch_core::compute::measure(dg_xch_core::compute::Phase::CoinPrepare, || {
                    let mut writes = Vec::new();
                    let keys: Vec<_> = chunk.iter().map(|(id, _)| key(COIN, id)).collect();
                    for ((id, height), bytes) in chunk.iter().zip(view.get_many(&keys)?) {
                        let Some(old) = bytes.as_deref().map(decode::<CoinRecord>).transpose()?
                        else {
                            continue;
                        };
                        let mut value = old;
                        value.spent_block_index = *height;
                        value.spent = *height != 0;
                        writes.extend(coin_writes(id, Some(&old), Some(&value), services)?);
                    }
                    Ok(writes)
                })
            }))
            .collect()
    });
    view.pending.extend(writes?.into_iter().flatten());
    hints(view, &pairs);
    Ok(())
}

fn apply(
    view: &mut View<'_>,
    h: u32,
    timestamp: u64,
    additions: &[CoinRecord],
    removals: &[Bytes32],
) -> Result<(), StoreError> {
    for addition in additions {
        let value = CoinRecord {
            confirmed_block_index: h,
            spent_block_index: 0,
            spent: false,
            timestamp,
            ..*addition
        };
        write_coin(view, &value.coin.name(), Some(value))?;
    }
    for id in removals {
        if let Some(old) = coin(view, id)? {
            let mut value = old;
            value.spent_block_index = h;
            value.spent = h != 0;
            let services = service_indexes(view)?;
            view.pending
                .extend(coin_writes(id, Some(&old), Some(&value), services)?);
        }
    }
    Ok(())
}

fn hints(view: &mut View<'_>, pairs: &[(Bytes32, Bytes32)]) {
    #[cfg(feature = "hint")]
    for (hint, id) in pairs {
        view.put(index(HINT, hint, id), Vec::new());
    }
    #[cfg(not(feature = "hint"))]
    let _ = (view, pairs);
}

fn rollback_coins(view: &mut View<'_>, fork: u32) -> Result<u64, StoreError> {
    let mut count = 0;
    for prefix in [CONFIRMED, SPENT] {
        let mut names = Vec::new();
        if let Some(start) = fork.checked_add(1) {
            view.scan_from(&[prefix], &key(prefix, start.to_be_bytes()), |k, _| {
                if k.len() != 37 {
                    return Err(StoreError::Corrupt("invalid rocksdb height index".into()));
                }
                names.push(hash(&k[5..])?);
                Ok(true)
            })?;
        }
        for id in names {
            let Some(mut value) = coin(view, &id)? else {
                return Err(StoreError::Corrupt("dangling rocksdb height index".into()));
            };
            if prefix == CONFIRMED {
                write_coin(view, &id, None)?;
            } else {
                value.spent_block_index = 0;
                value.spent = false;
                write_coin(view, &id, Some(value))?;
            }
            count += 1;
        }
    }
    // Hint mappings intentionally survive rollback, matching the existing store contract.
    log::info!("rocksdb rollback_coins fork_height={fork} reverted={count}");
    Ok(count)
}

#[cfg(feature = "coin-index")]
fn indexed_coins(
    view: &View<'_>,
    prefix: &[u8],
    mut visit: impl FnMut(CoinRecord) -> Result<bool, StoreError>,
) -> Result<(), StoreError> {
    #[cfg(feature = "coin-index")]
    if matches!(prefix.first(), Some(&PUZZLE) | Some(&PARENT)) && !service_indexes(view)? {
        return Err(StoreError::Batch(
            "rocksdb wallet indexes are deferred during catch-up; retry after index build".into(),
        ));
    }
    view.scan(prefix, |k, _| {
        if k.len() != prefix.len() + 32 {
            return Err(StoreError::Corrupt("invalid rocksdb coin index".into()));
        }
        let id = hash(&k[prefix.len()..])?;
        let value = coin(view, &id)?
            .ok_or_else(|| StoreError::Corrupt("dangling rocksdb coin index".into()))?;
        visit(value)
    })
}

#[async_trait]
impl CoinStore for RocksDbStore {
    fn supports_explorer_queries(&self) -> bool {
        cfg!(feature = "coin-index")
    }

    #[cfg(feature = "coin-index")]
    async fn coin_page(
        &self,
        request: &dg_xch_core::protocols::explorer::CoinPageRequest,
        height: u32,
    ) -> Result<Vec<CoinRecord>, StoreError> {
        request
            .validate()
            .map_err(|e| StoreError::Corrupt(e.into()))?;
        let request = request.clone();
        self.read("coin_page", move |view| {
            let started = std::time::Instant::now();
            let mut page = std::collections::BTreeMap::new();
            let capacity = (request.offset + request.limit) as usize;
            let (prefix, ids) = if request.parent_ids.is_empty() {
                (PUZZLE, &request.puzzle_hashes)
            } else {
                (PARENT, &request.parent_ids)
            };
            let hashes: std::collections::HashSet<_> = ids.iter().copied().collect();
            for ph in hashes {
                indexed_coins(view, &key(prefix, ph), |mut coin| {
                    crate::explorer::work_deadline(started)?;
                    if coin.confirmed_block_index > height {
                        return Ok(true);
                    }
                    if coin.spent_block_index > height {
                        coin.spent_block_index = 0;
                        coin.spent = false;
                    }
                    if request
                        .spent
                        .is_some_and(|spent| spent != (coin.spent_block_index != 0))
                    {
                        return Ok(true);
                    }
                    page.insert(
                        (
                            coin.confirmed_block_index,
                            Vec::<u8>::from(coin.coin.name()),
                        ),
                        coin,
                    );
                    if page.len() > capacity {
                        page.pop_first();
                    }
                    Ok(true)
                })?;
            }
            Ok(page
                .into_values()
                .rev()
                .skip(request.offset as usize)
                .take(request.limit as usize)
                .collect())
        })
        .await
    }
    #[cfg(feature = "coin-index")]
    async fn coin_summary(
        &self,
        ph: &Bytes32,
        height: u32,
    ) -> Result<dg_xch_core::protocols::explorer::CoinSummary, StoreError> {
        let ph = *ph;
        self.read("coin_summary", move |view| {
            let started = std::time::Instant::now();
            let mut total = crate::explorer::Summary::default();
            indexed_coins(view, &key(PUZZLE, ph), |coin| {
                crate::explorer::work_deadline(started)?;
                total.add(&coin, height)?;
                Ok(true)
            })?;
            Ok(total.finish())
        })
        .await
    }

    async fn get_coin_record(&self, name: &Bytes32) -> Result<Option<CoinRecord>, StoreError> {
        let name = *name;
        self.read("coin", move |view| coin(view, &name)).await
    }
    async fn get_coin_records(&self, names: &[Bytes32]) -> Result<Vec<CoinRecord>, StoreError> {
        let names = names.to_vec();
        self.read("coins", move |view| {
            let mut result = Vec::with_capacity(names.len());
            for chunk in names.chunks(512) {
                let keys: Vec<_> = chunk.iter().map(|id| key(COIN, id)).collect();
                for bytes in view.get_many(&keys)?.into_iter().flatten() {
                    result.push(decode::<CoinRecord>(&bytes)?);
                }
            }
            log::debug!(
                "rocksdb coins requested={} found={}",
                names.len(),
                result.len()
            );
            Ok(result)
        })
        .await
    }
    async fn apply_block(
        &self,
        h: u32,
        timestamp: u64,
        additions: &[CoinRecord],
        removals: &[Bytes32],
    ) -> Result<(), StoreError> {
        let mut batch = self.begin().await?;
        self.apply_block_in(&mut batch, h, timestamp, additions, removals)
            .await?;
        self.commit(batch).await
    }
    async fn apply_block_in(
        &self,
        batch: &mut BatchHandle,
        h: u32,
        timestamp: u64,
        additions: &[CoinRecord],
        removals: &[Bytes32],
    ) -> Result<(), StoreError> {
        let additions = additions.to_vec();
        let removals = removals.to_vec();
        self.mutate(batch, "coin_deltas", move |view| {
            log::debug!(
                "rocksdb coin_deltas height={h} additions={} removals={}",
                additions.len(),
                removals.len()
            );
            apply(view, h, timestamp, &additions, &removals)
        })
        .await
    }
    async fn prepare_coin_window(
        &self,
        changes: Vec<crate::types::OwnedCoinChanges>,
    ) -> Result<crate::types::PreparedCoinWindow, StoreError> {
        let workers = self.coin_workers.clone();
        let telemetry = self.telemetry.clone();
        tokio::task::spawn_blocking(move || {
            let _timer = telemetry.coin_prepare.start();
            let input = changes
                .iter()
                .map(|c| c.additions.len() + c.removals.len() + c.hints.len())
                .sum::<usize>();
            let prepared = prepare_changes(&changes, &workers);
            if let crate::types::PreparedCoinWindow::Rocksdb {
                additions,
                removals,
                hints,
            } = &prepared
            {
                telemetry
                    .coin_prepare_input_rows
                    .fetch_add(input as u64, Ordering::Relaxed);
                telemetry.coin_prepare_output_rows.fetch_add(
                    (additions.len() + removals.len() + hints.len()) as u64,
                    Ordering::Relaxed,
                );
            }
            Ok(prepared)
        })
        .await
        .map_err(|e| StoreError::Batch(format!("rocksdb coin preparation: {e}")))?
    }

    async fn apply_prepared_coin_window_in(
        &self,
        batch: &mut BatchHandle,
        prepared: crate::types::PreparedCoinWindow,
    ) -> Result<(), StoreError> {
        let workers = self.coin_workers.clone();
        self.mutate(batch, "coin_window", move |view| {
            apply_prepared(view, prepared, &workers)
        })
        .await
    }

    async fn apply_coin_window_in(
        &self,
        batch: &mut BatchHandle,
        changes: &[crate::types::CoinChanges<'_>],
    ) -> Result<(), StoreError> {
        let changes = changes
            .iter()
            .map(|c| crate::types::OwnedCoinChanges {
                height: c.height,
                timestamp: c.timestamp,
                additions: c.additions.to_vec(),
                removals: c.removals.to_vec(),
                hints: c.hints.to_vec(),
            })
            .collect();
        let prepared = self.prepare_coin_window(changes).await?;
        self.apply_prepared_coin_window_in(batch, prepared).await
    }
    async fn rollback_to(&self, fork: u32) -> Result<u64, StoreError> {
        let mut batch = self.begin().await?;
        let count = self.rollback_to_in(&mut batch, fork).await?;
        self.commit(batch).await?;
        Ok(count)
    }
    async fn rollback_to_in(&self, batch: &mut BatchHandle, fork: u32) -> Result<u64, StoreError> {
        self.mutate(batch, "rollback_coins", move |view| {
            rollback_coins(view, fork)
        })
        .await
    }
    async fn ensure_reorg_indexes(&self) -> Result<(), StoreError> {
        log::info!("rocksdb ensure_reorg_indexes policy=always_live");
        Ok(())
    }
    async fn apply_hints(&self, pairs: &[(Bytes32, Bytes32)]) -> Result<(), StoreError> {
        let mut batch = self.begin().await?;
        self.apply_hints_in(&mut batch, pairs).await?;
        self.commit(batch).await
    }
    async fn apply_hints_in(
        &self,
        batch: &mut BatchHandle,
        pairs: &[(Bytes32, Bytes32)],
    ) -> Result<(), StoreError> {
        let pairs = pairs.to_vec();
        self.mutate(batch, "hints", move |view| {
            hints(view, &pairs);
            Ok(())
        })
        .await
    }
    #[cfg(feature = "coin-index")]
    async fn get_unspent_by_puzzle_hash(
        &self,
        ph: &Bytes32,
    ) -> Result<Vec<CoinRecord>, StoreError> {
        let prefix = key(PUZZLE, ph);
        self.read("unspent_by_puzzle", move |view| {
            let mut result = Vec::new();
            indexed_coins(view, &prefix, |value| {
                if value.spent_block_index == 0 {
                    result.push(value);
                }
                Ok(true)
            })?;
            Ok(result)
        })
        .await
    }
    #[cfg(feature = "coin-index")]
    async fn get_coins_by_parent(&self, parent: &Bytes32) -> Result<Vec<CoinRecord>, StoreError> {
        let prefix = key(PARENT, parent);
        self.read("coins_by_parent", move |view| {
            let mut result = Vec::new();
            indexed_coins(view, &prefix, |value| {
                result.push(value);
                Ok(true)
            })?;
            Ok(result)
        })
        .await
    }
    #[cfg(feature = "coin-index")]
    async fn get_coins_added_at_height(&self, h: u32) -> Result<Vec<CoinRecord>, StoreError> {
        self.read("coins_added", move |view| {
            let mut result = Vec::new();
            indexed_coins(view, &key(CONFIRMED, h.to_be_bytes()), |value| {
                result.push(value);
                Ok(true)
            })?;
            Ok(result)
        })
        .await
    }
    #[cfg(feature = "coin-index")]
    async fn get_coins_removed_at_height(&self, h: u32) -> Result<Vec<CoinRecord>, StoreError> {
        self.read("coins_removed", move |view| {
            let mut result = Vec::new();
            // SQLite's spent_index=0 query returns unspent coins too.
            if h == 0 {
                view.scan(&[COIN], |_, bytes| {
                    let value: CoinRecord = decode(bytes)?;
                    if value.spent_block_index == 0 {
                        result.push(value);
                    }
                    Ok(true)
                })?;
            } else {
                indexed_coins(view, &key(SPENT, h.to_be_bytes()), |value| {
                    result.push(value);
                    Ok(true)
                })?;
            }
            Ok(result)
        })
        .await
    }
    #[cfg(feature = "hint")]
    async fn get_coins_for_hint(
        &self,
        hint: &Bytes32,
        max_items: usize,
    ) -> Result<Vec<Bytes32>, StoreError> {
        let prefix = key(HINT, hint);
        self.read("hint_ids", move |view| {
            let mut result = Vec::new();
            if max_items > 0 {
                view.scan(&prefix, |k, _| {
                    result.push(hash(&k[prefix.len()..])?);
                    Ok(result.len() < max_items)
                })?;
            }
            Ok(result)
        })
        .await
    }
    #[cfg(feature = "coin-index")]
    async fn get_coin_states_by_puzzle_hashes(
        &self,
        hashes: &[Bytes32],
        min_height: u32,
        include_spent: bool,
        max_items: usize,
    ) -> Result<Vec<CoinState>, StoreError> {
        let hashes = hashes.to_vec();
        self.read("coin_states", move |view| {
            let mut result = Vec::new();
            for ph in hashes {
                if result.len() >= max_items {
                    break;
                }
                indexed_coins(view, &key(PUZZLE, ph), |value| {
                    if (value.confirmed_block_index >= min_height
                        || value.spent_block_index >= min_height)
                        && (include_spent || value.spent_block_index == 0)
                    {
                        result.push(crate::traits::coin_state_from_record(&value));
                    }
                    Ok(result.len() < max_items)
                })?;
            }
            Ok(result)
        })
        .await
    }
    #[cfg(feature = "coin-index")]
    async fn batch_coin_states_by_puzzle_hashes(
        &self,
        hashes: &[Bytes32],
        min_height: u32,
        filters: &CoinStateFilters,
        max_items: usize,
    ) -> Result<(Vec<CoinState>, Option<u32>), StoreError> {
        let hashes = hashes.to_vec();
        let filters = filters.clone();
        self.read("paged_coin_states", move |view| {
            if !filters.include_spent && !filters.include_unspent {
                return Ok((Vec::new(), None));
            }
            // V1 scans each matching prefix but retains only the smallest max_items+1 states.
            // This bounds memory even for hot hashes; an activity-height index is a later optimization.
            let cap = max_items
                .checked_add(1)
                .ok_or_else(|| StoreError::Batch("coin-state limit overflow".into()))?;
            let mut ordered = BTreeMap::new();
            let mut scanned = 0usize;
            let mut visit = |value: CoinRecord| -> Result<bool, StoreError> {
                scanned += 1;
                let activity = value.confirmed_block_index.max(value.spent_block_index);
                let spent = value.spent_block_index != 0;
                if activity >= min_height
                    && value.coin.amount >= filters.min_amount
                    && ((spent && filters.include_spent) || (!spent && filters.include_unspent))
                {
                    ordered.insert(
                        (activity, value.coin.name().bytes()),
                        crate::traits::coin_state_from_record(&value),
                    );
                    if ordered.len() > cap {
                        ordered.pop_last();
                    }
                }
                Ok(true)
            };
            for ph in hashes {
                indexed_coins(view, &key(PUZZLE, ph), &mut visit)?;
                #[cfg(feature = "hint")]
                if filters.include_hinted {
                    let prefix = key(HINT, ph);
                    view.scan(&prefix, |k, _| {
                        if let Some(value) = coin(view, &hash(&k[prefix.len()..])?)? {
                            visit(value)?;
                        }
                        Ok(true)
                    })?;
                }
            }
            let merged = ordered
                .into_values()
                .map(|cs| (cs.coin.name(), cs))
                .collect();
            let result = crate::traits::page_coin_states(merged, max_items);
            log::debug!(
                "rocksdb paged_coin_states scanned={scanned} returned={} next_height={:?}",
                result.0.len(),
                result.1
            );
            Ok(result)
        })
        .await
    }
}
