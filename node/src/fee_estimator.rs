// Fee estimation — a bitcoin-core-derived bucketed estimator. Provides the `get_fee_estimate`
// RPC + `RequestFeeEstimates` wallet handler.
//
// Model:
//   - Transactions are bucketed by fee-per-cost (× 1000), buckets growing geometrically by
//     STEP_SIZE from INITIAL_STEP to INFINITE_FEE_RATE (`init_buckets`).
//   - Three horizons (short/medium/long) each keep exponentially-decayed moving averages, over
//     block history, of: tx count per bucket, count confirmed-within-Y-periods per bucket,
//     count that left the mempool without confirming per bucket, and summed fee-rate per bucket
//     (`FeeStat`).
//   - `new_block` (process_block) decays the averages then records, for every mempool item the
//     block confirmed, how many blocks it waited — the positive signal.
//   - `add_mempool_item`/`remove_mempool_item` track the still-in-mempool population per bucket.
//   - A query walks buckets from the top, accumulating until a bucket band clears SUCCESS_PCT
//     with enough data, and reports that band's median fee-rate (`estimate_median_val`).
//
// The numeric conversion is median → mojos_per_clvm_cost via two /1000 steps (see
// `estimate_rate`). Empty/insufficient history yields rate 0.0 (the floor); a mempool under
// sustained pressure converges on the prevailing fee-per-cost.

const INITIAL_STEP: f64 = 5.0; // first bucket above zero
const MAX_FEE_RATE: f64 = 40_000_000.0; // mojo per 1000 cost unit
const INFINITE_FEE_RATE: f64 = 1_000_000_000.0;
const STEP_SIZE: f64 = 1.05; // buckets grow 5% each

const SHORT_BLOCK_PERIOD: usize = 12;
const SHORT_SCALE: usize = 1;
const MED_BLOCK_PERIOD: usize = 24;
const MED_SCALE: usize = 2;
const LONG_BLOCK_PERIOD: usize = 42;
const LONG_SCALE: usize = 24;

const SECONDS_PER_BLOCK: u64 = 40;

const SHORT_DECAY: f64 = 0.962;
const MED_DECAY: f64 = 0.9952;
const LONG_DECAY: f64 = 0.999_31;

const SUCCESS_PCT: f64 = 0.85; // require 85% within-target confirmations
const SUFFICIENT_FEE_TXS: f64 = 0.01; // avg txs/block per bucket needed for significance

/// One bucket band's accumulated stats during a median query; only `start` reaches the parse
/// fallback.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Default)]
struct BucketResult {
    start: f64,
    end: f64,
    within_target: f64,
    total_confirmed: f64,
    in_mempool: f64,
    left_mempool: f64,
}

/// The outcome of a median query.
#[derive(Clone, Copy, Debug)]
struct EstimateResult {
    #[allow(dead_code)] // the wire path re-derives it
    requested_time: u64,
    fail_bucket: BucketResult,
    median: f64,
}

fn clamp(n: i64, smallest: i64, largest: i64) -> i64 {
    smallest.max(n.min(largest))
}

/// The bucket a fee-rate falls into — the bucket to the left unless the rate matches exactly
/// (`bisect_left(buckets, fee_rate) - 1`, clamped).
#[must_use]
fn get_bucket_index(buckets: &[f64], fee_rate: f64) -> usize {
    debug_assert!(!buckets.is_empty(), "buckets must be non-empty");
    // bisect_left: first index i with buckets[i] >= fee_rate.
    let bisect_left = buckets.partition_point(|&b| b < fee_rate);
    let idx = clamp(bisect_left as i64 - 1, 0, buckets.len() as i64 - 1);
    idx as usize
}

/// The geometric bucket ladder: INITIAL_STEP, ×STEP_SIZE up to MAX_FEE_RATE, capped by
/// INFINITE_FEE_RATE.
#[must_use]
fn init_buckets() -> Vec<f64> {
    let mut buckets = Vec::new();
    let mut fee_rate = INITIAL_STEP;
    while fee_rate < MAX_FEE_RATE {
        buckets.push(fee_rate);
        fee_rate *= STEP_SIZE;
    }
    buckets.push(INFINITE_FEE_RATE);
    buckets
}

// The per-item info the tracker ingests: cost, fee, and the height the item entered the mempool
// (the reference for "blocks waited").
#[derive(Clone, Copy, Debug)]
struct ItemInfo {
    cost: u64,
    fee: u64,
    height_added: u32,
}

impl ItemInfo {
    #[allow(clippy::cast_precision_loss)]
    fn fee_per_cost(self) -> f64 {
        if self.cost == 0 {
            0.0
        } else {
            self.fee as f64 / self.cost as f64
        }
    }
}

// The decayed bucketed confirmation history for one horizon.
struct FeeStat {
    buckets: Vec<f64>,
    tx_ct_avg: Vec<f64>,
    confirmed_average: Vec<Vec<f64>>, // [period][bucket]
    failed_average: Vec<Vec<f64>>,    // [period][bucket]
    m_fee_rate_avg: Vec<f64>,
    decay: f64,
    scale: usize,
    unconfirmed_txs: Vec<Vec<u64>>, // [block_index % max_confirms][bucket]
    old_unconfirmed_txs: Vec<u64>,
    max_confirms: usize,
}

impl FeeStat {
    fn new(buckets: Vec<f64>, max_periods: usize, decay: f64, scale: usize) -> Self {
        let n = buckets.len();
        let max_confirms = scale * max_periods;
        FeeStat {
            confirmed_average: vec![vec![0.0; n]; max_periods],
            failed_average: vec![vec![0.0; n]; max_periods],
            tx_ct_avg: vec![0.0; n],
            m_fee_rate_avg: vec![0.0; n],
            unconfirmed_txs: vec![vec![0u64; n]; max_confirms],
            old_unconfirmed_txs: vec![0u64; n],
            decay,
            scale,
            max_confirms,
            buckets,
        }
    }

    // Record a tx that confirmed `blocks_to_confirm` blocks after entry.
    fn tx_confirmed(&mut self, blocks_to_confirm: usize, item: ItemInfo) {
        debug_assert!(blocks_to_confirm >= 1);
        let periods_to_confirm = blocks_to_confirm.div_ceil(self.scale);
        let fee_rate = item.fee_per_cost() * 1000.0;
        let bucket_index = get_bucket_index(&self.buckets, fee_rate);
        for i in periods_to_confirm..self.confirmed_average.len() {
            self.confirmed_average[i - 1][bucket_index] += 1.0;
        }
        self.tx_ct_avg[bucket_index] += 1.0;
        self.m_fee_rate_avg[bucket_index] += fee_rate;
    }

    // Decay every average toward zero.
    fn update_moving_averages(&mut self) {
        for j in 0..self.buckets.len() {
            for i in 0..self.confirmed_average.len() {
                self.confirmed_average[i][j] *= self.decay;
                self.failed_average[i][j] *= self.decay;
            }
            self.tx_ct_avg[j] *= self.decay;
            self.m_fee_rate_avg[j] *= self.decay;
        }
    }

    // Retire the block-index row that's about to be reused, folding its outstanding unconfirmed
    // counts into old_unconfirmed_txs.
    fn clear_current(&mut self, block_height: u32) {
        let row = block_height as usize % self.unconfirmed_txs.len();
        for i in 0..self.buckets.len() {
            self.old_unconfirmed_txs[i] += self.unconfirmed_txs[row][i];
            self.unconfirmed_txs[row][i] = 0;
        }
    }

    // A new mempool tx joins the unconfirmed population at its bucket.
    fn new_mempool_tx(&mut self, block_height: u32, fee_rate: f64) -> usize {
        let bucket_index = get_bucket_index(&self.buckets, fee_rate);
        let row = block_height as usize % self.unconfirmed_txs.len();
        self.unconfirmed_txs[row][bucket_index] += 1;
        bucket_index
    }

    // The count of mempool transactions this horizon is currently tracking as unconfirmed
    // (current-window rows + aged-out). Observability for the add/remove signal.
    fn outstanding(&self) -> u64 {
        let cur: u64 = self.unconfirmed_txs.iter().flatten().sum();
        let old: u64 = self.old_unconfirmed_txs.iter().sum();
        cur + old
    }

    // A tx left the mempool without confirming (evicted/expired/replaced) — decrement its
    // unconfirmed slot and, if it waited past a period, credit failed_average.
    #[allow(clippy::cast_precision_loss)]
    fn remove_tx(&mut self, latest_seen_height: u32, item: ItemInfo, bucket_index: usize) {
        let block_ago = if latest_seen_height == 0 {
            0i64
        } else {
            i64::from(latest_seen_height) - i64::from(item.height_added)
        };
        if block_ago < 0 {
            return;
        }
        let bins = self.unconfirmed_txs.len() as i64;
        if block_ago >= bins {
            if self.old_unconfirmed_txs[bucket_index] > 0 {
                self.old_unconfirmed_txs[bucket_index] -= 1;
            }
        } else {
            let row = item.height_added as usize % self.unconfirmed_txs.len();
            if self.unconfirmed_txs[row][bucket_index] > 0 {
                self.unconfirmed_txs[row][bucket_index] -= 1;
            }
        }
        if block_ago as usize >= self.scale {
            let periods_ago = block_ago as f64 / self.scale as f64;
            for i in 0..self.failed_average.len() {
                if i as f64 >= periods_ago {
                    break;
                }
                self.failed_average[i][bucket_index] += 1.0;
            }
        }
    }

    // Walk buckets from the top, accumulating until a band clears `success_break_point` with
    // enough data, then report that band's median fee-rate. `median == -1.0` means no passing band.
    #[allow(clippy::too_many_lines)]
    #[allow(clippy::cast_precision_loss)]
    fn estimate_median_val(
        &self,
        conf_target: usize,
        sufficient_tx_val: f64,
        success_break_point: f64,
        block_height: u32,
    ) -> EstimateResult {
        let mut n_conf = 0.0;
        let mut total_num = 0.0;
        let mut extra_num = 0.0;
        let mut fail_num = 0.0;
        let period_target = if conf_target == 0 {
            0
        } else {
            conf_target.div_ceil(self.scale)
        };
        let max_bucket_index = self.buckets.len() - 1;

        let mut cur_near_bucket = max_bucket_index;
        let mut best_near_bucket = max_bucket_index;
        let mut cur_far_bucket = max_bucket_index;
        let mut best_far_bucket = max_bucket_index;

        let mut found_answer = false;
        let bins = self.unconfirmed_txs.len() as i64;
        let mut new_bucket_range = true;
        let mut passing = true;
        // Only `median` + `fail_bucket` reach the wire; the passing band is captured by
        // best_near/far + found.
        let mut fail_bucket = BucketResult::default();

        // period_target-1 must index confirmed_average; out of range → no estimate.
        if period_target < 1 || period_target > self.confirmed_average.len() {
            return EstimateResult {
                requested_time: conf_target as u64 * SECONDS_PER_BLOCK,
                fail_bucket,
                median: -1.0,
            };
        }
        let ct = period_target - 1;

        let mut bucket = max_bucket_index as i64;
        while bucket >= 0 {
            let b = bucket as usize;
            if new_bucket_range {
                cur_near_bucket = b;
                new_bucket_range = false;
            }
            cur_far_bucket = b;

            n_conf += self.confirmed_average[ct][b];
            total_num += self.tx_ct_avg[b];
            fail_num += self.failed_average[ct][b];
            for conf_ct in conf_target..self.max_confirms {
                let idx = (i64::from(block_height) - conf_ct as i64).rem_euclid(bins) as usize;
                extra_num += self.unconfirmed_txs[idx][b] as f64;
            }
            extra_num += self.old_unconfirmed_txs[b] as f64;

            if total_num >= sufficient_tx_val / (1.0 - self.decay) {
                let curr_pct = n_conf / (total_num + fail_num + extra_num);
                if curr_pct < success_break_point {
                    if passing {
                        let fail_min_bucket = cur_near_bucket.min(cur_far_bucket);
                        let fail_max_bucket = cur_near_bucket.max(cur_far_bucket);
                        fail_bucket = BucketResult {
                            start: if fail_min_bucket != 0 {
                                self.buckets[fail_min_bucket - 1]
                            } else {
                                0.0
                            },
                            end: self.buckets[fail_max_bucket],
                            within_target: n_conf,
                            total_confirmed: total_num,
                            in_mempool: extra_num,
                            left_mempool: fail_num,
                        };
                        passing = false;
                    }
                } else {
                    // A passing band: bank it, reset the accumulators, and remember its extent.
                    found_answer = true;
                    passing = true;
                    n_conf = 0.0;
                    total_num = 0.0;
                    fail_num = 0.0;
                    extra_num = 0.0;
                    best_near_bucket = cur_near_bucket;
                    best_far_bucket = cur_far_bucket;
                    new_bucket_range = true;
                }
            }
            bucket -= 1;
        }

        let mut median = -1.0;
        let mut tx_sum = 0.0;
        let min_bucket = best_near_bucket.min(best_far_bucket);
        let max_bucket = best_near_bucket.max(best_far_bucket);
        for i in min_bucket..=max_bucket {
            tx_sum += self.tx_ct_avg[i];
        }
        if found_answer && tx_sum != 0.0 {
            tx_sum /= 2.0;
            for i in min_bucket..max_bucket {
                if self.tx_ct_avg[i] < tx_sum {
                    tx_sum -= self.tx_ct_avg[i];
                } else {
                    median = self.m_fee_rate_avg[i] / self.tx_ct_avg[i];
                    break;
                }
            }
        }

        if passing && !new_bucket_range {
            let fail_min_bucket = cur_near_bucket.min(cur_far_bucket);
            let fail_max_bucket = cur_near_bucket.max(cur_far_bucket);
            fail_bucket = BucketResult {
                start: if fail_min_bucket != 0 {
                    self.buckets[fail_min_bucket - 1]
                } else {
                    0.0
                },
                end: self.buckets[fail_max_bucket],
                within_target: n_conf,
                total_confirmed: total_num,
                in_mempool: extra_num,
                left_mempool: fail_num,
            };
        }

        EstimateResult {
            requested_time: conf_target as u64 * SECONDS_PER_BLOCK - SECONDS_PER_BLOCK,
            fail_bucket,
            median,
        }
    }
}

/// The three-horizon confirmation tracker.
pub struct FeeTracker {
    short_horizon: FeeStat,
    med_horizon: FeeStat,
    long_horizon: FeeStat,
    latest_seen_height: u32,
    first_recorded_height: u32,
    buckets: Vec<f64>,
}

impl Default for FeeTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl FeeTracker {
    #[must_use]
    pub fn new() -> Self {
        let buckets = init_buckets();
        FeeTracker {
            short_horizon: FeeStat::new(
                buckets.clone(),
                SHORT_BLOCK_PERIOD,
                SHORT_DECAY,
                SHORT_SCALE,
            ),
            med_horizon: FeeStat::new(buckets.clone(), MED_BLOCK_PERIOD, MED_DECAY, MED_SCALE),
            long_horizon: FeeStat::new(buckets.clone(), LONG_BLOCK_PERIOD, LONG_DECAY, LONG_SCALE),
            latest_seen_height: 0,
            first_recorded_height: 0,
            buckets,
        }
    }

    // A new transaction block confirmed `items` — decay, then record.
    fn process_block(&mut self, block_height: u32, items: &[ItemInfo]) {
        if block_height <= self.latest_seen_height {
            return; // ignore reorgs / non-advancing heights
        }
        self.latest_seen_height = block_height;
        self.short_horizon.clear_current(block_height);
        self.med_horizon.clear_current(block_height);
        self.long_horizon.clear_current(block_height);
        self.short_horizon.update_moving_averages();
        self.med_horizon.update_moving_averages();
        self.long_horizon.update_moving_averages();
        for item in items {
            self.process_block_tx(block_height, *item);
        }
        if self.first_recorded_height == 0 && !items.is_empty() {
            self.first_recorded_height = block_height;
        }
    }

    fn process_block_tx(&mut self, current_height: u32, item: ItemInfo) {
        if current_height <= item.height_added {
            return;
        }
        let blocks_to_confirm = (current_height - item.height_added) as usize;
        self.short_horizon.tx_confirmed(blocks_to_confirm, item);
        self.med_horizon.tx_confirmed(blocks_to_confirm, item);
        self.long_horizon.tx_confirmed(blocks_to_confirm, item);
    }

    // A new mempool item joins the unconfirmed population. Each horizon buckets the fee-rate
    // itself.
    fn add_tx(&mut self, item: ItemInfo) {
        let fee_rate = item.fee_per_cost() * 1000.0;
        self.short_horizon
            .new_mempool_tx(self.latest_seen_height, fee_rate);
        self.med_horizon
            .new_mempool_tx(self.latest_seen_height, fee_rate);
        self.long_horizon
            .new_mempool_tx(self.latest_seen_height, fee_rate);
    }

    // A mempool item left without confirming.
    fn remove_tx(&mut self, item: ItemInfo) {
        let fee_rate = item.fee_per_cost() * 1000.0;
        let bucket_index = get_bucket_index(&self.buckets, fee_rate);
        self.short_horizon
            .remove_tx(self.latest_seen_height, item, bucket_index);
        self.med_horizon
            .remove_tx(self.latest_seen_height, item, bucket_index);
        self.long_horizon
            .remove_tx(self.latest_seen_height, item, bucket_index);
    }

    fn estimate_fee_for_block(&self, target_block: u32) -> EstimateResult {
        self.med_horizon.estimate_median_val(
            target_block as usize,
            SUFFICIENT_FEE_TXS,
            SUCCESS_PCT,
            self.latest_seen_height,
        )
    }

    fn estimate_fee(&self, target_time: u64) -> EstimateResult {
        let confirm_target_block = (target_time / SECONDS_PER_BLOCK) + 1;
        self.estimate_fee_for_block(u32::try_from(confirm_target_block).unwrap_or(u32::MAX))
    }

    /// The most recent transaction-block height fed to the tracker (0 until the first block).
    #[must_use]
    pub fn latest_seen_height(&self) -> u32 {
        self.latest_seen_height
    }

    /// The height of the first block that carried confirmed transactions (0 until then).
    #[must_use]
    pub fn first_recorded_height(&self) -> u32 {
        self.first_recorded_height
    }

    /// The count of mempool transactions the short horizon is tracking as unconfirmed — the
    /// observable proof that the `add_mempool_item` / `remove_mempool_item` hooks fire.
    #[must_use]
    pub fn outstanding_short(&self) -> u64 {
        self.short_horizon.outstanding()
    }
}

/// The node-facing fee estimator. Wraps the tracker with the mempool-size bookkeeping and the
/// median→fee-rate parse that the RPC and the wallet handler consume. One lives inside each
/// [`crate::mempool::Mempool`].
pub struct FeeEstimator {
    tracker: FeeTracker,
    // The most recent mempool cost.
    last_mempool_cost: u64,
    mempool_max_size: u64,
}

impl FeeEstimator {
    #[must_use]
    pub fn new(mempool_max_size: u64) -> Self {
        FeeEstimator {
            tracker: FeeTracker::new(),
            last_mempool_cost: 0,
            mempool_max_size,
        }
    }

    pub(crate) fn add_mempool_item(
        &mut self,
        cost: u64,
        fee: u64,
        height_added: u32,
        mempool_cost: u64,
    ) {
        self.last_mempool_cost = mempool_cost;
        self.tracker.add_tx(ItemInfo {
            cost,
            fee,
            height_added,
        });
    }

    pub(crate) fn remove_mempool_item(
        &mut self,
        cost: u64,
        fee: u64,
        height_added: u32,
        mempool_cost: u64,
    ) {
        self.last_mempool_cost = mempool_cost;
        self.tracker.remove_tx(ItemInfo {
            cost,
            fee,
            height_added,
        });
    }

    // A transaction block confirmed these items.
    pub(crate) fn new_block(
        &mut self,
        block_height: u32,
        included: &[(u64, u64, u32)],
        mempool_cost: u64,
    ) {
        self.last_mempool_cost = mempool_cost;
        let items: Vec<ItemInfo> = included
            .iter()
            .map(|&(cost, fee, height_added)| ItemInfo {
                cost,
                fee,
                height_added,
            })
            .collect();
        self.tracker.process_block(block_height, &items);
    }

    /// The estimated fee-rate (mojos per clvm cost) to be confirmed within `time_offset_seconds`;
    /// 0.0 when there is not enough history. `parse` maps the tracker's median (fee-rate × 1000
    /// scale) back to fee-per-cost via /1000, then the V2→rate step divides by 1000 again.
    #[must_use]
    pub fn estimate_fee_rate(&self, time_offset_seconds: u64) -> f64 {
        let r = self.tracker.estimate_fee(time_offset_seconds);
        let parsed = self.parse(&r);
        if parsed < 0.0 { 0.0 } else { parsed / 1000.0 }
    }

    // Median → fee-per-cost, with the one-bucket-above-the-lowest-failing-bucket fallback.
    // Returns -1.0 when the tracker found no answer at all.
    fn parse(&self, r: &EstimateResult) -> f64 {
        if (r.median - -1.0).abs() > f64::EPSILON {
            return r.median / 1000.0;
        }
        if r.fail_bucket.start == 0.0 {
            return -1.0;
        }
        let max_val = self.tracker.buckets.len() - 1;
        let start_index =
            (get_bucket_index(&self.tracker.buckets, r.fail_bucket.start) + 3).min(max_val);
        self.tracker.buckets[start_index] / 1000.0
    }

    /// The mempool's max cost.
    #[must_use]
    pub fn mempool_max_size(&self) -> u64 {
        self.mempool_max_size
    }

    /// The last-seen mempool cost.
    #[must_use]
    pub fn mempool_size(&self) -> u64 {
        self.last_mempool_cost
    }

    /// The tracker beneath — for observability and tests.
    #[must_use]
    pub fn tracker(&self) -> &FeeTracker {
        &self.tracker
    }

    /// Feed a confirmed-block signal directly. Public so integration tests and the
    /// block-inclusion path can drive the positive signal; the mempool wires this from
    /// [`crate::mempool::Mempool::new_peak`].
    pub fn ingest_block(
        &mut self,
        block_height: u32,
        included: &[(u64, u64, u32)],
        mempool_cost: u64,
    ) {
        self.new_block(block_height, included, mempool_cost);
    }
}

#[cfg(test)]
#[path = "../tests/unit/fee_estimator/tests.rs"]
mod tests;
