use crate::error::NodeError;
use crate::primitives::ConsensusPrimitives;
use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
use dg_xch_core::blockchain::header_block::HeaderBlock;
use dg_xch_core::blockchain::proof_of_space::ProofOfSpace;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48, Bytes96};
use dg_xch_core::blockchain::vdf_info::VdfInfo;
use dg_xch_core::blockchain::vdf_proof::VdfProof;
use dg_xch_core::consensus::block_header_validation::{
    HeaderSigTag, HeaderValidationVerifier, ValidationState, bls_verify,
    validate_finished_header_block,
};
use dg_xch_core::consensus::constants::ConsensusConstants;
use std::collections::HashMap;

pub struct PrimitiveVerifier<'a, P>(pub &'a P);

impl<P: ConsensusPrimitives> HeaderValidationVerifier for PrimitiveVerifier<'_, P> {
    fn validate_vdf(
        &self,
        constants: &ConsensusConstants,
        input: &ClassgroupElement,
        info: &VdfInfo,
        proof: &VdfProof,
        target: Option<&VdfInfo>,
    ) -> bool {
        self.0.verify_vdf(constants, input, info, proof, target)
    }

    fn pospace_quality_string(
        &self,
        constants: &ConsensusConstants,
        proof_of_space: &ProofOfSpace,
        challenge: Bytes32,
        cc_sp_hash: Bytes32,
        height: u32,
    ) -> Option<Bytes32> {
        self.0
            .proof_of_space_quality(proof_of_space, constants, challenge, cc_sp_hash, height)
    }
}

// One VDF verification captured during the sequential header walk, replayed in the parallel drain.
#[derive(Clone)]
pub struct QueuedVdf {
    input: ClassgroupElement,
    info: VdfInfo,
    proof: VdfProof,
    target: Option<VdfInfo>,
}

// One header BLS signature captured during the sequential header walk, replayed in the parallel
// drain. `tag` carries which of the five finished-header gates this is so the drain reproduces
// the exact rejection string on the failing block.
#[derive(Clone)]
pub struct QueuedSig {
    pk: Bytes48,
    msg: Vec<u8>,
    sig: Bytes96,
    tag: HeaderSigTag,
}

/// Window-level header-validation sink: the cross-block pipeline stages every block of a sync
/// window through the sequential header walk with the expensive pure gates deferred here, then
/// drains the whole window across all cores in one batch — per-block batches leave most cores
/// idle. Two queues, drained by [`verify_vdf_batch`] and [`verify_sig_batch`].
/// A `std::sync::Mutex` per queue, not a `RefCell`: the staging loop holds the sink across
/// `.await` points, and the server runs it inside spawned tasks whose futures must be `Send`.
#[derive(Default)]
pub struct HeaderSink {
    pub vdf: std::sync::Mutex<Vec<QueuedVdf>>,
    pub sig: std::sync::Mutex<Vec<QueuedSig>>,
}

impl HeaderSink {
    /// Snapshot the queue lengths so a failed per-block validation attempt can be rewound with
    /// [`HeaderSink::truncate`] before a retry — without this, a retry after a mid-validate
    /// failure (the walk-cache store-fallback repair path) would double-queue the proofs the
    /// first attempt already deferred.
    #[must_use]
    pub fn checkpoint(&self) -> (usize, usize) {
        (
            self.vdf.lock().map_or(0, |q| q.len()),
            self.sig.lock().map_or(0, |q| q.len()),
        )
    }

    /// Rewind both queues to a [`HeaderSink::checkpoint`] (drops entries deferred after it).
    pub fn truncate(&self, checkpoint: (usize, usize)) {
        if let Ok(mut q) = self.vdf.lock() {
            q.truncate(checkpoint.0);
        }
        if let Ok(mut q) = self.sig.lock() {
            q.truncate(checkpoint.1);
        }
    }
}

// Deferred-VDF wrapper: the header validator's `validate_vdf` calls are pure boolean gates — no
// verification result ever feeds a later input computation — so the sequential walk queues every
// proof and answers true, and the queue is verified afterwards across all cores. The accept/reject
// decision is identical (all gates ANDed); only the failure short-circuit order changes.
// Proof-of-space stays inline: its quality string is a value consumed by required_iters.
struct DeferredVdfVerifier<'a, P> {
    inner: PrimitiveVerifier<'a, P>,
    queue: std::cell::RefCell<Vec<QueuedVdf>>,
    sig_queue: std::cell::RefCell<Vec<QueuedSig>>,
}

impl<P: ConsensusPrimitives> HeaderValidationVerifier for DeferredVdfVerifier<'_, P> {
    fn validate_vdf(
        &self,
        _constants: &ConsensusConstants,
        input: &ClassgroupElement,
        info: &VdfInfo,
        proof: &VdfProof,
        target: Option<&VdfInfo>,
    ) -> bool {
        self.queue.borrow_mut().push(QueuedVdf {
            input: *input,
            info: *info,
            proof: proof.clone(),
            target: target.copied(),
        });
        true
    }

    fn pospace_quality_string(
        &self,
        constants: &ConsensusConstants,
        proof_of_space: &ProofOfSpace,
        challenge: Bytes32,
        cc_sp_hash: Bytes32,
        height: u32,
    ) -> Option<Bytes32> {
        self.inner
            .pospace_quality_string(constants, proof_of_space, challenge, cc_sp_hash, height)
    }

    fn verify_bls_sig(&self, pk: &Bytes48, msg: &[u8], sig: &Bytes96, tag: HeaderSigTag) -> bool {
        self.sig_queue.borrow_mut().push(QueuedSig {
            pk: *pk,
            msg: msg.to_vec(),
            sig: *sig,
            tag,
        });
        true
    }
}

// The exact bytes of every field that determines a queued verification's result, fixed-size
// fields first and the variable-length witness as the tail — injective, so two distinct
// verifications can never collapse into one dedup bucket.
fn dedup_key(q: &QueuedVdf) -> Vec<u8> {
    let witness = q.proof.witness.as_slice();
    let mut key = Vec::with_capacity(100 + 32 + 100 + 8 + 2 + 141 + witness.len());
    key.extend_from_slice(<_ as AsRef<[u8]>>::as_ref(&q.input.data));
    key.extend_from_slice(<_ as AsRef<[u8]>>::as_ref(&q.info.challenge));
    key.extend_from_slice(<_ as AsRef<[u8]>>::as_ref(&q.info.output.data));
    key.extend_from_slice(&q.info.number_of_iterations.to_be_bytes());
    key.push(q.proof.witness_type);
    key.push(u8::from(q.proof.normalized_to_identity));
    match &q.target {
        None => key.push(0),
        Some(t) => {
            key.push(1);
            key.extend_from_slice(<_ as AsRef<[u8]>>::as_ref(&t.challenge));
            key.extend_from_slice(<_ as AsRef<[u8]>>::as_ref(&t.output.data));
            key.extend_from_slice(&t.number_of_iterations.to_be_bytes());
        }
    }
    key.extend_from_slice(witness);
    key
}

// Verify a queued batch across every available core (scoped threads; results ANDed). Wesolowski
// verification is pure CPU on immutable inputs — embarrassingly parallel.
pub(crate) fn verify_vdf_batch<P: ConsensusPrimitives + Sync>(
    primitives: &P,
    constants: &ConsensusConstants,
    queue: Vec<QueuedVdf>,
) -> bool {
    if queue.is_empty() {
        return true;
    }
    log::debug!("vdf.batch proofs={}", queue.len());
    let mut seen: std::collections::HashSet<Vec<u8>> =
        std::collections::HashSet::with_capacity(queue.len());
    let mut order: Vec<usize> = Vec::with_capacity(queue.len());
    for (i, q) in queue.iter().enumerate() {
        if seen.insert(dedup_key(q)) {
            order.push(i);
        }
    }
    drop(seen);
    order.sort_by_key(|&i| std::cmp::Reverse(queue[i].proof.witness_type));
    #[cfg(feature = "drain-probe")]
    let workers = dg_xch_core::compute::worker_count().min(order.len());
    #[cfg(feature = "drain-probe")]
    let probe = drain_probe::BatchProbe::start(
        primitives,
        constants,
        &queue,
        workers,
        queue.len().div_ceil(workers),
    );
    let failed = std::sync::atomic::AtomicBool::new(false);
    let results = dg_xch_core::compute::map(dg_xch_core::compute::Phase::Vdf, &order, |index| {
        if failed.load(std::sync::atomic::Ordering::Relaxed) {
            return false;
        }
        let proof = &queue[*index];
        let valid = primitives.verify_vdf_serial(
            constants,
            &proof.input,
            &proof.info,
            &proof.proof,
            proof.target.as_ref(),
        );
        if !valid {
            failed.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        valid
    });
    let ok = results.into_iter().all(|valid| valid);
    #[cfg(feature = "drain-probe")]
    probe.finish(ok, queue.len());
    ok
}

fn verify_one_sig(q: &QueuedSig) -> bool {
    bls_verify(&q.pk, &q.msg, &q.sig)
}

/// Verify a queued batch of header BLS signatures on the shared CPU pool, ANDed. Same
/// work-stealing pool as `verify_vdf_batch`, but without the witness sort or dedup pass:
/// each header sig is one AugScheme pairing of uniform cost. Each signature is verified through
/// the same `bls_verify` the inline path calls, so the outcome is identical.
#[must_use]
pub(crate) fn verify_sig_batch(queue: &[QueuedSig]) -> bool {
    if queue.is_empty() {
        return true;
    }
    log::debug!("sig.batch sigs={}", queue.len());
    let failed = std::sync::atomic::AtomicBool::new(false);
    dg_xch_core::compute::map(dg_xch_core::compute::Phase::Signature, queue, |signature| {
        if failed.load(std::sync::atomic::Ordering::Relaxed) {
            return false;
        }
        let valid = verify_one_sig(signature);
        if !valid {
            failed.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        valid
    })
    .into_iter()
    .all(|valid| valid)
}

#[must_use]
pub fn first_failing_sig(queue: &[QueuedSig]) -> Option<HeaderSigTag> {
    queue.iter().find(|q| !verify_one_sig(q)).map(|q| q.tag)
}

/// Full single-block PoW/VDF header validation against the ancestor records, returning the block's
/// `required_iters`. Runs core's `validate_finished_header_block` through the engine's primitive
/// backends, with every VDF proof deferred out of the sequential walk and verified across all
/// cores — the genesis-era long-sync's dominant cost (three Wesolowski proofs per finished
/// sub-slot) parallelizes per block.
///
/// # Errors
/// Returns [`NodeError::Invalid`] if any header check fails or an ancestor referenced by the walk is absent.
pub fn validate_finished_header<P: ConsensusPrimitives + Sync>(
    primitives: &P,
    constants: &ConsensusConstants,
    ancestors: &HashMap<Bytes32, BlockRecord>,
    block: &HeaderBlock,
    vs: ValidationState,
    check_sub_epoch_summary: bool,
) -> Result<u64, NodeError> {
    let sink = HeaderSink::default();
    let required_iters = validate_finished_header_deferred(
        primitives,
        constants,
        ancestors,
        block,
        vs,
        check_sub_epoch_summary,
        &sink,
    )?;
    let vdf_queue = sink.vdf.into_inner().unwrap_or_default();
    if !verify_vdf_batch(primitives, constants, vdf_queue) {
        return Err(NodeError::Invalid(format!(
            "INVALID_VDF at height {} (deferred batch)",
            block.height()
        )));
    }
    // Header sigs drained the same way: the single-block path attributes the exact rejection
    // string via the failing signature's tag.
    let sig_queue = sink.sig.into_inner().unwrap_or_default();
    if let Some(tag) = first_failing_sig(&sig_queue) {
        return Err(NodeError::Invalid(format!(
            "{} at height {} (deferred batch)",
            tag.rejection(),
            block.height()
        )));
    }
    Ok(required_iters)
}

pub fn validate_finished_header_deferred<P: ConsensusPrimitives + Sync>(
    primitives: &P,
    constants: &ConsensusConstants,
    ancestors: &HashMap<Bytes32, BlockRecord>,
    block: &HeaderBlock,
    vs: ValidationState,
    check_sub_epoch_summary: bool,
    sink: &HeaderSink,
) -> Result<u64, NodeError> {
    let verifier = DeferredVdfVerifier {
        inner: PrimitiveVerifier(primitives),
        queue: std::cell::RefCell::new(Vec::new()),
        sig_queue: std::cell::RefCell::new(Vec::new()),
    };
    let required_iters = validate_finished_header_block(
        constants,
        &verifier,
        ancestors,
        block,
        vs,
        check_sub_epoch_summary,
    )
    .map_err(NodeError::Io)?;
    // Extend both window queues. Both extends must succeed or the block's deferred work is lost — a
    // poisoned sink fails the window closed (the silent-fallback ban), never a validation bypass.
    match sink.vdf.lock() {
        Ok(mut q) => q.extend(verifier.queue.into_inner()),
        Err(_) => {
            return Err(NodeError::Invalid("poisoned window VDF sink".to_string()));
        }
    }
    match sink.sig.lock() {
        Ok(mut q) => q.extend(verifier.sig_queue.into_inner()),
        Err(_) => {
            return Err(NodeError::Invalid("poisoned window sig sink".to_string()));
        }
    }
    Ok(required_iters)
}

// Window-drain measurement probe (feature `drain-probe`; measurement builds only).
//   DGXCH_DRAIN_PROBE=wall   — per-batch wall / process-CPU / worker count.
//   DGXCH_DRAIN_PROBE=serial — additionally verifies every queued proof serially first to name
//                              the per-proof cost split (doubles VDF work; the pre-pass warms the
//                              discriminant memo, flattering the parallel wall).
// Both modes count proof recurrence within the run.
#[cfg(feature = "drain-probe")]
mod drain_probe {
    use super::QueuedVdf;
    use crate::primitives::ConsensusPrimitives;
    use dg_xch_core::consensus::constants::ConsensusConstants;
    use std::collections::HashMap;
    use std::hash::{Hash, Hasher};
    use std::sync::{Mutex, OnceLock};
    use std::time::Instant;

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Mode {
        Off,
        Wall,
        Serial,
    }

    fn mode() -> Mode {
        static MODE: OnceLock<Mode> = OnceLock::new();
        *MODE.get_or_init(|| match std::env::var("DGXCH_DRAIN_PROBE").as_deref() {
            Ok("wall") => Mode::Wall,
            Ok("serial") => Mode::Serial,
            _ => Mode::Off,
        })
    }

    // utime+stime of the whole process, milliseconds (Linux /proc; USER_HZ=100 on every fleet
    // kernel). 0 elsewhere — the probe is a Linux measurement tool.
    fn process_cpu_ms() -> u64 {
        std::fs::read_to_string("/proc/self/stat")
            .ok()
            .and_then(|s| {
                let (_, rest) = s.rsplit_once(')')?;
                let fields: Vec<&str> = rest.split_whitespace().collect();
                let utime: u64 = fields.get(11)?.parse().ok()?;
                let stime: u64 = fields.get(12)?.parse().ok()?;
                Some((utime + stime) * 10)
            })
            .unwrap_or(0)
    }

    fn proof_key(q: &QueuedVdf) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        <_ as AsRef<[u8]>>::as_ref(&q.input.data).hash(&mut h);
        <_ as AsRef<[u8]>>::as_ref(&q.info.challenge).hash(&mut h);
        <_ as AsRef<[u8]>>::as_ref(&q.info.output.data).hash(&mut h);
        q.info.number_of_iterations.hash(&mut h);
        q.proof.witness_type.hash(&mut h);
        q.proof.witness.as_slice().hash(&mut h);
        h.finish()
    }

    fn seen() -> &'static Mutex<HashMap<u64, u32>> {
        static SEEN: OnceLock<Mutex<HashMap<u64, u32>>> = OnceLock::new();
        SEEN.get_or_init(|| Mutex::new(HashMap::new()))
    }

    struct SerialSplit {
        serial_wall_ms: f64,
        serial_cpu_ms: u64,
        chunk_max_ms: f64,
        chunk_mean_ms: f64,
        // count, total wall ms, keyed by min(witness_type, 3).
        per_wt: [(u64, f64); 4],
        min_ms: f64,
        max_ms: f64,
    }

    pub(super) struct BatchProbe {
        mode: Mode,
        wall: Instant,
        cpu_ms0: u64,
        workers: usize,
        repeats: u64,
        uniq_total: usize,
        iters_min: u64,
        iters_max: u64,
        wt_counts: [u64; 4],
        serial: Option<SerialSplit>,
    }

    impl BatchProbe {
        pub(super) fn start<P: ConsensusPrimitives + Sync>(
            primitives: &P,
            constants: &ConsensusConstants,
            queue: &[QueuedVdf],
            workers: usize,
            chunk: usize,
        ) -> Self {
            let mode = mode();
            let mut repeats = 0u64;
            let mut uniq_total = 0usize;
            let mut iters_min = u64::MAX;
            let mut iters_max = 0u64;
            let mut wt_counts = [0u64; 4];
            if mode != Mode::Off {
                if let Ok(mut map) = seen().lock() {
                    for q in queue {
                        let c = map.entry(proof_key(q)).or_insert(0);
                        if *c > 0 {
                            repeats += 1;
                        }
                        *c += 1;
                    }
                    uniq_total = map.len();
                }
                for q in queue {
                    iters_min = iters_min.min(q.info.number_of_iterations);
                    iters_max = iters_max.max(q.info.number_of_iterations);
                    wt_counts[(q.proof.witness_type as usize).min(3)] += 1;
                }
            }
            let serial = (mode == Mode::Serial).then(|| {
                let cpu0 = process_cpu_ms();
                let mut per = Vec::with_capacity(queue.len());
                for q in queue {
                    let t = Instant::now();
                    let _ = primitives.verify_vdf(
                        constants,
                        &q.input,
                        &q.info,
                        &q.proof,
                        q.target.as_ref(),
                    );
                    per.push(t.elapsed().as_secs_f64() * 1e3);
                }
                let serial_cpu_ms = process_cpu_ms().saturating_sub(cpu0);
                let mut per_wt = [(0u64, 0f64); 4];
                for (q, ms) in queue.iter().zip(&per) {
                    let b = (q.proof.witness_type as usize).min(3);
                    per_wt[b].0 += 1;
                    per_wt[b].1 += ms;
                }
                let sums: Vec<f64> = per.chunks(chunk).map(|c| c.iter().sum()).collect();
                let chunk_max_ms = sums.iter().copied().fold(0.0f64, f64::max);
                let chunk_mean_ms = sums.iter().sum::<f64>() / sums.len().max(1) as f64;
                SerialSplit {
                    serial_wall_ms: per.iter().sum(),
                    serial_cpu_ms,
                    chunk_max_ms,
                    chunk_mean_ms,
                    per_wt,
                    min_ms: per.iter().copied().fold(f64::INFINITY, f64::min),
                    max_ms: per.iter().copied().fold(0.0f64, f64::max),
                }
            });
            Self {
                mode,
                wall: Instant::now(),
                cpu_ms0: process_cpu_ms(),
                workers,
                repeats,
                uniq_total,
                iters_min,
                iters_max,
                wt_counts,
                serial,
            }
        }

        pub(super) fn finish(self, ok: bool, proofs: usize) {
            if self.mode == Mode::Off {
                return;
            }
            let wall_ms = self.wall.elapsed().as_secs_f64() * 1e3;
            let cpu_ms = process_cpu_ms().saturating_sub(self.cpu_ms0);
            let mut line = format!(
                "DRAIN-PROBE proofs={proofs} ok={ok} wall_ms={wall_ms:.1} cpu_ms={cpu_ms} \
                 workers={} repeats={} uniq_total={} iters={}..{} wt_counts={:?}",
                self.workers,
                self.repeats,
                self.uniq_total,
                self.iters_min,
                self.iters_max,
                self.wt_counts
            );
            if let Some(s) = &self.serial {
                let wt: Vec<String> = s
                    .per_wt
                    .iter()
                    .enumerate()
                    .filter(|(_, (n, _))| *n > 0)
                    .map(|(w, (n, total))| format!("wt{w}:{n}x{:.1}ms", total / *n as f64))
                    .collect();
                line.push_str(&format!(
                    " serial_wall_ms={:.1} serial_cpu_ms={} chunk_max_ms={:.1} \
                     chunk_mean_ms={:.1} proof_ms={:.1}..{:.1} per_wt=[{}]",
                    s.serial_wall_ms,
                    s.serial_cpu_ms,
                    s.chunk_max_ms,
                    s.chunk_mean_ms,
                    s.min_ms,
                    s.max_ms,
                    wt.join(",")
                ));
            }
            eprintln!("{line}");
        }
    }
}

#[cfg(test)]
#[path = "../tests/unit/header/tests.rs"]
mod tests;
