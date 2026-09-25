// Unfinished-block cache, keyed by the partial (reward-chain) hash, then by foliage hash —
// several unfinished blocks can share one proof of space with different transaction sets, and the
// "best" of a set is the one with the smallest foliage hash. Entries exist in three states:
// requested-but-not-received (`block: None`), received-but-unvalidated, and validated
// (`required_iters` present).

use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::blockchain::unfinished_block::UnfinishedBlock;
use std::collections::{HashMap, HashSet, VecDeque};

// Per-reward-hash variant cap and seen-set LRU size.
const MAX_PER_REWARD_HASH: usize = 20;
const SEEN_CAP: usize = 1_000;

pub struct UnfinishedBlockEntry {
    // None while the block is requested but not yet received.
    pub block: Option<UnfinishedBlock>,
    // The pre-validation product (`validate_unfinished_header_block`); None until validated.
    pub required_iters: Option<u64>,
    pub height: u32,
}

#[derive(Default)]
pub struct UnfinishedCache {
    // partial (reward-chain) hash → foliage hash (None = non-tx block or old-protocol announce)
    // → entry.
    blocks: HashMap<Bytes32, HashMap<Option<Bytes32>, UnfinishedBlockEntry>>,
    // Recently-seen partial hashes, insertion-ordered for LRU eviction.
    seen: HashSet<Bytes32>,
    seen_order: VecDeque<Bytes32>,
}

// The "best" of a reward-hash group: prefer entries with foliage and a received block, smallest
// foliage hash first.
fn find_best(
    group: &HashMap<Option<Bytes32>, UnfinishedBlockEntry>,
) -> (Option<Bytes32>, Option<&UnfinishedBlock>) {
    if group.len() == 1 {
        let (foliage, entry) = group.iter().next().expect("len checked");
        return match &entry.block {
            Some(b) => (*foliage, Some(b)),
            None => (None, None),
        };
    }
    group
        .iter()
        .filter_map(|(foliage, entry)| Some((((*foliage)?), entry.block.as_ref()?)))
        .min_by(|(a, _), (b, _)| <Bytes32 as AsRef<[u8]>>::as_ref(a).cmp(b.as_ref()))
        .map_or((None, None), |(foliage, block)| {
            (Some(foliage), Some(block))
        })
}

fn evict_worst(group: &mut HashMap<Option<Bytes32>, UnfinishedBlockEntry>) {
    if group.len() <= MAX_PER_REWARD_HASH {
        return;
    }
    // A None foliage hash is the worst (quality unknown); otherwise the LARGEST foliage hash.
    if group.remove(&None).is_some() {
        return;
    }
    if let Some(worst) = group
        .keys()
        .filter_map(|k| *k)
        .max_by(|a, b| <Bytes32 as AsRef<[u8]>>::as_ref(a).cmp(b.as_ref()))
    {
        group.remove(&Some(worst));
    }
}

impl UnfinishedCache {
    pub fn diagnostic_counts(&self) -> (usize, usize, usize) {
        let entries = self.blocks.values().flat_map(|group| group.values());
        let mut received = 0;
        let mut requesting = 0;
        for entry in entries {
            if entry.block.is_some() {
                received += 1;
            } else {
                requesting += 1;
            }
        }
        (received, requesting, self.seen.len())
    }

    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// True if `partial_hash` was already seen recently; marks it seen either way (the announce
    /// dedup).
    pub fn seen(&mut self, partial_hash: Bytes32) -> bool {
        if self.seen.contains(&partial_hash) {
            return true;
        }
        self.seen.insert(partial_hash);
        self.seen_order.push_back(partial_hash);
        while self.seen_order.len() > SEEN_CAP {
            if let Some(evicted) = self.seen_order.pop_front() {
                self.seen.remove(&evicted);
            }
        }
        false
    }

    /// Whether a fetch for this exact (reward hash, foliage hash) is already in flight, and how
    /// many variants of the reward hash we track (the variant count bounds how many duplicates
    /// we will pull).
    #[must_use]
    pub fn is_requesting(
        &self,
        reward_hash: &Bytes32,
        foliage_hash: Option<&Bytes32>,
    ) -> (bool, usize) {
        match self.blocks.get(reward_hash) {
            None => (false, 0),
            Some(group) => match foliage_hash {
                None => (!group.is_empty(), group.len()),
                Some(f) => (group.contains_key(&Some(*f)), group.len()),
            },
        }
    }

    pub fn mark_requesting(&mut self, reward_hash: Bytes32, foliage_hash: Option<Bytes32>) {
        let group = self.blocks.entry(reward_hash).or_default();
        group.entry(foliage_hash).or_insert(UnfinishedBlockEntry {
            block: None,
            required_iters: None,
            height: 0,
        });
        evict_worst(group);
    }

    /// Drop a requested-but-never-received placeholder; a received block is left alone.
    pub fn remove_requesting(&mut self, reward_hash: &Bytes32, foliage_hash: Option<&Bytes32>) {
        let Some(group) = self.blocks.get_mut(reward_hash) else {
            return;
        };
        let key = foliage_hash.copied();
        if group.get(&key).is_some_and(|e| e.block.is_none()) {
            group.remove(&key);
        }
        if group.is_empty() {
            self.blocks.remove(reward_hash);
        }
    }

    /// Cache a received, pre-validated unfinished block.
    pub fn add_block(
        &mut self,
        partial_hash: Bytes32,
        height: u32,
        block: UnfinishedBlock,
        required_iters: u64,
    ) {
        let foliage_hash = block.foliage.foliage_transaction_block_hash;
        let group = self.blocks.entry(partial_hash).or_default();
        group.insert(
            foliage_hash,
            UnfinishedBlockEntry {
                block: Some(block),
                required_iters: Some(required_iters),
                height,
            },
        );
        evict_worst(group);
    }

    /// The best unfinished block for a reward hash — the old-protocol lookup and the timelord's
    /// infusion-point path.
    #[must_use]
    pub fn get_block(&self, reward_hash: &Bytes32) -> Option<&UnfinishedBlock> {
        find_best(self.blocks.get(reward_hash)?).1
    }

    /// The v2 lookup: the block at (reward hash, foliage hash), the number of variants held, and
    /// whether a better variant (smaller foliage hash) exists.
    #[must_use]
    pub fn get_block2(
        &self,
        reward_hash: &Bytes32,
        foliage_hash: Option<&Bytes32>,
    ) -> (Option<&UnfinishedBlock>, usize, bool) {
        let Some(group) = self.blocks.get(reward_hash) else {
            return (None, 0, false);
        };
        let Some(f) = foliage_hash else {
            return (find_best(group).1, group.len(), false);
        };
        let (best_foliage, _) = find_best(group);
        let has_better = best_foliage
            .is_some_and(|b| <Bytes32 as AsRef<[u8]>>::as_ref(&b).cmp(f.as_ref()).is_lt());
        match group.get(&Some(*f)) {
            Some(entry) => (entry.block.as_ref(), group.len(), has_better),
            None => (None, group.len(), has_better),
        }
    }

    /// Every received unfinished block cached at exactly `height` — the read behind the
    /// `get_unfinished_block_headers` RPC (which passes the peak height).
    #[must_use]
    pub fn blocks_at_height(&self, height: u32) -> Vec<&UnfinishedBlock> {
        self.blocks
            .values()
            .flat_map(|group| group.values())
            .filter(|entry| entry.height == height)
            .filter_map(|entry| entry.block.as_ref())
            .collect()
    }

    /// Drop every cached block at or below `height` (peak advanced past them).
    pub fn prune_below(&mut self, height: u32) {
        self.blocks.retain(|_, group| {
            group.retain(|_, e| e.height > height || e.block.is_none());
            !group.is_empty()
        });
    }
}
