use super::CompactPlot;
pub use super::Entry;
use crate::compute::{SCRATCH_BYTES, Work, allocate, check_cancelled};
use crate::params::ProofParams;
use crate::plotting::PlotLimits;
use crate::radix;
use std::io::{Error, ErrorKind};
use std::sync::atomic::AtomicBool;
use std::time::Instant;

pub const TRANSFER_BYTES: u64 = 64 * 1024 * 1024;
pub const OUTPUT_ENTRIES: usize = TRANSFER_BYTES as usize / size_of::<Entry>();
pub const LEFT_BATCH: usize = 1 << 20;
pub const INDEX_BYTES: u64 = ((1u64 << 28) + 1) * 4;

pub struct BatchStatus {
    pub output_count: usize,
    pub pair_evaluations: u64,
}

pub trait Backend {
    fn generate(&mut self, start: usize, count: usize, cancelled: &AtomicBool)
    -> Result<(), Error>;

    fn upload_index(
        &mut self,
        table: u32,
        entries: &[Entry],
        cancelled: &AtomicBool,
    ) -> Result<(), Error>;

    fn match_table(
        &mut self,
        table: u32,
        start: usize,
        count: usize,
        pair_budget: u64,
        cancelled: &AtomicBool,
    ) -> Result<BatchStatus, Error>;

    fn read_entries(
        &mut self,
        count: usize,
        destination: &mut Vec<Entry>,
        capacity: usize,
        cancelled: &AtomicBool,
    ) -> Result<(), Error>;

    fn release_inputs(&mut self) -> Result<(), Error>;
}

pub fn memory_required(max_entries: usize) -> Result<u64, Error> {
    CompactPlot::memory_required(28, max_entries)?
        .checked_add(INDEX_BYTES + 3 * TRANSFER_BYTES + 1024 * 1024)
        .ok_or_else(|| Error::other("resident plotting memory budget overflow"))
}

fn read_entries_exact(
    gpu: &mut impl Backend,
    count: usize,
    destination: &mut Vec<Entry>,
    capacity: usize,
    cancelled: &AtomicBool,
) -> Result<(), Error> {
    check_cancelled(cancelled)?;
    let expected = destination
        .len()
        .checked_add(count)
        .filter(|expected| count <= OUTPUT_ENTRIES && *expected <= capacity)
        .ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidInput,
                "resident readback exceeds entry budget",
            )
        })?;
    gpu.read_entries(count, destination, capacity, cancelled)?;
    if destination.len() != expected {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "resident backend returned an incorrect entry count",
        ));
    }
    check_cancelled(cancelled)
}

pub fn build<Engine: Backend>(
    params: &ProofParams,
    limits: PlotLimits,
    cancelled: &AtomicBool,
    create: impl FnOnce() -> Result<Option<Engine>, Error>,
) -> Result<Option<CompactPlot>, Error> {
    if params.k() != 28 || params.strength() != 2 || !cfg!(target_endian = "little") {
        return Ok(None);
    }
    check_cancelled(cancelled)?;
    let base = CompactPlot::memory_required(params.k(), limits.max_entries)?;
    if memory_required(limits.max_entries)? > limits.memory_bytes {
        return Ok(None);
    }
    if limits.max_work < CompactPlot::minimum_work(params) {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "insufficient resident plotting work budget",
        ));
    }
    let capacity = usize::try_from((base - SCRATCH_BYTES) / (2 * size_of::<Entry>() as u64))
        .map_err(|_| Error::other("resident plot exceeds platform address space"))?;
    let Some(mut gpu) = create()? else {
        return Ok(None);
    };
    let mut work = Work::new(limits, cancelled);
    let profile = std::env::var_os("DGX_POS2_PROFILE").is_some();
    let mut entries = allocate::<Entry>(capacity)?;
    let initial = 1usize << 28;
    let started = Instant::now();
    for start in (0..initial).step_by(OUTPUT_ENTRIES) {
        let count = OUTPUT_ENTRIES.min(initial - start);
        work.charge(count as u64)?;
        gpu.generate(start, count, cancelled)?;
        read_entries_exact(&mut gpu, count, &mut entries, capacity, cancelled)?;
    }
    if profile {
        eprintln!(
            "pos2_resident generation seconds={:.3}",
            started.elapsed().as_secs_f64()
        );
    }
    let mut table_counts = [entries.len(), 0, 0, 0];
    for table in 1..=3 {
        let started = Instant::now();
        let mut scratch = Vec::new();
        radix::sort(
            &mut entries,
            &mut scratch,
            28,
            |entry| u64::from(entry.info),
            cancelled,
        )?;
        drop(scratch);
        if profile {
            eprintln!(
                "pos2_resident sort_{} seconds={:.3}",
                table - 1,
                started.elapsed().as_secs_f64()
            );
        }
        let started = Instant::now();
        gpu.upload_index(table, &entries, cancelled)?;
        let input_count = entries.len();
        entries.clear();
        if profile {
            eprintln!(
                "pos2_resident upload_index_{table} seconds={:.3}",
                started.elapsed().as_secs_f64()
            );
        }
        let started = Instant::now();
        for start in (0..input_count).step_by(LEFT_BATCH) {
            let count = LEFT_BATCH.min(input_count - start);
            work.charge(count as u64 * 4)?;
            let status = gpu.match_table(table, start, count, work.remaining(), cancelled)?;
            if status.output_count > OUTPUT_ENTRIES
                || status.output_count > capacity.saturating_sub(entries.len())
            {
                return Err(Error::other("resident table exceeds output capacity"));
            }
            work.charge(status.pair_evaluations)?;
            read_entries_exact(
                &mut gpu,
                status.output_count,
                &mut entries,
                capacity,
                cancelled,
            )?;
        }
        gpu.release_inputs()?;
        table_counts[table as usize] = entries.len();
        if profile {
            eprintln!(
                "pos2_resident matching_{table} seconds={:.3} entries={}",
                started.elapsed().as_secs_f64(),
                entries.len()
            );
        }
    }
    drop(gpu);
    let started = Instant::now();
    let mut fragments = allocate(entries.len())?;
    for chunk in entries.chunks(4096) {
        check_cancelled(cancelled)?;
        fragments.extend(chunk.iter().map(|entry| entry.meta));
    }
    drop(entries);
    let mut scratch = Vec::new();
    radix::sort(
        &mut fragments,
        &mut scratch,
        56,
        |fragment| *fragment,
        cancelled,
    )?;
    if profile {
        eprintln!(
            "pos2_resident sort_3 seconds={:.3} work={}",
            started.elapsed().as_secs_f64(),
            limits.max_work - work.remaining()
        );
    }
    Ok(Some(CompactPlot {
        params: params.clone(),
        fragments,
        table_counts,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::atomic::Ordering;

    #[derive(Default)]
    struct TestBackend {
        append_count: usize,
        calls: usize,
        fail: bool,
        cancel_after_read: bool,
    }

    impl Backend for TestBackend {
        fn generate(
            &mut self,
            _start: usize,
            _count: usize,
            _cancelled: &AtomicBool,
        ) -> Result<(), Error> {
            unreachable!("preflight tests must not generate a table")
        }

        fn upload_index(
            &mut self,
            _table: u32,
            _entries: &[Entry],
            _cancelled: &AtomicBool,
        ) -> Result<(), Error> {
            unreachable!("preflight tests must not upload a table")
        }

        fn match_table(
            &mut self,
            _table: u32,
            _start: usize,
            _count: usize,
            _pair_budget: u64,
            _cancelled: &AtomicBool,
        ) -> Result<BatchStatus, Error> {
            unreachable!("preflight tests must not match a table")
        }

        fn read_entries(
            &mut self,
            _count: usize,
            destination: &mut Vec<Entry>,
            _capacity: usize,
            cancelled: &AtomicBool,
        ) -> Result<(), Error> {
            self.calls += 1;
            destination.extend((0..self.append_count).map(|position| Entry {
                meta: position as u64,
                info: position as u32,
                x_bits: 0,
            }));
            if self.cancel_after_read {
                cancelled.store(true, Ordering::Relaxed);
            }
            if self.fail {
                Err(Error::new(ErrorKind::Unsupported, "test backend failure"))
            } else {
                Ok(())
            }
        }

        fn release_inputs(&mut self) -> Result<(), Error> {
            unreachable!("preflight tests must not allocate inputs")
        }
    }

    fn params() -> ProofParams {
        ProofParams::new([37; 32].into(), 28, 2, false).unwrap()
    }

    fn limits() -> PlotLimits {
        PlotLimits {
            memory_bytes: 12 * 1024 * 1024 * 1024,
            max_entries: 310_000_000,
            max_work: 100_000_000_000,
        }
    }

    #[test]
    #[cfg(target_endian = "little")]
    fn preflight_rejects_invalid_limits_before_backend_creation() {
        let params = params();
        let limits = limits();
        let required = memory_required(limits.max_entries).unwrap();
        let cancelled = AtomicBool::new(false);
        for invalid in [
            PlotLimits {
                max_entries: (1 << 28) - 1,
                ..limits
            },
            PlotLimits {
                memory_bytes: required,
                max_work: CompactPlot::minimum_work(&params) - 1,
                ..limits
            },
        ] {
            let error = build::<TestBackend>(&params, invalid, &cancelled, || {
                panic!("invalid budgets must not create a backend")
            })
            .err()
            .unwrap();
            assert_eq!(error.kind(), ErrorKind::InvalidInput);
        }
        let error = build::<TestBackend>(&params, limits, &AtomicBool::new(true), || {
            panic!("cancellation must not create a backend")
        })
        .err()
        .unwrap();
        assert_eq!(error.kind(), ErrorKind::Interrupted);
        let result = build::<TestBackend>(
            &params,
            PlotLimits {
                memory_bytes: required - 1,
                ..limits
            },
            &cancelled,
            || panic!("insufficient memory must not create a backend"),
        )
        .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn unsupported_parameters_do_not_create_a_backend() {
        for (plot_size, strength) in [(18, 2), (26, 2), (28, 3), (30, 2), (32, 2)] {
            let params = ProofParams::new([37; 32].into(), plot_size, strength, false).unwrap();
            let result = build::<TestBackend>(&params, limits(), &AtomicBool::new(false), || {
                panic!("unsupported parameters must not create a backend")
            })
            .unwrap();
            assert!(result.is_none());
        }
    }

    #[test]
    #[cfg(target_endian = "little")]
    fn unsupported_backend_returns_before_table_allocation() {
        let called = Cell::new(false);
        let result = build::<TestBackend>(&params(), limits(), &AtomicBool::new(false), || {
            called.set(true);
            Ok(None)
        })
        .unwrap();
        assert!(called.get());
        assert!(result.is_none());
    }

    #[test]
    fn readback_requires_exact_append_count() {
        let cancelled = AtomicBool::new(false);
        for append_count in [0, 1, 2, 3] {
            let mut backend = TestBackend {
                append_count,
                ..Default::default()
            };
            let mut destination = vec![Entry {
                meta: 99,
                ..Default::default()
            }];
            let result = read_entries_exact(&mut backend, 2, &mut destination, 4, &cancelled);
            if append_count == 2 {
                result.unwrap();
                assert_eq!(destination.len(), 3);
                assert_eq!(destination[0].meta, 99);
                assert_eq!(destination[1].meta, 0);
                assert_eq!(destination[2].meta, 1);
            } else {
                assert_eq!(result.unwrap_err().kind(), ErrorKind::InvalidData);
            }
            assert_eq!(backend.calls, 1);
        }
    }

    #[test]
    fn readback_checks_capacity_and_cancellation_before_backend_call() {
        let mut backend = TestBackend::default();
        for (length, count, capacity) in [
            (0, 1, 0),
            (1, 0, 0),
            (1, 1, 1),
            (0, OUTPUT_ENTRIES + 1, OUTPUT_ENTRIES + 1),
        ] {
            let mut destination = vec![Entry::default(); length];
            let error = read_entries_exact(
                &mut backend,
                count,
                &mut destination,
                capacity,
                &AtomicBool::new(false),
            )
            .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::InvalidInput);
            assert_eq!(destination.len(), length);
        }
        let error = read_entries_exact(&mut backend, 0, &mut Vec::new(), 0, &AtomicBool::new(true))
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Interrupted);
        assert_eq!(backend.calls, 0);
        read_entries_exact(&mut backend, 0, &mut Vec::new(), 0, &AtomicBool::new(false)).unwrap();
        assert_eq!(backend.calls, 1);
    }

    #[test]
    fn readback_propagates_backend_errors_and_observes_late_cancellation() {
        for (fail, cancel_after_read, expected) in [
            (true, false, ErrorKind::Unsupported),
            (false, true, ErrorKind::Interrupted),
        ] {
            let mut backend = TestBackend {
                append_count: 1,
                fail,
                cancel_after_read,
                ..Default::default()
            };
            let error =
                read_entries_exact(&mut backend, 1, &mut Vec::new(), 1, &AtomicBool::new(false))
                    .unwrap_err();
            assert_eq!(error.kind(), expected);
            assert_eq!(backend.calls, 1);
        }
    }
}
