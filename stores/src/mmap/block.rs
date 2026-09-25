use super::{BLOCK_PAYLOAD, MmapStore};
use crate::error::StoreError;
use crate::traits::BlockStore;
use crate::types::{BatchHandle, BatchInner, BlockStatus, Savepoint};
use async_trait::async_trait;
use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::clvm::program::SerializedProgram;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use std::io::Cursor;

const VERSION: ChiaProtocolVersion = ChiaProtocolVersion::Chia0_0_37;

// Payload codec: [record_off+1:8][body_off+1:8][height:4][status:1][main:1].
pub(crate) struct BlockEntry {
    pub record_off: Option<u64>,
    pub body_off: Option<u64>,
    pub height: u32,
    pub status: u8,
    pub main: bool,
}

impl BlockEntry {
    pub(crate) fn parse(p: &[u8; BLOCK_PAYLOAD]) -> Self {
        Self {
            record_off: u64::from_le_bytes(p[..8].try_into().expect("payload")).checked_sub(1),
            body_off: u64::from_le_bytes(p[8..16].try_into().expect("payload")).checked_sub(1),
            height: u32::from_le_bytes(p[16..20].try_into().expect("payload")),
            status: p[20],
            main: p[21] != 0,
        }
    }

    pub(crate) fn pack(&self) -> [u8; BLOCK_PAYLOAD] {
        let mut out = [0u8; BLOCK_PAYLOAD];
        out[..8].copy_from_slice(&self.record_off.map_or(0, |o| o + 1).to_le_bytes());
        out[8..16].copy_from_slice(&self.body_off.map_or(0, |o| o + 1).to_le_bytes());
        out[16..20].copy_from_slice(&self.height.to_le_bytes());
        out[20] = self.status;
        out[21] = u8::from(self.main);
        out
    }
}

// Legacy-tolerant: older blobs decode via the fallback walk; see record_compat.rs.
use crate::record_compat::decode_record;

impl MmapStore {
    pub(crate) fn block_entry(
        &self,
        hh: &Bytes32,
    ) -> Result<Option<(u64, BlockEntry)>, StoreError> {
        match self.blocks_tbl.find(hh)? {
            None => Ok(None),
            Some(index) => {
                let payload = self.blocks_tbl.payload(index)?;
                Ok(Some((index, BlockEntry::parse(&payload))))
            }
        }
    }

    async fn record_at(&self, entry: &BlockEntry) -> Result<Option<BlockRecord>, StoreError> {
        match entry.record_off {
            None => Ok(None),
            Some(off) => Ok(Some(decode_record(&self.records.read(off).await?)?)),
        }
    }
}

#[async_trait]
impl BlockStore for MmapStore {
    async fn get_block_record(&self, hh: &Bytes32) -> Result<Option<BlockRecord>, StoreError> {
        match self.block_entry(hh)? {
            None => Ok(None),
            Some((_, entry)) => self.record_at(&entry).await,
        }
    }

    async fn get_block_record_by_height(&self, h: u32) -> Result<Option<BlockRecord>, StoreError> {
        match self.heights.get(h)? {
            None => Ok(None),
            Some(hh) => self.get_block_record(&hh).await,
        }
    }

    async fn get_peak(&self) -> Result<Option<(Bytes32, u32)>, StoreError> {
        Ok(*self.peak.read().await)
    }

    async fn min_record_height(&self) -> Result<Option<u32>, StoreError> {
        use std::sync::atomic::Ordering;
        const UNKNOWN: u64 = u64::MAX;
        // Fast path: the cached floor is valid while its slot is still occupied AND the slot just
        // below is still vacant (a backfill below the old floor fails the second check).
        let cached = self.min_height_cache.load(Ordering::Relaxed);
        if cached != UNKNOWN {
            #[allow(clippy::cast_possible_truncation)]
            let c = cached as u32;
            let occupied = self.heights.get(c)?.is_some();
            let below_vacant = c == 0 || self.heights.get(c - 1)?.is_none();
            if occupied && below_vacant {
                return Ok(Some(c));
            }
        }
        // (Re)scan from genesis for the first occupied slot. A genesis-synced store hits slot 0
        // immediately; an era-anchored store pays one scan, then the cache holds.
        for h in 0..self.heights.slots() {
            if self.heights.get(h)?.is_some() {
                self.min_height_cache.store(u64::from(h), Ordering::Relaxed);
                return Ok(Some(h));
            }
        }
        self.min_height_cache.store(UNKNOWN, Ordering::Relaxed);
        Ok(None)
    }

    async fn get_block(&self, hh: &Bytes32) -> Result<Option<FullBlock>, StoreError> {
        let Some((_, entry)) = self.block_entry(hh)? else {
            return Ok(None);
        };
        let Some(off) = entry.body_off else {
            return Ok(None);
        };
        let raw = zstd::decode_all(&self.bodies.read(off).await?[..])?;
        Ok(Some(FullBlock::from_bytes(
            &mut Cursor::new(&raw[..]),
            VERSION,
        )?))
    }

    async fn get_generator_at_height(
        &self,
        h: u32,
    ) -> Result<Option<SerializedProgram>, StoreError> {
        match self.heights.get(h)? {
            None => Ok(None),
            Some(hh) => Ok(self
                .get_block(&hh)
                .await?
                .and_then(|b| b.transactions_generator)),
        }
    }

    async fn add_block_records(&self, records: &[BlockRecord]) -> Result<(), StoreError> {
        let _w = self.write_lock.lock().await;
        // Two-phase sync-before-link (batched per call, matching the coin path): append every
        // record frame, ONE log fsync, then all new table links via `insert_batch` — the
        // headers-first pass hands hundreds of records per call, and the per-new-record fsync
        // was one ~100 ms iSCSI sync each.
        let mut pending: Vec<(Bytes32, [u8; BLOCK_PAYLOAD])> = Vec::new();
        let mut pending_idx: std::collections::HashMap<Bytes32, usize> =
            std::collections::HashMap::new();
        for r in records {
            let hh = r.header_hash;
            let bytes = r.to_bytes(VERSION)?;
            let off = self.records.append(&bytes).await?;
            if let Some(&slot) = pending_idx.get(&hh) {
                // Re-upsert within one call: repoint the staged entry at the fresh frame.
                let mut entry = BlockEntry::parse(&pending[slot].1);
                entry.record_off = Some(off);
                entry.height = r.height;
                pending[slot].1 = entry.pack();
                continue;
            }
            match self.blocks_tbl.find(&hh)? {
                Some(index) => {
                    let mut entry = BlockEntry::parse(&self.blocks_tbl.payload(index)?);
                    entry.record_off = Some(off);
                    entry.height = r.height;
                    let missing_body = entry.body_off.is_none();
                    self.blocks_tbl.set_payload(index, &entry.pack())?;
                    if missing_body {
                        self.unassociated.lock().await.insert(r.height);
                    }
                }
                None => {
                    let entry = BlockEntry {
                        record_off: Some(off),
                        body_off: None,
                        height: r.height,
                        status: BlockStatus::Unvalidated.as_u8(),
                        main: false,
                    };
                    let slot = pending.len();
                    pending.push((hh, entry.pack()));
                    pending_idx.insert(hh, slot);
                    self.unassociated.lock().await.insert(r.height);
                }
            }
        }
        if !pending.is_empty() {
            // libbitcoin ordering: the record log syncs before any table link lands.
            self.records.sync().await?;
            self.blocks_tbl.insert_batch(&pending)?;
        }
        self.records.sync().await?;
        self.blocks_tbl.sync()
    }

    async fn add_block_records_in(
        &self,
        batch: &mut BatchHandle,
        records: &[BlockRecord],
    ) -> Result<(), StoreError> {
        // mmap writes are immediately visible and ordered (sync-before-link); the batch only
        // scopes the durability point, so the direct write body applies unchanged.
        batch.require_mmap()?;
        self.add_block_records(records).await
    }

    async fn begin(&self) -> Result<BatchHandle, StoreError> {
        Ok(BatchHandle {
            inner: BatchInner::Mmap(crate::types::MmapBatch::default()),
            _timing: None,
        })
    }

    async fn append_many(
        &self,
        batch: &mut BatchHandle,
        blocks: &[FullBlock],
    ) -> Result<(), StoreError> {
        batch.require_mmap()?;
        let _w = self.write_lock.lock().await;
        // Same two-phase shape as add_block_records: the body-before-record arrivals (rare) are
        // staged and linked behind ONE log fsync instead of one per body.
        let mut pending: Vec<(Bytes32, [u8; BLOCK_PAYLOAD])> = Vec::new();
        for block in blocks {
            let hh = block.header_hash()?;
            let body = zstd::encode_all(&block.to_bytes(VERSION)?[..], 3)?;
            let off = self.bodies.append(&body).await?;
            match self.blocks_tbl.find(&hh)? {
                Some(index) => {
                    let mut entry = BlockEntry::parse(&self.blocks_tbl.payload(index)?);
                    entry.body_off = Some(off);
                    let height = entry.height;
                    self.blocks_tbl.set_payload(index, &entry.pack())?;
                    self.unassociated.lock().await.remove(&height);
                }
                None => {
                    let entry = BlockEntry {
                        record_off: None,
                        body_off: Some(off),
                        height: block.reward_chain_block.height,
                        status: BlockStatus::Unvalidated.as_u8(),
                        main: false,
                    };
                    pending.push((hh, entry.pack()));
                }
            }
        }
        if !pending.is_empty() {
            // libbitcoin ordering: the body log syncs before any table link lands.
            self.bodies.sync().await?;
            self.blocks_tbl.insert_batch(&pending)?;
        }
        Ok(())
    }

    async fn commit(&self, mut batch: BatchHandle) -> Result<(), StoreError> {
        batch.require_mmap()?;
        // Coin mutations staged under the batch land here when no set_peak_in drained them earlier
        // (a peak-less batch: the write-through download path, or a confirm that stopped short).
        self.flush_coin_batch(&mut batch).await?;
        // One durability point for everything appended under the batch.
        self.bodies.sync().await?;
        self.blocks_tbl.sync()?;
        // A reorg batch's journal normally clears in set_peak_in (after the peak meta write);
        // this is the belt for a sweep-carrying batch that never flipped the peak.
        self.clear_reorg_journal()
    }

    async fn get_unassociated(&self, limit: usize) -> Result<Vec<u32>, StoreError> {
        Ok(self
            .unassociated
            .lock()
            .await
            .iter()
            .take(limit)
            .copied()
            .collect())
    }

    async fn set_peak(&self, new_peak: &Bytes32) -> Result<u64, StoreError> {
        let _w = self.write_lock.lock().await;
        let Some((_, peak_entry)) = self.block_entry(new_peak)? else {
            return Err(StoreError::Corrupt(
                "set_peak: unknown header hash".to_string(),
            ));
        };
        // Fork point: walk the new branch's ancestry (all off-main until the fork) to the deepest
        // ancestor already on the main chain; -1 = no shared ancestor (full replace).
        let mut fork_height = -1i64;
        let mut cursor = *new_peak;
        loop {
            let Some((_, entry)) = self.block_entry(&cursor)? else {
                break;
            };
            if entry.main {
                fork_height = i64::from(entry.height);
                break;
            }
            let Some(rec) = self.record_at(&entry).await? else {
                break;
            };
            cursor = rec.prev_hash;
        }
        // Retire the whole old main branch above the fork (not just above the new peak), so a
        // same-height reorg cannot leave an abandoned sibling flagged.
        let old_top = self.peak.read().await.map_or(0, |(_, h)| h);
        let mut h = u32::try_from(fork_height + 1).unwrap_or(0);
        while h <= old_top {
            if let Some(hh) = self.heights.get(h)? {
                if let Some((index, mut entry)) = self.block_entry(&hh)? {
                    entry.main = false;
                    self.blocks_tbl.set_payload(index, &entry.pack())?;
                }
                self.heights.set(h, None)?;
            }
            h += 1;
        }
        // Link the new ancestry onto the main chain, back to (but not including) the fork ancestor.
        let mut links = 0u64;
        let mut cursor = *new_peak;
        loop {
            let Some((index, mut entry)) = self.block_entry(&cursor)? else {
                break;
            };
            if entry.main {
                break;
            }
            entry.main = true;
            self.blocks_tbl.set_payload(index, &entry.pack())?;
            self.heights.set(entry.height, Some(&cursor))?;
            links += 1;
            let Some(rec) = self.record_at(&entry).await? else {
                break;
            };
            cursor = rec.prev_hash;
        }
        *self.peak.write().await = Some((*new_peak, peak_entry.height));
        // Durability ordering: table + heights before the peak pointer.
        self.blocks_tbl.sync()?;
        self.heights.sync()?;
        self.write_meta().await?;
        Ok(links)
    }

    async fn set_peak_in(
        &self,
        batch: &mut BatchHandle,
        new_peak: &Bytes32,
    ) -> Result<u64, StoreError> {
        // Durability ordering: the batch's staged coin mutations (and their syncs) land BEFORE
        // the peak walk, whose meta write is always last — a torn shutdown can lose the peak
        // advance, never leave a durable peak over unlinked coins. For a reorg batch the flush
        // arms the journal before its first published mutation; once the peak meta is durable the
        // reorg is complete and the journal comes off (a crash in between converges at open).
        self.flush_coin_batch(batch).await?;
        let links = self.set_peak(new_peak).await?;
        self.clear_reorg_journal()?;
        Ok(links)
    }

    async fn get_status(&self, hh: &Bytes32) -> Result<BlockStatus, StoreError> {
        Ok(self
            .block_entry(hh)?
            .map_or(BlockStatus::Unvalidated, |(_, e)| {
                BlockStatus::from_u8(e.status)
            }))
    }

    async fn set_status(&self, hh: &Bytes32, s: BlockStatus) -> Result<(), StoreError> {
        let _w = self.write_lock.lock().await;
        let Some((index, mut entry)) = self.block_entry(hh)? else {
            return Err(StoreError::Corrupt(
                "set_status: unknown header hash".to_string(),
            ));
        };
        entry.status = s.as_u8();
        self.blocks_tbl.set_payload(index, &entry.pack())
    }

    async fn set_status_in(
        &self,
        batch: &mut BatchHandle,
        hh: &Bytes32,
        s: BlockStatus,
    ) -> Result<(), StoreError> {
        batch.require_mmap()?;
        self.set_status(hh, s).await
    }

    async fn savepoint(&self) -> Result<Savepoint, StoreError> {
        Ok(Savepoint {
            peak: *self.peak.read().await,
        })
    }

    async fn rollback(&self, sp: Savepoint) -> Result<u64, StoreError> {
        let _w = self.write_lock.lock().await;
        let floor = sp.peak.map_or(-1i64, |(_, h)| i64::from(h));
        let old_top = self.peak.read().await.map_or(0, |(_, h)| h);
        let mut touched = 0u64;
        let mut h = u32::try_from(floor + 1).unwrap_or(0);
        while h <= old_top {
            if let Some(hh) = self.heights.get(h)? {
                if let Some((index, mut entry)) = self.block_entry(&hh)? {
                    entry.main = false;
                    self.blocks_tbl.set_payload(index, &entry.pack())?;
                    touched += 1;
                }
                self.heights.set(h, None)?;
            }
            h += 1;
        }
        *self.peak.write().await = sp.peak;
        self.blocks_tbl.sync()?;
        self.heights.sync()?;
        self.write_meta().await?;
        Ok(touched)
    }

    async fn get_sub_epoch_segments(
        &self,
        ses_hash: &Bytes32,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        // Table lookup → frame read; the bytes stay opaque SubEpochSegments, no decode here.
        let Some(index) = self.segments_tbl.find(ses_hash)? else {
            return Ok(None);
        };
        let payload = self.segments_tbl.payload(index)?;
        let Some(off) = u64::from_le_bytes(payload).checked_sub(1) else {
            return Ok(None);
        };
        Ok(Some(self.segments.read(off).await?))
    }

    async fn persist_sub_epoch_segments(
        &self,
        ses_hash: &Bytes32,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        // Frame appended and synced before the table link lands, so a torn shutdown loses the
        // segments (rebuildable), never the table. Re-persist repoints the existing entry and the
        // old frame becomes unreferenced — this store is append-only, so an upsert cannot
        // overwrite in place.
        let _w = self.write_lock.lock().await;
        let off = self.segments.append(bytes).await?;
        self.segments.sync().await?;
        let payload = (off + 1).to_le_bytes();
        match self.segments_tbl.find(ses_hash)? {
            Some(index) => self.segments_tbl.set_payload(index, &payload)?,
            None => {
                self.segments_tbl.insert(ses_hash, &payload, true)?;
            }
        }
        self.segments_tbl.sync()
    }

    async fn build_indexes(&self) -> Result<(), StoreError> {
        // The tables ARE the indexes; nothing deferred to build.
        Ok(())
    }
}
