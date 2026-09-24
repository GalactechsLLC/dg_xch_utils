#[cfg(feature = "cuda")]
mod cuda;
#[cfg(test)]
mod tests;
#[cfg(feature = "vulkan")]
mod vulkan;

use crate::gigahorse_cpu::C30Table5Entry;
use std::ffi::OsStr;
use std::io::{Error, ErrorKind};

const BUCKETS: usize = 18_188_177;
const GENERATION_BATCH: usize = 65535 * 128;
const MATCHING_BATCH: usize = 65535;
const COARSE_BITS: u32 = 12;
const LOCAL_COARSE_BITS: u32 = 10;

fn parse_bool_override(value: Option<&OsStr>, setting: &str) -> Result<Option<bool>, Error> {
    match value {
        None => Ok(None),
        Some(value) if value == "0" => Ok(Some(false)),
        Some(value) if value == "1" => Ok(Some(true)),
        _ => Err(Error::new(
            ErrorKind::InvalidInput,
            format!("GigaHorse {setting} must be 0 or 1"),
        )),
    }
}

fn select_feature(requested: Option<bool>, supported: bool, feature: &str) -> Result<bool, Error> {
    if requested == Some(true) && !supported {
        return Err(Error::new(
            ErrorKind::Unsupported,
            format!("GigaHorse {feature} exceeds device resource limits"),
        ));
    }
    Ok(requested.unwrap_or(supported))
}

fn select_threads(
    value: Option<&OsStr>,
    setting: &str,
    allowed: &[u32],
    preference: &[u32],
    mut supported: impl FnMut(u32) -> Result<bool, Error>,
) -> Result<Option<u32>, Error> {
    if let Some(value) = value {
        let threads = allowed
            .iter()
            .copied()
            .find(|threads| value == OsStr::new(&threads.to_string()))
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidInput,
                    format!("GigaHorse {setting} must be one of {allowed:?}"),
                )
            })?;
        if !supported(threads)? {
            return Err(Error::new(
                ErrorKind::Unsupported,
                format!("GigaHorse {setting}={threads} exceeds device resource limits"),
            ));
        }
        return Ok(Some(threads));
    }
    for threads in preference {
        if supported(*threads)? {
            return Ok(Some(*threads));
        }
    }
    Ok(None)
}

fn parse_first_partitions(value: Option<&OsStr>, default: usize) -> Result<usize, Error> {
    match value {
        None => Ok(default),
        Some(value) if value == "0" => Ok(0),
        Some(value) if value == "32" => Ok(32),
        Some(value) if value == "64" => Ok(64),
        _ => Err(Error::new(
            ErrorKind::InvalidInput,
            "GigaHorse GH_GPU_F1_PARTITIONS must be 0, 32 or 64",
        )),
    }
}

fn kernel_index(operation: u32, flags: u32, operation_count: usize) -> Option<usize> {
    let operation = operation as usize;
    (operation < operation_count).then_some(if operation == 18 && flags & 2 != 0 {
        operation_count
    } else {
        operation
    })
}

#[cfg(test)]
mod tuning_tests {
    use super::*;

    #[test]
    fn boolean_overrides_preserve_automatic_selection() {
        assert_eq!(parse_bool_override(None, "TEST").unwrap(), None);
        assert_eq!(
            parse_bool_override(Some(OsStr::new("0")), "TEST").unwrap(),
            Some(false)
        );
        assert_eq!(
            parse_bool_override(Some(OsStr::new("1")), "TEST").unwrap(),
            Some(true)
        );
        for value in ["", "true", "01", " 1", "1 "] {
            assert_eq!(
                parse_bool_override(Some(OsStr::new(value)), "TEST")
                    .unwrap_err()
                    .kind(),
                ErrorKind::InvalidInput
            );
        }
        for supported in [false, true] {
            assert_eq!(select_feature(None, supported, "test").unwrap(), supported);
            assert!(!select_feature(Some(false), supported, "test").unwrap());
        }
        assert!(select_feature(Some(true), true, "test").unwrap());
        assert_eq!(
            select_feature(Some(true), false, "test")
                .unwrap_err()
                .kind(),
            ErrorKind::Unsupported
        );
    }

    #[test]
    fn thread_defaults_fall_back_but_explicit_overrides_do_not() {
        let allowed = [128, 256, 384, 512];
        for (limit, expected) in [
            (512, Some(512)),
            (256, Some(256)),
            (128, Some(128)),
            (64, None),
        ] {
            assert_eq!(
                select_threads(None, "TEST", &allowed, &[512, 256, 128], |threads| Ok(
                    threads <= limit
                ))
                .unwrap(),
                expected
            );
        }
        assert_eq!(
            select_threads(None, "TEST", &allowed, &[384, 256, 128], |_| Ok(true)).unwrap(),
            Some(384)
        );
        assert_eq!(
            select_threads(
                Some(OsStr::new("128")),
                "TEST",
                &allowed,
                &[512, 256, 128],
                |_| Ok(true)
            )
            .unwrap(),
            Some(128)
        );
        assert_eq!(
            select_threads(
                Some(OsStr::new("512")),
                "TEST",
                &allowed,
                &[512, 256, 128],
                |threads| Ok(threads <= 256)
            )
            .unwrap_err()
            .kind(),
            ErrorKind::Unsupported
        );
        for value in ["", "0", "1024", "0128", "+128", " 128", "128 "] {
            assert_eq!(
                select_threads(
                    Some(OsStr::new(value)),
                    "TEST",
                    &allowed,
                    &[512, 256, 128],
                    |_| Ok(true)
                )
                .unwrap_err()
                .kind(),
                ErrorKind::InvalidInput
            );
        }
        assert_eq!(
            select_threads(None, "TEST", &allowed, &[512, 256, 128], |_| Err(
                Error::other("query failed")
            ))
            .unwrap_err()
            .kind(),
            ErrorKind::Other
        );
    }

    #[test]
    fn first_partition_overrides_preserve_device_defaults() {
        for default in [32, 64] {
            assert_eq!(parse_first_partitions(None, default).unwrap(), default);
            assert_eq!(
                parse_first_partitions(Some(OsStr::new("0")), default).unwrap(),
                0
            );
            assert_eq!(
                parse_first_partitions(Some(OsStr::new("32")), default).unwrap(),
                32
            );
            assert_eq!(
                parse_first_partitions(Some(OsStr::new("64")), default).unwrap(),
                64
            );
        }
        for value in [
            "", "1", "16", "128", "032", "064", " 32", "32 ", " 64", "64 ",
        ] {
            assert_eq!(
                parse_first_partitions(Some(OsStr::new(value)), 32)
                    .unwrap_err()
                    .kind(),
                ErrorKind::InvalidInput
            );
        }
    }

    #[test]
    fn private_f1_kernel_mapping_preserves_logical_operation_limits() {
        for operation_count in [19, 20, 21] {
            for flags in 0..4 {
                for operation in 0..operation_count as u32 {
                    assert_eq!(
                        kernel_index(operation, flags, operation_count),
                        Some(if operation == 18 && flags & 2 != 0 {
                            operation_count
                        } else {
                            operation as usize
                        })
                    );
                }
                for operation in operation_count as u32..=21 {
                    assert_eq!(kernel_index(operation, flags, operation_count), None);
                }
                assert_eq!(kernel_index(u32::MAX, flags, operation_count), None);
            }
        }
    }
}

#[derive(Default, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
struct Parameters {
    pointers: [u64; 12],
    words: [u32; 32],
}

impl Parameters {
    fn precompute_chacha_columns(&mut self) {
        let constants = [0x61707865u32, 0x3320646e, 0x79622d32, 0x6b206574];
        for (column, constant) in constants.into_iter().enumerate().skip(1) {
            let mut first = constant;
            let mut second = self.words[8 + column];
            let mut third = self.words[12 + column];
            let mut fourth = 0u32;
            first = first.wrapping_add(second);
            fourth = (fourth ^ first).rotate_left(16);
            third = third.wrapping_add(fourth);
            second = (second ^ third).rotate_left(12);
            first = first.wrapping_add(second);
            fourth = (fourth ^ first).rotate_left(8);
            third = third.wrapping_add(fourth);
            second = (second ^ third).rotate_left(7);
            self.words[15 + column] = first;
            self.words[18 + column] = second;
            self.words[21 + column] = third;
            self.words[24 + column] = fourth;
        }
    }
}

trait Buffer {
    fn address(&self) -> u64;
}

trait Device {
    type Memory: Buffer;
    fn generation_threads(&self) -> usize;
    fn prefer_first_bitmap_summary(&self) -> bool {
        false
    }
    fn default_first_partitions(&self) -> usize {
        32
    }
    fn partition_second_threads(&self) -> usize {
        0
    }
    fn grouped_partition_scatter(&self) -> bool {
        false
    }
    fn local_csr_threads(&self) -> usize {
        0
    }
    fn local_coarse_bits(&self) -> u32 {
        if self.local_csr_threads() == 0 {
            LOCAL_COARSE_BITS
        } else {
            9
        }
    }
    #[cfg(test)]
    fn partition_second_kernel_threads(&self) -> usize;
    fn prefer_coarse_histogram(&self) -> bool {
        false
    }
    fn allocate(&self, words: usize) -> Result<Self::Memory, Error>;
    fn upload(&self, buffer: &Self::Memory, words: &[u32]) -> Result<(), Error>;
    fn download(&self, buffer: &Self::Memory, words: usize) -> Result<Vec<u32>, Error>;
    fn launch(&self, parameters: &Parameters, groups: usize) -> Result<(), Error>;
    fn launch_batch(&self, launches: &[(Parameters, usize)]) -> Result<(), Error> {
        for (parameters, groups) in launches {
            self.launch(parameters, *groups)?;
        }
        Ok(())
    }
}

pub enum Engine {
    #[cfg(feature = "cuda")]
    Cuda(cuda::Device),
    #[cfg(feature = "vulkan")]
    Vulkan(vulkan::Device),
}

impl Engine {
    pub fn cuda(device: usize) -> Result<Self, Error> {
        #[cfg(feature = "cuda")]
        {
            Ok(Self::Cuda(cuda::Device::new(device)?))
        }
        #[cfg(not(feature = "cuda"))]
        {
            let _ = device;
            Err(Error::new(
                ErrorKind::Unsupported,
                "GigaHorse CUDA support not compiled",
            ))
        }
    }

    pub fn vulkan(device: usize) -> Result<Self, Error> {
        #[cfg(feature = "vulkan")]
        {
            Ok(Self::Vulkan(vulkan::Device::new(device)?))
        }
        #[cfg(not(feature = "vulkan"))]
        {
            let _ = device;
            Err(Error::new(
                ErrorKind::Unsupported,
                "GigaHorse Vulkan support not compiled",
            ))
        }
    }

    pub fn reconstruct(
        &mut self,
        plot_id: &[u8; 32],
        bitmap: &[u64],
        memory_bytes: u64,
        check: &(impl Fn() -> Result<(), Error> + Sync),
    ) -> Result<Vec<C30Table5Entry>, Error> {
        match self {
            #[cfg(feature = "cuda")]
            Self::Cuda(device) => reconstruct(device, plot_id, bitmap, memory_bytes, check),
            #[cfg(feature = "vulkan")]
            Self::Vulkan(device) => reconstruct(device, plot_id, bitmap, memory_bytes, check),
        }
    }

    pub fn finish(
        &mut self,
        entries: Vec<C30Table5Entry>,
        challenge: &[u8; 32],
        check: &(impl Fn() -> Result<(), Error> + Sync),
    ) -> Result<Vec<Vec<u8>>, Error> {
        match self {
            #[cfg(feature = "cuda")]
            Self::Cuda(device) => finish(device, entries, challenge, check),
            #[cfg(feature = "vulkan")]
            Self::Vulkan(device) => finish(device, entries, challenge, check),
        }
    }
}

enum Histogram<Memory> {
    Fine(Memory),
    Coarse { counts: Memory, bits: u32 },
}

struct Partitions<Memory> {
    ranges: Memory,
    regions: usize,
    jobs: usize,
    f2_region_capacity: u32,
}

struct Table<Memory> {
    data: Memory,
    histogram: Option<Histogram<Memory>>,
    partitions: Option<Partitions<Memory>>,
    count: usize,
    number: u32,
}

struct Workspace<'device, Backend: Device> {
    device: &'device Backend,
    heads: Backend::Memory,
    status: Backend::Memory,
    leaves: u64,
}

fn validate_status(status: &[u32]) -> Result<(), Error> {
    if status[2] != 0 {
        return Err(Error::other(format!(
            "GigaHorse GPU capacity exceeded (code {})",
            status[2]
        )));
    }
    Ok(())
}

impl<'device, Backend: Device> Workspace<'device, Backend> {
    fn partition_second(
        &self,
        input: &Backend::Memory,
        count: usize,
        capacity: usize,
        workgroup_threads: usize,
        compact_f2: bool,
        check: &(impl Fn() -> Result<(), Error> + Sync),
    ) -> Result<Option<Table<Backend::Memory>>, Error> {
        const REGIONS: usize = 64;
        if ![128, 256, 512].contains(&workgroup_threads) {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "GigaHorse partitioned F2 workgroup size is unsupported",
            ));
        }
        let regional_capacity = capacity.div_ceil(REGIONS);
        let output_count = regional_capacity
            .checked_mul(REGIONS)
            .filter(|count| *count <= u32::MAX as usize)
            .ok_or_else(|| Error::other("GigaHorse partitioned F2 allocation exceeds limits"))?;
        let f2_region_capacity = if compact_f2 {
            regional_capacity as u32
        } else {
            0
        };
        let output = self
            .device
            .allocate(output_count * if f2_region_capacity == 0 { 4 } else { 3 })?;
        let ranges = self.device.allocate(REGIONS * 3)?;
        let coarse_bits = self.device.local_coarse_bits();
        let coarse_count = BUCKETS.div_ceil(1 << coarse_bits);
        let histogram = self.device.allocate(coarse_count)?;
        self.device.upload(&histogram, &vec![0; coarse_count])?;
        let metadata: Vec<_> = (0..REGIONS)
            .flat_map(|region| {
                [
                    (region * regional_capacity) as u32,
                    regional_capacity as u32,
                    0,
                ]
            })
            .collect();
        self.device.upload(&ranges, &metadata)?;
        self.device.upload(&self.status, &[0; 4])?;
        let mut parameters = Parameters::default();
        parameters.pointers[0] = input.address();
        parameters.pointers[1] = output.address();
        parameters.pointers[3] = histogram.address();
        parameters.pointers[5] = self.status.address();
        parameters.pointers[9] = ranges.address();
        parameters.words[0] = 17;
        parameters.words[1] = count as u32;
        parameters.words[2] = 2;
        parameters.words[5] = f2_region_capacity;
        parameters.words[7] = coarse_bits;
        let batch = 65535 * workgroup_threads;
        for base in (0..count).step_by(batch) {
            check()?;
            parameters.words[4] = base as u32;
            self.device.launch(
                &parameters,
                (count - base).min(batch).div_ceil(workgroup_threads),
            )?;
            let status = self.device.download(&self.status, 4)?;
            if status[2] == 4 {
                self.device.upload(&self.status, &[0; 4])?;
                return Ok(None);
            }
            validate_status(&status)?;
        }
        check()?;
        let metadata = self.device.download(&ranges, REGIONS * 3)?;
        let total: usize = metadata
            .as_chunks::<3>()
            .0
            .iter()
            .map(|range| range[2] as usize)
            .sum();
        if total != count
            || metadata
                .as_chunks::<3>()
                .0
                .iter()
                .enumerate()
                .any(|(region, range)| {
                    range[2] > range[1]
                        || range[0] as usize != region * regional_capacity
                        || range[1] as usize != regional_capacity
                })
        {
            return Err(Error::other(
                "GigaHorse partitioned F2 counts are inconsistent",
            ));
        }
        let jobs = metadata
            .as_chunks::<3>()
            .0
            .iter()
            .map(|range| (range[2] as usize).div_ceil(128))
            .sum();
        Ok(Some(Table {
            data: output,
            histogram: Some(Histogram::Coarse {
                counts: histogram,
                bits: coarse_bits,
            }),
            partitions: Some(Partitions {
                ranges,
                regions: REGIONS,
                jobs,
                f2_region_capacity,
            }),
            count,
            number: 2,
        }))
    }

    fn new(device: &'device Backend) -> Result<Self, Error> {
        Ok(Self {
            device,
            heads: device.allocate(BUCKETS)?,
            status: device.allocate(4)?,
            leaves: 0,
        })
    }

    fn status(&self) -> Result<Vec<u32>, Error> {
        let status = self.device.download(&self.status, 4)?;
        validate_status(&status)?;
        Ok(status)
    }

    fn next(
        &self,
        input: &Table<Backend::Memory>,
        capacity: usize,
        check: &(impl Fn() -> Result<(), Error> + Sync),
    ) -> Result<Table<Backend::Memory>, Error> {
        check()?;
        let started = std::time::Instant::now();
        let table = input.number + 1;
        let stride = if table == 2 { 4 } else { 8 };
        if input.count == 0 {
            return Ok(Table {
                data: self.device.allocate(1)?,
                histogram: None,
                partitions: None,
                count: 0,
                number: table,
            });
        }
        let fine_histogram = match input.histogram.as_ref() {
            Some(Histogram::Fine(histogram)) => Some(histogram),
            _ => None,
        };
        let coarse_histogram = match input.histogram.as_ref() {
            Some(Histogram::Coarse { counts, bits }) => Some((counts, *bits)),
            _ => None,
        };
        let partitions = input.partitions.as_ref();
        let local_csr = table == 3 && fine_histogram.is_none();
        let coarse_bits = if local_csr {
            coarse_histogram.map_or_else(|| self.device.local_coarse_bits(), |(_, bits)| bits)
        } else {
            COARSE_BITS
        };
        let coalesced_local = local_csr && coarse_bits == 9 && self.device.local_csr_threads() != 0;
        let coarse_count = BUCKETS.div_ceil(1 << coarse_bits);
        let allocated_coarse_histogram = if local_csr && coarse_histogram.is_none() {
            Some(self.device.allocate(coarse_count)?)
        } else {
            None
        };
        let coarse_histogram = coarse_histogram
            .map(|(counts, _)| counts)
            .or(allocated_coarse_histogram.as_ref());
        let allocated_links = if fine_histogram.is_none() {
            Some(
                self.device
                    .allocate(if table <= 3 { BUCKETS } else { input.count * 2 })?,
            )
        } else {
            None
        };
        let links = fine_histogram.or(allocated_links.as_ref()).unwrap();
        let packed = self
            .device
            .allocate(if table <= 3 { input.count * 2 } else { 1 })?;
        let coarse =
            self.device
                .allocate(if table <= 3 && (partitions.is_none() || local_csr) {
                    coarse_count * 2
                } else {
                    1
                })?;
        let staging =
            self.device
                .allocate(if table <= 3 && (partitions.is_none() || local_csr) {
                    input.count * 2
                } else {
                    1
                })?;
        let job_capacity = if local_csr {
            (input.count.div_ceil(128) + coarse_count)
                .max(partitions.map_or(0, |partitions| partitions.jobs))
        } else {
            partitions.map_or(input.count.div_ceil(128) + coarse_count, |partitions| {
                partitions.jobs
            })
        };
        let jobs = self
            .device
            .allocate(if table <= 3 { job_capacity * 3 } else { 1 })?;
        let matching_job_capacity = input.count.div_ceil(128) + BUCKETS.div_ceil(128);
        let matching_jobs = self.device.allocate(if table == 3 {
            matching_job_capacity * 3
        } else {
            1
        })?;
        let active = self.device.allocate(if table == 2 {
            input.count.min(BUCKETS)
        } else {
            1
        })?;
        log::debug!(
            "GigaHorse GPU F{table} allocation: {:.3}s",
            started.elapsed().as_secs_f64()
        );
        self.device.upload(&self.status, &[0; 4])?;
        let mut parameters = Parameters::default();
        parameters.pointers[..6].copy_from_slice(&[
            input.data.address(),
            0,
            self.heads.address(),
            links.address(),
            active.address(),
            self.status.address(),
        ]);
        parameters.pointers[10] = self.leaves;
        parameters.pointers[6] = packed.address();
        parameters.pointers[7] = coarse.address();
        parameters.pointers[8] = staging.address();
        parameters.pointers[9] = jobs.address();
        parameters.pointers[11] = matching_jobs.address();
        parameters.words[7] = coarse_bits;
        parameters.words[5] = partitions.map_or(0, |partitions| partitions.f2_region_capacity);
        parameters.words[1] = BUCKETS as u32;
        parameters.words[2] = input.number;
        let mut active_count = 0;
        let mut matching_count = 0;
        if let Some(partitions) = partitions.filter(|_| !local_csr) {
            parameters.words[0] = 9;
            let clear = (parameters, BUCKETS.div_ceil(128));
            parameters.words[0] = 13;
            parameters.words[1] = partitions.regions as u32;
            parameters.words[6] = 5;
            parameters.pointers[7] = partitions.ranges.address();
            let descriptors = (parameters, partitions.regions);
            parameters.words[0] = 10;
            parameters.words[1] = partitions.jobs as u32;
            self.device
                .launch_batch(&[clear, descriptors, (parameters, partitions.jobs)])?;
            active_count = self.status()?[1] as usize;
        } else if local_csr {
            parameters.pointers[9] = coarse_histogram.unwrap().address();
            parameters.words[6] = 3;
            if let Some(partitions) = partitions {
                let mut clear = parameters;
                clear.words[0] = 9;
                clear.words[1] = coarse_count as u32;
                clear.words[6] = 0;
                clear.pointers[3] = parameters.pointers[9];
                let mut descriptors = parameters;
                descriptors.words[0] = 13;
                descriptors.words[1] = partitions.regions as u32;
                descriptors.words[6] = 5;
                descriptors.pointers[7] = partitions.ranges.address();
                descriptors.pointers[9] = jobs.address();
                let mut histogram = parameters;
                histogram.words[0] = 10;
                histogram.words[1] = partitions.jobs as u32;
                histogram.words[6] = 6;
                histogram.pointers[9] = jobs.address();
                histogram.pointers[11] = coarse_histogram.unwrap().address();
                if allocated_coarse_histogram.is_some() {
                    self.device.launch_batch(&[
                        (clear, coarse_count.div_ceil(128)),
                        (descriptors, partitions.regions),
                        (histogram, partitions.jobs),
                    ])?;
                } else {
                    self.device.launch(&descriptors, partitions.regions)?;
                }
                self.status()?;
            } else if allocated_coarse_histogram.is_some() {
                let mut clear = parameters;
                clear.words[0] = 9;
                clear.words[1] = coarse_count as u32;
                clear.words[6] = 0;
                clear.pointers[3] = parameters.pointers[9];
                parameters.words[0] = 10;
                parameters.words[1] = input.count as u32;
                self.device.launch_batch(&[
                    (clear, coarse_count.div_ceil(128)),
                    (parameters, input.count.div_ceil(128)),
                ])?;
                self.status()?;
            }
            check()?;
            parameters.words[0] = 13;
            parameters.words[1] = BUCKETS as u32;
            let offsets = (parameters, coarse_count.div_ceil(128));
            parameters.pointers[9] = jobs.address();
            parameters.words[0] = 14;
            let scatter = if let Some(partitions) = partitions {
                parameters.words[1] = partitions.jobs as u32;
                parameters.words[6] = 6;
                if self.device.grouped_partition_scatter() {
                    parameters.words[0] = 19;
                    (parameters, partitions.jobs.div_ceil(16))
                } else {
                    (parameters, partitions.jobs)
                }
            } else {
                parameters.words[1] = input.count as u32;
                (parameters, input.count.div_ceil(128))
            };
            parameters.words[0] = if coalesced_local { 20 } else { 16 };
            parameters.words[1] = input.count as u32;
            parameters.words[6] = 3;
            let prefix = (parameters, coarse_count);
            parameters.words[0] = 12;
            parameters.words[6] = 4;
            if coalesced_local {
                self.device.launch_batch(&[offsets, scatter, prefix])?;
                let counts = self.status()?;
                matching_count = counts[1] as usize;
                if counts[0] != 0 {
                    self.device.launch(&parameters, counts[0] as usize)?;
                    self.status()?;
                }
            } else {
                self.device.launch_batch(&[
                    offsets,
                    scatter,
                    prefix,
                    (parameters, job_capacity),
                ])?;
                matching_count = self.status()?[1] as usize;
            }
            self.device.upload(&self.status, &[0; 4])?;
            log::debug!(
                "GigaHorse GPU F{table} local indexing: {:.3}s",
                started.elapsed().as_secs_f64()
            );
        } else {
            parameters.words[0] = if table <= 3 { 9 } else { 0 };
            parameters.words[6] = u32::from(fine_histogram.is_some()) * 2;
            let clear = (parameters, BUCKETS.div_ceil(128));
            check()?;
            parameters.words[0] = if table <= 3 { 10 } else { 2 };
            parameters.words[1] = input.count as u32;
            if fine_histogram.is_some() {
                self.device.launch(&clear.0, clear.1)?;
            } else {
                self.device
                    .launch_batch(&[clear, (parameters, input.count.div_ceil(128))])?;
            }
            active_count = self.status()?[1] as usize;
            log::debug!(
                "GigaHorse GPU F{table} histogram: {:.3}s",
                started.elapsed().as_secs_f64()
            );
        }
        if table <= 3 && !local_csr {
            parameters.words[0] = 11;
            let index_count = if table == 2 { active_count } else { BUCKETS };
            parameters.words[1] = index_count as u32;
            self.device.launch(&parameters, index_count.div_ceil(128))?;
            if table == 3 {
                matching_count = self.status()?[1] as usize;
            }
            self.device.upload(&self.status, &[0; 4])?;
            if let Some(partitions) = partitions {
                parameters.words[0] = 12;
                parameters.words[1] = partitions.jobs as u32;
                parameters.words[6] = 5;
                parameters.pointers[8] = input.data.address();
                self.device.launch(&parameters, partitions.jobs)?;
            } else {
                parameters.words[0] = 13;
                parameters.words[1] = BUCKETS as u32;
                let offsets = (parameters, coarse_count);
                parameters.words[0] = 14;
                parameters.words[1] = input.count as u32;
                let scatter = (parameters, input.count.div_ceil(128));
                parameters.words[0] = 12;
                parameters.words[1] = input.count as u32;
                self.device
                    .launch_batch(&[offsets, scatter, (parameters, job_capacity)])?;
            }
            self.status()?;
            log::debug!(
                "GigaHorse GPU F{table} indexing: {:.3}s",
                started.elapsed().as_secs_f64()
            );
        }
        drop(staging);
        drop(coarse);
        drop(jobs);
        let output = self.device.allocate(capacity.max(1) * stride)?;
        parameters.pointers[1] = output.address();
        parameters.pointers[7] = 0;
        parameters.pointers[8] = 0;
        parameters.pointers[9] = 0;
        parameters.words[0] = 3;
        parameters.words[1] = active_count as u32;
        parameters.words[2] = table;
        parameters.words[3] = capacity as u32;
        parameters.words[6] = matching_count as u32;
        let (work_count, batch) = if table == 2 {
            (active_count, MATCHING_BATCH)
        } else if table == 3 {
            (matching_count, MATCHING_BATCH)
        } else {
            (input.count, GENERATION_BATCH)
        };
        if table != 2 {
            parameters.words[0] = if table == 3 { 15 } else { 7 };
            parameters.words[1] = input.count as u32;
        }
        for base in (0..work_count).step_by(batch) {
            check()?;
            parameters.words[4] = base as u32;
            let work = (work_count - base).min(batch);
            self.device.launch(
                &parameters,
                if table <= 3 { work } else { work.div_ceil(128) },
            )?;
            self.status()?;
        }
        let count = self.status()?[0] as usize;
        drop(packed);
        drop(active);
        if table == 2 && self.device.partition_second_threads() != 0 {
            if let Some(partitioned) = self.partition_second(
                &output,
                count,
                capacity,
                self.device.partition_second_threads(),
                parse_bool_override(
                    std::env::var_os("GH_GPU_F2_SOA").as_deref(),
                    "GH_GPU_F2_SOA",
                )?
                .unwrap_or(true),
                check,
            )? {
                log::debug!(
                    "GigaHorse GPU F2 partitioned: {count} entries in {:.3}s",
                    started.elapsed().as_secs_f64()
                );
                return Ok(partitioned);
            }
            log::warn!("GigaHorse F2 partition capacity exceeded; retrying unpartitioned hashing");
        }
        let histogram = if table == 2 {
            let coarse_histogram = self.device.prefer_coarse_histogram();
            let coarse_bits = self.device.local_coarse_bits();
            let histogram_count = if coarse_histogram {
                BUCKETS.div_ceil(1 << coarse_bits)
            } else {
                BUCKETS
            };
            let histogram = self.device.allocate(histogram_count)?;
            let mut clear = parameters;
            clear.words[0] = 9;
            clear.words[1] = histogram_count as u32;
            clear.words[6] = 0;
            clear.pointers[3] = histogram.address();
            self.device.launch(&clear, histogram_count.div_ceil(128))?;
            parameters.pointers[9] = histogram.address();
            parameters.words[6] = if coarse_histogram { 3 } else { 0 };
            if coarse_histogram {
                parameters.words[7] = coarse_bits;
                Some(Histogram::Coarse {
                    counts: histogram,
                    bits: coarse_bits,
                })
            } else {
                Some(Histogram::Fine(histogram))
            }
        } else {
            None
        };
        if table <= 3 {
            log::debug!(
                "GigaHorse GPU F{table} matching: {:.3}s",
                started.elapsed().as_secs_f64()
            );
            parameters.words[0] = 8;
            parameters.words[1] = count as u32;
            for base in (0..count).step_by(GENERATION_BATCH) {
                check()?;
                parameters.words[4] = base as u32;
                self.device.launch(
                    &parameters,
                    (count - base).min(GENERATION_BATCH).div_ceil(128),
                )?;
            }
        }
        self.status()?;
        log::debug!(
            "GigaHorse GPU F{table}: {count} entries in {:.3}s",
            started.elapsed().as_secs_f64()
        );
        Ok(Table {
            data: output,
            histogram,
            partitions: None,
            count,
            number: table,
        })
    }
}

fn first_partition_ranges(bitmap: &[u64], regions: usize) -> Vec<u32> {
    let mut ranges = Vec::with_capacity(regions * 3);
    let mut offset = 0u32;
    let mut previous = false;
    let bits_per_region = 524288 / regions;
    for partition in 0..regions {
        let mut expected = 0u32;
        for index in partition * bits_per_region..(partition + 1) * bits_per_region {
            let selected = bitmap[index / 64] & (1u64 << (index % 64)) != 0;
            expected += if selected {
                8192
            } else if previous {
                512
            } else {
                0
            };
            previous = selected;
        }
        let capacity = if expected == 0 {
            0
        } else {
            (expected * 5 / 4).max(65536)
        };
        ranges.extend_from_slice(&[offset, capacity, 0]);
        offset += capacity;
    }
    ranges
}

fn first_bitmap_summary(words: &[u32]) -> Vec<u32> {
    words
        .chunks(4)
        .map(|chunk| {
            chunk.iter().enumerate().fold(0, |packed, (index, word)| {
                let summary = match word.count_ones() {
                    0 => 0,
                    1 => word.trailing_zeros() + 1,
                    _ => 33,
                };
                packed | summary << (index * 8)
            })
        })
        .collect()
}

#[test]
fn first_bitmap_summary_encodes_empty_single_and_multiple_bits() {
    assert_eq!(first_bitmap_summary(&[]), Vec::<u32>::new());
    assert_eq!(first_bitmap_summary(&[0, 1, 1 << 31, 3]), [0x21200100]);
    assert_eq!(first_bitmap_summary(&[u32::MAX]), [33]);
    for bit in 0..32 {
        assert_eq!(first_bitmap_summary(&[1 << bit]), [bit + 1]);
    }
}

#[test]
fn first_bitmap_summary_preserves_all_bucket_boundaries() {
    let mut seed = 0x59c31b8du32;
    let mut words: Vec<u32> = (0..16384)
        .map(|index| {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            match index % 4 {
                0 => 0,
                1 => 1 << (index / 4 % 32),
                2 => (1 << (index / 4 % 32)) | (1 << ((index / 4 + 1) % 32)),
                _ => seed,
            }
        })
        .collect();
    words[0] = 0;
    words[255] = 1 << 31;
    words[256] = 0;
    words[511] = 1 << 31;
    words[512] = 0;
    words[16383] = 1 << 31;
    let summaries = first_bitmap_summary(&words);
    assert_eq!(summaries.len(), 4096);
    for bucket in 0..524288u32 {
        let index = (bucket / 32) as usize;
        let bit = bucket % 32;
        let summary = (summaries[index / 4] >> ((index % 4) * 8)) & 255;
        let word = if summary > 32 {
            words[index]
        } else if summary == 0 {
            0
        } else {
            1 << (summary - 1)
        };
        assert_eq!(word, words[index]);
        for remainder in [0, 511, 512, 8191] {
            let expected = words[index] & (1 << bit) != 0
                || (bucket != 0
                    && remainder < 512
                    && words[((bucket - 1) / 32) as usize] & (1 << ((bucket - 1) % 32)) != 0);
            let mut actual = word & (1 << bit) != 0;
            if !actual && bucket != 0 && remainder < 512 {
                let previous = if bit == 0 { words[index - 1] } else { word };
                actual = previous & (1 << ((bit + 31) & 31)) != 0;
            }
            assert_eq!(
                actual, expected,
                "bitmap bucket={bucket}, remainder={remainder}"
            );
        }
    }
}

#[test]
fn first_partition_layout_preserves_boundary_overlap() {
    for regions in [32, 64] {
        let mut bitmap = vec![0u64; 8192];
        let words_per_region = bitmap.len() / regions;
        let ranges = first_partition_ranges(&bitmap, regions);
        assert_eq!(ranges.len(), regions * 3);
        assert!(ranges.iter().all(|word| *word == 0));
        bitmap[words_per_region - 1] = 1u64 << 63;
        let ranges = first_partition_ranges(&bitmap, regions);
        assert_eq!(&ranges[..6], &[0, 65536, 0, 65536, 65536, 0]);
        assert!(
            ranges.as_chunks::<3>().0[2..]
                .iter()
                .all(|range| range[1] == 0)
        );
        bitmap.fill(0);
        bitmap[..words_per_region].fill(u64::MAX);
        let ranges = first_partition_ranges(&bitmap, regions);
        let first_capacity = words_per_region as u32 * 64 * 8192 * 5 / 4;
        assert_eq!(
            &ranges[..6],
            &[0, first_capacity, 0, first_capacity, 65536, 0]
        );
        bitmap.fill(0);
        bitmap[8191] = 1u64 << 63;
        let ranges = first_partition_ranges(&bitmap, regions);
        assert!(
            ranges.as_chunks::<3>().0[..regions - 1]
                .iter()
                .all(|range| range[1] == 0)
        );
        assert_eq!(&ranges[(regions - 1) * 3..], &[0, 65536, 0]);
        bitmap.fill(0);
        for index in (0..524288).step_by(16) {
            bitmap[index / 64] |= 1 << (index % 64);
        }
        let ranges = first_partition_ranges(&bitmap, regions);
        for pair in ranges.as_chunks::<3>().0.windows(2) {
            assert_eq!(pair[0][0] + pair[0][1], pair[1][0]);
        }
        assert!(
            ranges
                .as_chunks::<3>()
                .0
                .iter()
                .all(|range| range[1] > 0 && range[2] == 0)
        );
    }
}

#[test]
fn first_bucket_division_preserves_full_width() {
    let mut hashes = vec![0, 1, 15112, 15113, 15114, u32::MAX];
    for partition_index in 1..32u32 {
        hashes.extend([partition_index * (1 << 27) - 1, partition_index * (1 << 27)]);
    }
    let mut seed = 0xa182b33du32;
    for _ in 0..65536 {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        hashes.push(seed);
    }
    for hash in hashes {
        for high_x in [0, 1, 31, 63] {
            let quotient = hash / 15113;
            let remainder = hash - quotient * 15113;
            let bucket = quotient * 64 + (remainder * 64 + high_x) / 15113;
            assert_eq!(
                u64::from(bucket),
                ((u64::from(hash) << 6) | u64::from(high_x)) / 15113
            );
        }
    }
}

fn generate_first<Backend: Device>(
    workspace: &Workspace<'_, Backend>,
    plot_id: &[u8; 32],
    bitmap: &Backend::Memory,
    bitmap_summary: bool,
    capacity: usize,
    partition_ranges: Option<&[u32]>,
    check: &(impl Fn() -> Result<(), Error> + Sync),
) -> Result<Table<Backend::Memory>, Error> {
    let device = workspace.device;
    if partition_ranges.is_some_and(|ranges| ![96, 192].contains(&ranges.len())) {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "Invalid GigaHorse F1 partition layout",
        ));
    }
    let regional_capacity = partition_ranges.map_or(0, |ranges| {
        ranges[ranges.len() - 3] as usize + ranges[ranges.len() - 2] as usize
    });
    let first = device.allocate(capacity.max(regional_capacity) * 2)?;
    let mut partitions = if let Some(ranges) = partition_ranges {
        let memory = device.allocate(ranges.len())?;
        device.upload(&memory, ranges)?;
        Some(Partitions {
            ranges: memory,
            regions: ranges.len() / 3,
            jobs: 0,
            f2_region_capacity: 0,
        })
    } else {
        None
    };
    let mut parameters = Parameters::default();
    parameters.pointers[1] = first.address();
    parameters.pointers[2] = workspace.heads.address();
    parameters.pointers[5] = workspace.status.address();
    parameters.pointers[6] = bitmap.address();
    parameters.words[3] = capacity as u32;
    let mut key = [0; 32];
    key[0] = 1;
    key[1..].copy_from_slice(&plot_id[..31]);
    for (index, bytes) in key.as_chunks::<4>().0.iter().enumerate() {
        parameters.words[index + 8] = u32::from_le_bytes(*bytes);
    }
    parameters.precompute_chacha_columns();
    loop {
        device.upload(&workspace.status, &[0; 4])?;
        parameters.words[5] = u32::from(bitmap_summary)
            | if partitions
                .as_ref()
                .is_some_and(|partitions| partitions.regions == 64)
            {
                2
            } else {
                0
            };
        let histogram = if let Some(partitions) = partitions.as_ref() {
            parameters.pointers[3] = partitions.ranges.address();
            parameters.words[0] = 18;
            None
        } else {
            let histogram = device.allocate(BUCKETS)?;
            parameters.pointers[3] = histogram.address();
            parameters.words[0] = 9;
            parameters.words[1] = BUCKETS as u32;
            device.launch(&parameters, BUCKETS.div_ceil(128))?;
            parameters.words[0] = 1;
            Some(Histogram::Fine(histogram))
        };
        let mut launches = Vec::with_capacity(4);
        let mut overflow = false;
        let mut count = 0;
        for base in (0..1usize << 28).step_by(GENERATION_BATCH) {
            check()?;
            let work = ((1usize << 28) - base).min(GENERATION_BATCH);
            parameters.words[1] = work as u32;
            parameters.words[4] = base as u32;
            launches.push((parameters, work.div_ceil(device.generation_threads())));
            if launches.len() == 4 || base + work == 1usize << 28 {
                device.launch_batch(&launches)?;
                let status = device.download(&workspace.status, 4)?;
                if status[2] == 4 && partitions.is_some() {
                    overflow = true;
                    break;
                }
                validate_status(&status)?;
                count = status[0] as usize;
                launches.clear();
            }
        }
        if overflow {
            log::warn!(
                "GigaHorse F1 partition capacity exceeded; retrying unpartitioned generation"
            );
            partitions = None;
            continue;
        }
        if let Some(partitions) = partitions.as_mut() {
            let ranges = device.download(&partitions.ranges, partitions.regions * 3)?;
            let total: usize = ranges
                .as_chunks::<3>()
                .0
                .iter()
                .map(|range| range[2] as usize)
                .sum();
            if total > regional_capacity
                || ranges
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .any(|range| range[2] > range[1])
            {
                return Err(Error::other(
                    "GigaHorse partitioned F1 counts are inconsistent",
                ));
            }
            partitions.jobs = ranges
                .as_chunks::<3>()
                .0
                .iter()
                .map(|range| (range[2] as usize).div_ceil(128))
                .sum();
            count = total;
        }
        return Ok(Table {
            data: first,
            histogram,
            partitions,
            count,
            number: 1,
        });
    }
}

fn reconstruct<Backend: Device>(
    device: &Backend,
    plot_id: &[u8; 32],
    bitmap: &[u64],
    memory_bytes: u64,
    check: &(impl Fn() -> Result<(), Error> + Sync),
) -> Result<Vec<C30Table5Entry>, Error> {
    check()?;
    if bitmap.len() != 8192 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "GigaHorse C30 bitmap requires 524288 bits",
        ));
    }
    let selected: usize = bitmap.iter().map(|word| word.count_ones() as usize).sum();
    if selected > 32768 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "GigaHorse bitmap exceeds reconstruction limits",
        ));
    }
    if selected == 0 {
        return Ok(Vec::new());
    }
    let capacity = (selected * 8192 * 17 / 16 * 5 / 4).max(65536);
    let regions = parse_first_partitions(
        std::env::var_os("GH_GPU_F1_PARTITIONS").as_deref(),
        device.default_first_partitions(),
    )?;
    let partition_ranges = (regions != 0).then(|| first_partition_ranges(bitmap, regions));
    let regional_capacity = partition_ranges.as_ref().map_or(0, |ranges| {
        ranges[ranges.len() - 3] as usize + ranges[ranges.len() - 2] as usize
    });
    let required =
        capacity.max(regional_capacity) as u64 * 40 + BUCKETS as u64 * 16 + 512 * 1024 * 1024;
    if required > memory_bytes {
        return Err(Error::other(format!(
            "GigaHorse GPU needs at least {required} bytes for this bitmap"
        )));
    }
    let workspace = Workspace::new(device)?;
    let started = std::time::Instant::now();
    let bitmap_summary = parse_bool_override(
        std::env::var_os("GH_GPU_F1_BITMAP_SUMMARY").as_deref(),
        "GH_GPU_F1_BITMAP_SUMMARY",
    )?
    .unwrap_or_else(|| device.prefer_first_bitmap_summary());
    let mut bitmap_words: Vec<_> = bitmap
        .iter()
        .flat_map(|word| [*word as u32, (word >> 32) as u32])
        .collect();
    if bitmap_summary {
        bitmap_words.extend(first_bitmap_summary(&bitmap_words));
    }
    let bitmap_gpu = device.allocate(bitmap_words.len())?;
    device.upload(&bitmap_gpu, &bitmap_words)?;
    let first = generate_first(
        &workspace,
        plot_id,
        &bitmap_gpu,
        bitmap_summary,
        capacity,
        partition_ranges.as_deref(),
        check,
    )?;
    log::debug!(
        "GigaHorse GPU F1: {} entries in {:.3}s",
        first.count,
        started.elapsed().as_secs_f64()
    );
    let second = workspace.next(&first, capacity, check)?;
    drop(first);
    let third = workspace.next(&second, (second.count / 16).max(65536), check)?;
    let fourth = workspace.next(&third, (third.count / 64).max(65536), check)?;
    let fifth = workspace.next(&fourth, (fourth.count * 2).max(65536), check)?;
    if fifth.count == 0 {
        return Ok(Vec::new());
    }
    if fifth.count > 65536 {
        return Err(Error::other("GigaHorse GPU F5 output exceeds limit"));
    }
    let output = device.allocate(fifth.count * 22)?;
    let mut parameters = Parameters::default();
    parameters.words[0] = 4;
    parameters.words[1] = fifth.count as u32;
    parameters.words[5] = second
        .partitions
        .as_ref()
        .map_or(0, |partitions| partitions.f2_region_capacity);
    parameters.pointers[0] = fifth.data.address();
    parameters.pointers[1] = output.address();
    parameters.pointers[7] = second.data.address();
    parameters.pointers[8] = third.data.address();
    parameters.pointers[9] = fourth.data.address();
    check()?;
    device.launch(&parameters, fifth.count.div_ceil(128))?;
    check()?;
    let output = device.download(&output, fifth.count * 22)?;
    Ok(output
        .as_chunks::<22>()
        .0
        .iter()
        .map(|words| {
            let mut metadata = [0; 16];
            for (bytes, word) in metadata.as_chunks_mut::<4>().0.iter_mut().zip(&words[2..6]) {
                bytes.copy_from_slice(&word.to_be_bytes());
            }
            C30Table5Entry {
                f_value: u64::from(words[0]) | u64::from(words[1]) << 32,
                metadata,
                xs: words[6..22].try_into().unwrap(),
            }
        })
        .collect())
}

fn finish<Backend: Device>(
    device: &Backend,
    entries: Vec<C30Table5Entry>,
    challenge: &[u8; 32],
    check: &(impl Fn() -> Result<(), Error> + Sync),
) -> Result<Vec<Vec<u8>>, Error> {
    check()?;
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    if entries.len() > 262144 {
        return Err(Error::other("GigaHorse GPU proof candidate limit exceeded"));
    }
    if entries.iter().any(|entry| entry.f_value >= 1 << 38) {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "GigaHorse GPU F5 value exceeds 38 bits",
        ));
    }
    let mut nodes = Vec::with_capacity(entries.len() * 8);
    let mut leaves = Vec::with_capacity(entries.len() * 22);
    for entry in &entries {
        let mut node = [0; 8];
        node[0] = entry.f_value as u32;
        node[1] = (entry.f_value >> 32) as u32;
        for (index, bytes) in entry.metadata.as_chunks::<4>().0.iter().enumerate() {
            node[index + 2] = u32::from_be_bytes(*bytes);
        }
        nodes.extend_from_slice(&node);
        leaves.extend_from_slice(&node[..6]);
        leaves.extend_from_slice(&entry.xs);
    }
    let data = device.allocate(nodes.len())?;
    device.upload(&data, &nodes)?;
    let leaf_data = device.allocate(leaves.len())?;
    device.upload(&leaf_data, &leaves)?;
    let mut workspace = Workspace::new(device)?;
    workspace.leaves = leaf_data.address();
    let fifth = Table {
        partitions: None,
        data,
        histogram: None,
        count: entries.len(),
        number: 5,
    };
    let sixth = workspace.next(&fifth, entries.len() * 4, check)?;
    let seventh = workspace.next(&sixth, (sixth.count * 4).max(1024), check)?;
    let output = device.allocate(1024 * 64)?;
    device.upload(&workspace.status, &[0; 4])?;
    let mut parameters = Parameters::default();
    parameters.pointers[0] = seventh.data.address();
    parameters.pointers[1] = output.address();
    parameters.pointers[5] = workspace.status.address();
    parameters.pointers[7] = sixth.data.address();
    parameters.pointers[10] = leaf_data.address();
    parameters.words[0] = 5;
    parameters.words[1] = seventh.count as u32;
    parameters.words[3] = 1024;
    parameters.words[5] = u32::from_be_bytes(challenge[..4].try_into().unwrap());
    check()?;
    if seventh.count != 0 {
        device.launch(&parameters, seventh.count.div_ceil(128))?;
    }
    check()?;
    let count = workspace.status()?[0] as usize;
    Ok(device
        .download(&output, count * 64)?
        .as_chunks::<64>()
        .0
        .iter()
        .map(|words| words.iter().flat_map(|word| word.to_be_bytes()).collect())
        .collect())
}
