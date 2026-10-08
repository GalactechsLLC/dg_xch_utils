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

fn coin_indexes(record: &CoinRecord) -> Vec<Vec<u8>> {
    let id = record.coin.name();
    let mut keys = vec![index(
        CONFIRMED,
        record.confirmed_block_index.to_be_bytes(),
        &id,
    )];
    if record.spent_block_index > 0 {
        keys.push(index(SPENT, record.spent_block_index.to_be_bytes(), &id));
    }
    #[cfg(feature = "coin-index")]
    {
        keys.push(index(PUZZLE, record.coin.puzzle_hash, &id));
        keys.push(index(PARENT, record.coin.parent_coin_info, &id));
    }
    keys
}

fn write_coin(
    view: &mut View<'_>,
    id: &Bytes32,
    value: Option<CoinRecord>,
) -> Result<(), StoreError> {
    if let Some(old) = coin(view, id)? {
        for k in coin_indexes(&old) {
            view.delete(k);
        }
    }
    match value {
        Some(record) => {
            for k in coin_indexes(&record) {
                view.put(k, Vec::new());
            }
            view.put(key(COIN, id), record.to_bytes(VERSION)?);
        }
        None => view.delete(key(COIN, id)),
    }
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
        if let Some(mut value) = coin(view, id)? {
            value.spent_block_index = h;
            value.spent = h != 0;
            write_coin(view, id, Some(value))?;
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
    async fn get_coin_record(&self, name: &Bytes32) -> Result<Option<CoinRecord>, StoreError> {
        let name = *name;
        self.read("coin", move |view| coin(view, &name)).await
    }
    async fn get_coin_records(&self, names: &[Bytes32]) -> Result<Vec<CoinRecord>, StoreError> {
        let names = names.to_vec();
        self.read("coins", move |view| {
            let mut result = Vec::with_capacity(names.len());
            for id in &names {
                if let Some(record) = coin(view, id)? {
                    result.push(record);
                }
            }
            log::info!(
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
            log::info!(
                "rocksdb coin_deltas height={h} additions={} removals={}",
                additions.len(),
                removals.len()
            );
            apply(view, h, timestamp, &additions, &removals)
        })
        .await
    }
    async fn apply_coin_window_in(
        &self,
        batch: &mut BatchHandle,
        changes: &[crate::types::CoinChanges<'_>],
    ) -> Result<(), StoreError> {
        let changes: Vec<_> = changes
            .iter()
            .map(|c| crate::types::OwnedCoinChanges {
                height: c.height,
                timestamp: c.timestamp,
                additions: c.additions.to_vec(),
                removals: c.removals.to_vec(),
                hints: c.hints.to_vec(),
            })
            .collect();
        self.mutate(batch, "coin_window", move |view| {
            let mut additions = 0;
            let mut removals = 0;
            let mut hint_count = 0;
            for c in &changes {
                apply(view, c.height, c.timestamp, &c.additions, &c.removals)?;
                hints(view, &c.hints);
                additions += c.additions.len(); removals += c.removals.len(); hint_count += c.hints.len();
            }
            log::info!("rocksdb coin_window blocks={} additions={additions} removals={removals} hints={hint_count}", changes.len());
            Ok(())
        }).await
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
            log::info!(
                "rocksdb paged_coin_states scanned={scanned} returned={} next_height={:?}",
                result.0.len(),
                result.1
            );
            Ok(result)
        })
        .await
    }
}
