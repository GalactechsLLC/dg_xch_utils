use super::*;
use crate::{BlockStatus, BlockStore, types::Savepoint};
use async_trait::async_trait;
use dg_xch_core::blockchain::{block_record::BlockRecord, full_block::FullBlock};
use dg_xch_core::clvm::program::SerializedProgram;
use std::collections::HashSet;

fn record(view: &View<'_>, id: &Bytes32) -> Result<Option<BlockRecord>, StoreError> {
    view.get(&key(RECORD, id))?
        .map(|bytes| crate::record_compat::decode_record(&bytes))
        .transpose()
}

fn peak(view: &View<'_>) -> Result<Option<(Bytes32, u32)>, StoreError> {
    view.get(PEAK)?
        .map(|bytes| {
            if bytes.len() != 36 {
                return Err(StoreError::Corrupt("invalid rocksdb peak".into()));
            }
            Ok((hash(&bytes[..32])?, height(&bytes[32..])?))
        })
        .transpose()
}

fn put_peak(view: &mut View<'_>, id: &Bytes32, h: u32) {
    let mut bytes = id.bytes().to_vec();
    bytes.extend_from_slice(&h.to_be_bytes());
    view.put(PEAK.to_vec(), bytes);
}

fn write_records(view: &mut View<'_>, records: &[BlockRecord]) -> Result<(), StoreError> {
    for r in records {
        if let Some(old) = record(view, &r.header_hash)?
            && old.height != r.height
        {
            view.delete(index(MISSING, old.height.to_be_bytes(), &r.header_hash));
            if view.get(&key(MAIN, old.height.to_be_bytes()))?.as_deref()
                == Some(r.header_hash.as_ref())
            {
                view.delete(key(MAIN, old.height.to_be_bytes()));
                view.put(
                    key(MAIN, r.height.to_be_bytes()),
                    r.header_hash.bytes().to_vec(),
                );
            }
        }
        view.put(key(RECORD, r.header_hash), r.to_bytes(VERSION)?);
        if view.get(&key(BODY, r.header_hash))?.is_none() {
            view.put(
                index(MISSING, r.height.to_be_bytes(), &r.header_hash),
                Vec::new(),
            );
        }
    }
    Ok(())
}

fn set_status(view: &mut View<'_>, id: &Bytes32, status: BlockStatus) -> Result<(), StoreError> {
    if view.get(&key(RECORD, id))?.is_some() {
        view.put(key(STATUS, id), vec![status.as_u8()]);
    }
    Ok(())
}

fn retire(view: &mut View<'_>, floor: Option<u32>) -> Result<u64, StoreError> {
    let mut keys = Vec::new();
    let start = match floor {
        Some(floor) => match floor.checked_add(1) {
            Some(first) => key(MAIN, first.to_be_bytes()),
            None => return Ok(0),
        },
        None => vec![MAIN],
    };
    view.scan_from(&[MAIN], &start, |k, _| {
        height(&k[1..])?;
        keys.push(k.to_vec());
        Ok(true)
    })?;
    let count = keys.len() as u64;
    for key in keys {
        view.delete(key);
    }
    Ok(count)
}

fn set_peak(view: &mut View<'_>, id: &Bytes32) -> Result<u64, StoreError> {
    let tip = record(view, id)?
        .ok_or_else(|| StoreError::Corrupt("set_peak: unknown header hash".into()))?;
    let mut branch = Vec::new();
    let mut seen = HashSet::new();
    let mut cursor = *id;
    let fork = loop {
        if !seen.insert(cursor) {
            return Err(StoreError::Corrupt(
                "cycle in rocksdb block ancestry".into(),
            ));
        }
        let Some(r) = record(view, &cursor)? else {
            break None;
        };
        if view.get(&key(MAIN, r.height.to_be_bytes()))?.as_deref() == Some(cursor.as_ref()) {
            break Some(r.height);
        }
        branch.push((r.height, cursor));
        cursor = r.prev_hash;
    };
    retire(view, fork)?;
    let links = branch.len() as u64;
    for (h, hh) in branch {
        view.put(key(MAIN, h.to_be_bytes()), hh.bytes().to_vec());
    }
    put_peak(view, id, tip.height);
    Ok(links)
}

fn body(view: &View<'_>, id: &Bytes32) -> Result<Option<FullBlock>, StoreError> {
    view.get(&key(BODY, id))?
        .map(|bytes| decode(&zstd::decode_all(&bytes[..])?))
        .transpose()
}

#[async_trait]
impl BlockStore for RocksDbStore {
    async fn get_block_record(&self, hh: &Bytes32) -> Result<Option<BlockRecord>, StoreError> {
        let hh = *hh;
        self.read("block_record", move |view| record(view, &hh))
            .await
    }
    async fn get_block_records_by_hash(
        &self,
        hashes: &[Bytes32],
    ) -> Result<Vec<BlockRecord>, StoreError> {
        let hashes = hashes.to_vec();
        self.read("block_records", move |view| {
            let mut result = Vec::with_capacity(hashes.len());
            for hh in hashes {
                if let Some(r) = record(view, &hh)? {
                    result.push(r);
                }
            }
            Ok(result)
        })
        .await
    }
    async fn get_block_record_by_height(&self, h: u32) -> Result<Option<BlockRecord>, StoreError> {
        self.read("record_by_height", move |view| {
            match view.get(&key(MAIN, h.to_be_bytes()))? {
                Some(id) => record(view, &hash(&id)?),
                None => Ok(None),
            }
        })
        .await
    }
    async fn get_peak(&self) -> Result<Option<(Bytes32, u32)>, StoreError> {
        self.read("peak", peak).await
    }
    async fn min_record_height(&self) -> Result<Option<u32>, StoreError> {
        self.read("min_record_height", |view| {
            let mut result = None;
            view.scan(&[MAIN], |k, _| {
                result = Some(height(&k[1..])?);
                Ok(false)
            })?;
            Ok(result)
        })
        .await
    }
    async fn get_block(&self, hh: &Bytes32) -> Result<Option<FullBlock>, StoreError> {
        let hh = *hh;
        self.read("block_body", move |view| body(view, &hh)).await
    }
    async fn get_generator_at_height(
        &self,
        h: u32,
    ) -> Result<Option<SerializedProgram>, StoreError> {
        self.read("generator", move |view| {
            let Some(id) = view.get(&key(MAIN, h.to_be_bytes()))? else {
                return Ok(None);
            };
            Ok(body(view, &hash(&id)?)?.and_then(|block| block.transactions_generator))
        })
        .await
    }
    async fn add_block_records(&self, records: &[BlockRecord]) -> Result<(), StoreError> {
        let mut batch = self.begin().await?;
        self.add_block_records_in(&mut batch, records).await?;
        self.commit(batch).await
    }
    async fn add_block_records_in(
        &self,
        batch: &mut BatchHandle,
        records: &[BlockRecord],
    ) -> Result<(), StoreError> {
        let records = records.to_vec();
        self.mutate(batch, "records", move |view| {
            log::info!("rocksdb records count={}", records.len());
            write_records(view, &records)
        })
        .await
    }
    async fn begin(&self) -> Result<BatchHandle, StoreError> {
        let started = Instant::now();
        let guard = self.writer.clone().lock_owned().await;
        log::info!(
            "rocksdb begin writer_wait_us={}",
            started.elapsed().as_micros()
        );
        Ok(BatchHandle {
            inner: crate::types::BatchInner::Rocksdb(Arc::new(RocksBatch {
                db: self.db.clone(),
                state: Mutex::new(BatchState::default()),
                options: self.options.clone(),
                in_flight: AtomicUsize::new(0),
                _writer: guard,
                started: Instant::now(),
            })),
            _timing: None,
        })
    }
    async fn append_many(
        &self,
        batch: &mut BatchHandle,
        blocks: &[FullBlock],
    ) -> Result<(), StoreError> {
        let blocks = blocks.to_vec();
        self.mutate(batch, "bodies", move |view| {
            let mut bytes = 0;
            for block in &blocks {
                let id = block.header_hash()?;
                let r = record(view, &id)?
                    .ok_or_else(|| StoreError::Batch("body has no block record".into()))?;
                let encoded = zstd::encode_all(&block.to_bytes(VERSION)?[..], 3)?;
                bytes += encoded.len();
                view.put(key(BODY, id), encoded);
                view.delete(index(MISSING, r.height.to_be_bytes(), &id));
            }
            log::info!(
                "rocksdb bodies count={} compressed_bytes={bytes}",
                blocks.len()
            );
            Ok(())
        })
        .await
    }
    async fn commit(&self, batch: BatchHandle) -> Result<(), StoreError> {
        let state = self.batch(&batch)?;
        let permit = self
            .workers
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| StoreError::Batch(e.to_string()))?;
        let near_tip = self.near_tip();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            // Both the handle and state retain the writer until the blocking commit finishes.
            let _handle = batch;
            let mut pending = state.state.lock().map_err(|_| StoreError::Batch("rocksdb batch mutex poisoned".into()))?;
            if pending.failed || state.in_flight.load(Ordering::SeqCst) != 0 { return Err(StoreError::Batch("cannot commit failed rocksdb batch".into())); }
            let mut writes = WriteBatch::default();
            for (k, value) in &pending.pending {
                match value { Some(value) => writes.put(k, value), None => writes.delete(k) }
            }
            let bytes = writes.size_in_bytes();
            let started = Instant::now();
            let mut options = WriteOptions::default();
            options.set_sync(true);
            let result = state.db.write_opt(writes, &options).map_err(StoreError::from);
            log::info!("rocksdb commit near_tip={near_tip} keys={} batch_bytes={bytes} commit_us={} writer_hold_us={} success={}", pending.pending.len(), started.elapsed().as_micros(), state.started.elapsed().as_micros(), result.is_ok());
            if result.is_ok() { pending.finished = true; }
            Self::log_properties(&state.db, &state.options);
            result
        }).await.map_err(|e| StoreError::Batch(format!("rocksdb commit worker: {e}")))?
    }
    fn near_tip(&self) -> bool {
        self.near_tip.load(Ordering::Relaxed)
    }
    fn set_near_tip(&self, near_tip: bool) {
        if self.near_tip.swap(near_tip, Ordering::Relaxed) != near_tip {
            log::info!("rocksdb phase near_tip={near_tip}");
        }
    }
    async fn get_unassociated(&self, limit: usize) -> Result<Vec<u32>, StoreError> {
        self.read("unassociated", move |view| {
            let mut result = Vec::new();
            if limit > 0 {
                view.scan(&[MISSING], |k, _| {
                    if k.len() != 37 {
                        return Err(StoreError::Corrupt("invalid missing-body index".into()));
                    }
                    result.push(height(&k[1..5])?);
                    Ok(result.len() < limit)
                })?;
            }
            Ok(result)
        })
        .await
    }
    async fn set_peak(&self, hh: &Bytes32) -> Result<u64, StoreError> {
        let mut batch = self.begin().await?;
        let touched = self.set_peak_in(&mut batch, hh).await?;
        self.commit(batch).await?;
        Ok(touched)
    }
    async fn set_peak_in(&self, batch: &mut BatchHandle, hh: &Bytes32) -> Result<u64, StoreError> {
        let hh = *hh;
        self.mutate(batch, "set_peak", move |view| {
            let touched = set_peak(view, &hh)?;
            log::info!("rocksdb set_peak links={touched}");
            Ok(touched)
        })
        .await
    }
    async fn extend_peak_in(
        &self,
        batch: &mut BatchHandle,
        extension: &[Bytes32],
        new_height: u32,
    ) -> Result<u64, StoreError> {
        let extension = extension.to_vec();
        self.mutate(batch, "extend_peak", move |view| {
            let Some(last) = extension.last() else {
                return Ok(0);
            };
            for hh in &extension {
                let r = record(view, hh)?
                    .ok_or_else(|| StoreError::Corrupt("extend_peak: missing record".into()))?;
                view.put(key(MAIN, r.height.to_be_bytes()), hh.bytes().to_vec());
            }
            put_peak(view, last, new_height);
            Ok(extension.len() as u64)
        })
        .await
    }
    async fn get_status(&self, hh: &Bytes32) -> Result<BlockStatus, StoreError> {
        let hh = *hh;
        self.read("status", move |view| match view.get(&key(STATUS, hh))? {
            Some(v) if v.len() == 1 => Ok(BlockStatus::from_u8(v[0])),
            Some(_) => Err(StoreError::Corrupt("invalid rocksdb status".into())),
            None => Ok(BlockStatus::Unvalidated),
        })
        .await
    }
    async fn set_status(&self, hh: &Bytes32, status: BlockStatus) -> Result<(), StoreError> {
        let mut batch = self.begin().await?;
        self.set_status_in(&mut batch, hh, status).await?;
        self.commit(batch).await
    }
    async fn set_status_in(
        &self,
        batch: &mut BatchHandle,
        hh: &Bytes32,
        status: BlockStatus,
    ) -> Result<(), StoreError> {
        let hh = *hh;
        self.mutate(batch, "status", move |view| set_status(view, &hh, status))
            .await
    }
    async fn set_status_many_in(
        &self,
        batch: &mut BatchHandle,
        hashes: &[Bytes32],
        status: BlockStatus,
    ) -> Result<(), StoreError> {
        let hashes = hashes.to_vec();
        self.mutate(batch, "statuses", move |view| {
            for hh in &hashes {
                set_status(view, hh, status)?;
            }
            Ok(())
        })
        .await
    }
    async fn savepoint(&self) -> Result<Savepoint, StoreError> {
        Ok(Savepoint {
            peak: self.get_peak().await?,
        })
    }
    async fn rollback(&self, sp: Savepoint) -> Result<u64, StoreError> {
        let batch = self.begin().await?;
        let count = self
            .mutate(&batch, "rollback_peak", move |view| {
                let count = retire(view, sp.peak.map(|(_, h)| h))?;
                match sp.peak {
                    Some((hh, h)) => put_peak(view, &hh, h),
                    None => view.delete(PEAK.to_vec()),
                }
                Ok(count)
            })
            .await?;
        self.commit(batch).await?;
        Ok(count)
    }
    async fn get_sub_epoch_segments(&self, hh: &Bytes32) -> Result<Option<Vec<u8>>, StoreError> {
        let hh = *hh;
        self.read("segments", move |view| view.get(&key(SEGMENTS, hh)))
            .await
    }
    async fn persist_sub_epoch_segments(
        &self,
        hh: &Bytes32,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        let hh = *hh;
        let bytes = bytes.to_vec();
        let batch = self.begin().await?;
        self.mutate(&batch, "segments", move |view| {
            view.put(key(SEGMENTS, hh), bytes);
            Ok(())
        })
        .await?;
        self.commit(batch).await
    }
    async fn build_indexes(&self) -> Result<(), StoreError> {
        log::info!("rocksdb build_indexes policy=always_live");
        Ok(())
    }
    async fn shed_service_indexes(&self) -> Result<(), StoreError> {
        log::info!("rocksdb shed_service_indexes policy=always_live");
        Ok(())
    }
}
