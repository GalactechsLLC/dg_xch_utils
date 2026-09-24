use super::Parameters;
use cudarc::driver::{
    CudaDevice, CudaFunction, CudaSlice, DevicePtr, DeviceSlice, LaunchAsync, LaunchConfig, sys,
};
use parking_lot::Mutex;
use std::ffi::OsStr;
use std::io::{Error, ErrorKind};
use std::sync::Arc;
use std::time::Instant;

const PROFILE_BATCH_CAPACITY: usize = 16;
const MAXIMUM_RETAINED_BYTES: u64 = 12 * 1024 * 1024 * 1024;
const KERNEL_NAMES: [&str; 22] = [
    "gh_kernel_0",
    "gh_kernel_1",
    "gh_kernel_2",
    "gh_kernel_3",
    "gh_kernel_4",
    "gh_kernel_5",
    "gh_kernel_6",
    "gh_kernel_7",
    "gh_kernel_8",
    "gh_kernel_9",
    "gh_kernel_10",
    "gh_kernel_11",
    "gh_kernel_12",
    "gh_kernel_13",
    "gh_kernel_14",
    "gh_kernel_15",
    "gh_kernel_16",
    "gh_kernel_17",
    "gh_kernel_18",
    "gh_kernel_19",
    "gh_kernel_20",
    "gh_kernel_21",
];

pub struct Device {
    device: Arc<CudaDevice>,
    launch_lock: Mutex<()>,
    functions: Vec<CudaFunction>,
    dense_threads: u32,
    generation_threads: u32,
    partition_second: bool,
    partition_threads: u32,
    grouped_partition_scatter: bool,
    local_csr_threads: u32,
    pool: Option<Arc<MemoryPool>>,
    profile: Option<Profile>,
}

unsafe impl cudarc::driver::DeviceRepr for Parameters {}

pub struct Memory {
    data: CudaSlice<u32>,
    _pool: Option<Arc<MemoryPool>>,
}

struct MemoryPool {
    device: Arc<CudaDevice>,
    handle: sys::CUmemoryPool,
}

#[derive(Clone, Copy, Default)]
struct KernelTiming {
    launches: u64,
    milliseconds: f64,
}

struct ProfileSummary {
    kernels: [KernelTiming; KERNEL_NAMES.len() * 8],
    host_milliseconds: [f64; 4],
}

struct Profile {
    device: Arc<CudaDevice>,
    events: Vec<sys::CUevent>,
    summary: Mutex<ProfileSummary>,
}

unsafe impl Send for Profile {}
unsafe impl Sync for Profile {}

struct ProfileTimer<'profile> {
    profile: &'profile Profile,
    category: usize,
    started: Instant,
}

impl Profile {
    fn new(device: &Arc<CudaDevice>) -> Result<Self, Error> {
        let mut profile = Self {
            device: device.clone(),
            events: Vec::with_capacity(PROFILE_BATCH_CAPACITY * 2),
            summary: Mutex::new(ProfileSummary {
                kernels: [KernelTiming::default(); KERNEL_NAMES.len() * 8],
                host_milliseconds: [0.0; 4],
            }),
        };
        for _ in 0..PROFILE_BATCH_CAPACITY * 2 {
            profile.events.push(
                cudarc::driver::result::event::create(sys::CUevent_flags::CU_EVENT_DEFAULT)
                    .map_err(Error::other)?,
            );
        }
        Ok(profile)
    }

    fn timer(&self, category: usize) -> ProfileTimer<'_> {
        ProfileTimer {
            profile: self,
            category,
            started: Instant::now(),
        }
    }

    fn record(&self, slot: usize, finished: bool) -> Result<(), Error> {
        unsafe {
            cudarc::driver::result::event::record(
                self.events[slot * 2 + usize::from(finished)],
                *self.device.cu_stream(),
            )
            .map_err(Error::other)
        }
    }

    fn collect(&self, batch: &[(Parameters, usize)]) -> Result<(), Error> {
        let mut summary = self.summary.lock();
        for (slot, (parameters, groups)) in batch.iter().enumerate() {
            if *groups == 0 {
                continue;
            }
            let milliseconds = unsafe {
                cudarc::driver::result::event::elapsed(
                    self.events[slot * 2],
                    self.events[slot * 2 + 1],
                )
                .map_err(Error::other)?
            };
            let operation = parameters.words[0] as usize;
            let table = parameters.words[2].min(7) as usize;
            let timing = &mut summary.kernels[operation * 8 + table];
            timing.launches += 1;
            timing.milliseconds += f64::from(milliseconds);
        }
        Ok(())
    }
}

impl Drop for ProfileTimer<'_> {
    fn drop(&mut self) {
        self.profile.summary.lock().host_milliseconds[self.category] +=
            self.started.elapsed().as_secs_f64() * 1000.0;
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        let summary = self.summary.get_mut();
        for (index, timing) in summary.kernels.iter().enumerate() {
            if timing.launches > 0 {
                log::info!(
                    "GigaHorse CUDA profile op={}, table_arg={}: {} launches, GPU {:.3} ms, average {:.3} ms",
                    index / 8,
                    index % 8,
                    timing.launches,
                    timing.milliseconds,
                    timing.milliseconds / timing.launches as f64
                );
            }
        }
        log::info!(
            "GigaHorse CUDA profile host: allocate {:.3} ms, upload {:.3} ms, readback {:.3} ms, dispatch+wait {:.3} ms; GPU kernels {:.3} ms",
            summary.host_milliseconds[0],
            summary.host_milliseconds[1],
            summary.host_milliseconds[2],
            summary.host_milliseconds[3],
            summary
                .kernels
                .iter()
                .map(|timing| timing.milliseconds)
                .sum::<f64>()
        );
        if let Err(error) = self.device.bind_to_thread() {
            log::warn!("GigaHorse CUDA profiling cleanup could not bind device: {error}");
            return;
        }
        for event in &self.events {
            if let Err(error) = unsafe { cudarc::driver::result::event::destroy(*event) } {
                log::warn!("GigaHorse CUDA profiling event cleanup failed: {error}");
            }
        }
    }
}

unsafe impl Send for MemoryPool {}
unsafe impl Sync for MemoryPool {}

impl MemoryPool {
    fn new(device: &Arc<CudaDevice>) -> Result<Option<Arc<Self>>, Error> {
        if device
            .attribute(sys::CUdevice_attribute::CU_DEVICE_ATTRIBUTE_MEMORY_POOLS_SUPPORTED)
            .map_err(Error::other)?
            == 0
        {
            return Ok(None);
        }
        let properties = sys::CUmemPoolProps {
            allocType: sys::CUmemAllocationType::CU_MEM_ALLOCATION_TYPE_PINNED,
            handleTypes: sys::CUmemAllocationHandleType::CU_MEM_HANDLE_TYPE_NONE,
            location: sys::CUmemLocation {
                type_: sys::CUmemLocationType::CU_MEM_LOCATION_TYPE_DEVICE,
                id: *device.cu_device(),
            },
            ..Default::default()
        };
        let mut handle = std::ptr::null_mut();
        unsafe {
            sys::lib()
                .cuMemPoolCreate(&mut handle, &properties)
                .result()
                .map_err(Error::other)?;
        }
        let pool = Arc::new(Self {
            device: device.clone(),
            handle,
        });
        let (_, total_bytes) = cudarc::driver::result::mem_get_info().map_err(Error::other)?;
        let mut retained_bytes = MAXIMUM_RETAINED_BYTES.min(total_bytes as u64 / 4 * 3);
        unsafe {
            sys::lib()
                .cuMemPoolSetAttribute(
                    handle,
                    sys::CUmemPool_attribute::CU_MEMPOOL_ATTR_RELEASE_THRESHOLD,
                    std::ptr::from_mut(&mut retained_bytes).cast(),
                )
                .result()
                .map_err(Error::other)?;
        }
        log::debug!(
            "GigaHorse CUDA private memory pool release threshold: {} MiB",
            retained_bytes / (1024 * 1024)
        );
        Ok(Some(pool))
    }

    fn allocate(self: &Arc<Self>, words: usize) -> Result<Memory, Error> {
        self.device.bind_to_thread().map_err(Error::other)?;
        let bytes = words
            .checked_mul(std::mem::size_of::<u32>())
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidInput,
                    "GigaHorse CUDA allocation size overflow",
                )
            })?;
        let mut address = 0;
        unsafe {
            sys::lib()
                .cuMemAllocFromPoolAsync(&mut address, bytes, self.handle, *self.device.cu_stream())
                .result()
                .map_err(Error::other)?;
            Ok(Memory {
                data: self.device.upgrade_device_ptr(address, words),
                _pool: Some(self.clone()),
            })
        }
    }
}

impl Drop for MemoryPool {
    fn drop(&mut self) {
        if let Err(error) = self.device.bind_to_thread() {
            log::warn!("GigaHorse CUDA memory pool cleanup could not bind device: {error}");
            return;
        }
        if let Err(error) = self.device.synchronize() {
            log::warn!("GigaHorse CUDA memory pool cleanup synchronization failed: {error}");
        }
        unsafe {
            if let Err(error) = sys::lib().cuMemPoolTrimTo(self.handle, 0).result() {
                log::warn!("GigaHorse CUDA memory pool trim failed: {error}");
            }
            if let Err(error) = sys::lib().cuMemPoolDestroy(self.handle).result() {
                log::warn!("GigaHorse CUDA memory pool destruction failed: {error}");
            }
        }
    }
}

impl super::Buffer for Memory {
    fn address(&self) -> u64 {
        *self.data.device_ptr()
    }
}

impl Device {
    pub fn new(ordinal: usize) -> Result<Self, Error> {
        let local_csr_value = std::env::var_os("GH_CUDA_LOCAL_CSR_THREADS");
        let local_csr_override = parse_local_csr_threads(local_csr_value.as_deref())?;
        let grouped_override =
            parse_grouped_partition_scatter(std::env::var_os("GH_CUDA_GROUPED_F2").as_deref())?;
        let partition_override =
            parse_partition_second(std::env::var_os("GH_CUDA_DIRECT_F2").as_deref())?;
        let device = CudaDevice::new(ordinal).map_err(Error::other)?;
        let major = device
            .attribute(sys::CUdevice_attribute::CU_DEVICE_ATTRIBUTE_COMPUTE_CAPABILITY_MAJOR)
            .map_err(Error::other)?;
        let minor = device
            .attribute(sys::CUdevice_attribute::CU_DEVICE_ATTRIBUTE_COMPUTE_CAPABILITY_MINOR)
            .map_err(Error::other)?;
        let mut options = vec![format!("--gpu-architecture=compute_{major}{minor}")];
        if parse_dense_targets(std::env::var_os("GH_CUDA_DENSE_TARGETS").as_deref())? {
            options.push("-DGH_CUDA_DENSE_TARGETS=8".to_owned());
        }
        if super::parse_bool_override(
            std::env::var_os("GH_CUDA_PACKED_DENSE").as_deref(),
            "GH_CUDA_PACKED_DENSE",
        )?
        .unwrap_or(true)
        {
            options.push("-DGH_CUDA_PACKED_DENSE=1".to_owned());
        }
        let ptx = cudarc::nvrtc::compile_ptx_with_opts(
            include_str!("kernel.inc"),
            cudarc::nvrtc::CompileOptions {
                options,
                ..Default::default()
            },
        )
        .map_err(Error::other)?;
        device
            .load_ptx(ptx, "gigahorse", &KERNEL_NAMES)
            .map_err(Error::other)?;
        let functions = KERNEL_NAMES
            .iter()
            .map(|name| {
                device
                    .get_func("gigahorse", name)
                    .ok_or_else(|| Error::other(format!("missing GigaHorse CUDA kernel {name}")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let maximum_threads = device
            .attribute(sys::CUdevice_attribute::CU_DEVICE_ATTRIBUTE_MAX_THREADS_PER_BLOCK)
            .map_err(Error::other)?
            .min(
                device
                    .attribute(sys::CUdevice_attribute::CU_DEVICE_ATTRIBUTE_MAX_BLOCK_DIM_X)
                    .map_err(Error::other)?,
            ) as u32;
        let maximum_shared = device
            .attribute(sys::CUdevice_attribute::CU_DEVICE_ATTRIBUTE_MAX_SHARED_MEMORY_PER_BLOCK)
            .map_err(Error::other)? as u32;
        let supports_threads = |operation: usize, threads: u32| -> Result<bool, Error> {
            let shared_bytes = match operation {
                19 => 46_080,
                20 => 35_336,
                _ => 0,
            };
            Ok(threads <= maximum_threads
                && shared_bytes <= maximum_shared
                && functions[operation]
                    .occupancy_max_active_blocks_per_multiprocessor(threads, 0, None)
                    .map_err(Error::other)?
                    != 0)
        };
        let local_csr_threads = if local_csr_override == Some(0) {
            0
        } else {
            super::select_threads(
                local_csr_value.as_deref(),
                "GH_CUDA_LOCAL_CSR_THREADS",
                &[256, 512],
                &[512, 256],
                |threads| supports_threads(20, threads),
            )?
            .unwrap_or(0)
        };
        let partition_threads = super::select_threads(
            None,
            "CUDA partitioned F2 workgroup",
            &[128, 256, 512],
            &[512, 256, 128],
            |threads| supports_threads(17, threads),
        )?;
        let partition_second = super::select_feature(
            partition_override,
            partition_threads.is_some(),
            "CUDA partitioned F2",
        )?;
        let partition_threads = partition_threads.unwrap_or(128);
        let dense_threads = super::select_threads(
            std::env::var_os("GH_CUDA_DENSE_THREADS").as_deref(),
            "GH_CUDA_DENSE_THREADS",
            &[128, 256, 384, 512],
            &[384, 256, 128],
            |threads| supports_threads(3, threads),
        )?
        .ok_or_else(|| Error::other("GigaHorse CUDA dense workgroup exceeds kernel limits"))?;
        let generation_threads = super::select_threads(
            std::env::var_os("GH_CUDA_F1_THREADS").as_deref(),
            "GH_CUDA_F1_THREADS",
            &[128, 256, 512],
            &[512, 256, 128],
            |threads| {
                Ok(supports_threads(1, threads)?
                    && supports_threads(18, threads)?
                    && supports_threads(21, threads)?)
            },
        )?
        .ok_or_else(|| Error::other("GigaHorse CUDA F1 workgroup exceeds kernel limits"))?;
        log::info!("GigaHorse CUDA F1 generation: {generation_threads} threads");
        for threads in [128, 256, 384, 512] {
            let active_blocks = if threads <= maximum_threads {
                functions[3]
                    .occupancy_max_active_blocks_per_multiprocessor(threads, 0, None)
                    .map_err(Error::other)?
            } else {
                0
            };
            if threads == dense_threads && active_blocks == 0 {
                return Err(Error::other(
                    "GigaHorse CUDA dense workgroup exceeds kernel resource limits",
                ));
            }
            log::debug!(
                "GigaHorse CUDA dense matching: {threads} threads, {active_blocks} blocks per multiprocessor"
            );
        }
        log::info!(
            "GigaHorse CUDA: {}, compute_{major}{minor}, dense matching {dense_threads} threads",
            device.name().map_err(Error::other)?
        );
        let pool = MemoryPool::new(&device)?;
        let grouped_partition_scatter = super::select_feature(
            grouped_override.or((!partition_second).then_some(false)),
            supports_threads(19, partition_threads)?,
            "CUDA grouped F2 scatter",
        )?;
        log::info!(
            "GigaHorse CUDA direct F2: {partition_second}, grouped scatter: {grouped_partition_scatter}, local CSR threads: {local_csr_threads}"
        );
        let profile = if std::env::var_os("GH_CUDA_PROFILE").is_some() {
            Some(Profile::new(&device)?)
        } else {
            None
        };
        Ok(Self {
            device,
            launch_lock: Mutex::new(()),
            functions,
            dense_threads,
            generation_threads,
            partition_second,
            partition_threads,
            grouped_partition_scatter,
            local_csr_threads,
            pool,
            profile,
        })
    }
}

fn parse_local_csr_threads(value: Option<&OsStr>) -> Result<Option<u32>, Error> {
    match value {
        None => Ok(None),
        Some(value) if value == "0" => Ok(Some(0)),
        Some(value) if value == "256" => Ok(Some(256)),
        Some(value) if value == "512" => Ok(Some(512)),
        _ => Err(Error::new(
            ErrorKind::InvalidInput,
            "GigaHorse GH_CUDA_LOCAL_CSR_THREADS must be 0, 256 or 512",
        )),
    }
}

fn parse_grouped_partition_scatter(value: Option<&OsStr>) -> Result<Option<bool>, Error> {
    super::parse_bool_override(value, "GH_CUDA_GROUPED_F2")
}

fn parse_partition_second(value: Option<&OsStr>) -> Result<Option<bool>, Error> {
    super::parse_bool_override(value, "GH_CUDA_DIRECT_F2")
}

fn parse_dense_targets(value: Option<&OsStr>) -> Result<bool, Error> {
    match value {
        None => Ok(true),
        Some(value) if value == "1" => Ok(false),
        Some(value) if value == "8" => Ok(true),
        _ => Err(Error::new(
            ErrorKind::InvalidInput,
            "GigaHorse GH_CUDA_DENSE_TARGETS must be 1 or 8",
        )),
    }
}

#[cfg(test)]
mod configuration_tests {
    use super::*;

    #[test]
    fn dense_target_modulo_fold_matches_remainder() {
        for parity_term in 0_u32..=127 {
            let square = parity_term * parity_term;
            let mut folded = (square & 127) + (square >> 7);
            if folded >= 127 {
                folded -= 127;
            }
            assert_eq!(folded, square % 127);
        }
    }

    #[test]
    fn dense_collision_modulo_fold_matches_remainder() {
        for parity_term in 0_u32..128 {
            for column in 0_u32..127 {
                let square_column = parity_term * parity_term + column;
                assert!(square_column <= 16255);
                let mut target_column = (square_column & 127) + (square_column >> 7);
                if target_column >= 127 {
                    target_column -= 127;
                }
                assert_eq!(target_column, square_column % 127);
            }
        }
    }

    #[test]
    fn dense_targets_default_to_eight_and_overrides_are_strict() {
        assert!(parse_dense_targets(None).unwrap());
        assert!(!parse_dense_targets(Some(OsStr::new("1"))).unwrap());
        assert!(parse_dense_targets(Some(OsStr::new("8"))).unwrap());
        for value in ["", "0", "2", "08", " 8", "8 "] {
            assert_eq!(
                parse_dense_targets(Some(OsStr::new(value)))
                    .unwrap_err()
                    .kind(),
                ErrorKind::InvalidInput
            );
        }
    }

    #[test]
    fn local_csr_threads_override_is_strict() {
        assert_eq!(parse_local_csr_threads(None).unwrap(), None);
        for threads in [0_u32, 256, 512] {
            assert_eq!(
                parse_local_csr_threads(Some(OsStr::new(&threads.to_string()))).unwrap(),
                Some(threads)
            );
        }
        for value in ["", "128", "384", "1024", "0256", " 256", "256 "] {
            assert_eq!(
                parse_local_csr_threads(Some(OsStr::new(value)))
                    .unwrap_err()
                    .kind(),
                ErrorKind::InvalidInput
            );
        }
    }

    #[test]
    fn grouped_partition_scatter_override_is_strict() {
        assert_eq!(parse_grouped_partition_scatter(None).unwrap(), None);
        assert_eq!(
            parse_grouped_partition_scatter(Some(OsStr::new("0"))).unwrap(),
            Some(false)
        );
        assert_eq!(
            parse_grouped_partition_scatter(Some(OsStr::new("1"))).unwrap(),
            Some(true)
        );
        for value in ["", "2", "true", "false", "01", " 1", "1 "] {
            assert_eq!(
                parse_grouped_partition_scatter(Some(OsStr::new(value)))
                    .unwrap_err()
                    .kind(),
                ErrorKind::InvalidInput,
            );
        }
    }

    #[test]
    fn partition_second_override_is_strict() {
        assert_eq!(parse_partition_second(None).unwrap(), None);
        assert_eq!(
            parse_partition_second(Some(OsStr::new("0"))).unwrap(),
            Some(false)
        );
        assert_eq!(
            parse_partition_second(Some(OsStr::new("1"))).unwrap(),
            Some(true)
        );
        for value in ["", "2", "true", "false", "01", " 1", "1 "] {
            assert_eq!(
                parse_partition_second(Some(OsStr::new(value)))
                    .unwrap_err()
                    .kind(),
                ErrorKind::InvalidInput,
            );
        }
    }
}

impl super::Device for Device {
    type Memory = Memory;

    fn local_csr_threads(&self) -> usize {
        self.local_csr_threads as usize
    }

    fn grouped_partition_scatter(&self) -> bool {
        self.grouped_partition_scatter
    }

    fn partition_second_threads(&self) -> usize {
        if self.partition_second {
            self.partition_threads as usize
        } else {
            0
        }
    }

    #[cfg(test)]
    fn partition_second_kernel_threads(&self) -> usize {
        self.partition_threads as usize
    }

    fn generation_threads(&self) -> usize {
        self.generation_threads as usize
    }

    fn prefer_coarse_histogram(&self) -> bool {
        true
    }

    fn allocate(&self, words: usize) -> Result<Memory, Error> {
        let _timer = self.profile.as_ref().map(|profile| profile.timer(0));
        self.device.bind_to_thread().map_err(Error::other)?;
        if let Some(pool) = &self.pool {
            return pool.allocate(words.max(1));
        }
        unsafe { self.device.alloc(words.max(1)) }
            .map(|data| Memory { data, _pool: None })
            .map_err(Error::other)
    }

    fn upload(&self, buffer: &Memory, words: &[u32]) -> Result<(), Error> {
        let _timer = self.profile.as_ref().map(|profile| profile.timer(1));
        self.device.bind_to_thread().map_err(Error::other)?;
        if words.len() > buffer.data.len() {
            return Err(Error::other("GigaHorse CUDA upload out of bounds"));
        }
        unsafe { cudarc::driver::result::memcpy_htod_sync(*buffer.data.device_ptr(), words) }
            .map_err(Error::other)
    }

    fn download(&self, buffer: &Memory, words: usize) -> Result<Vec<u32>, Error> {
        let _timer = self.profile.as_ref().map(|profile| profile.timer(2));
        self.device.bind_to_thread().map_err(Error::other)?;
        if words > buffer.data.len() {
            return Err(Error::other("GigaHorse CUDA readback out of bounds"));
        }
        if words == 0 {
            return Ok(Vec::new());
        }
        self.device
            .dtoh_sync_copy(&buffer.data.slice(..words))
            .map_err(Error::other)
    }

    fn launch(&self, parameters: &Parameters, groups: usize) -> Result<(), Error> {
        self.launch_batch(&[(*parameters, groups)])
    }

    fn launch_batch(&self, launches: &[(Parameters, usize)]) -> Result<(), Error> {
        if launches.is_empty() {
            return Ok(());
        }
        let _timer = self.profile.as_ref().map(|profile| profile.timer(3));
        if launches.iter().any(|(parameters, groups)| {
            super::kernel_index(
                parameters.words[0],
                parameters.words[5],
                self.functions.len() - 1,
            )
            .is_none()
                || *groups > u32::MAX as usize
                || (parameters.words[0] == 20 && self.local_csr_threads == 0)
        }) {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "GigaHorse CUDA launch exceeds supported operation or grid limits",
            ));
        }
        self.device.bind_to_thread().map_err(Error::other)?;
        let _launch = self.launch_lock.lock();
        let batch_capacity = if self.profile.is_some() {
            PROFILE_BATCH_CAPACITY
        } else {
            launches.len()
        };
        for batch in launches.chunks(batch_capacity) {
            let launched = (|| {
                for (slot, (parameters, groups)) in batch.iter().enumerate() {
                    if *groups == 0 {
                        continue;
                    }
                    let operation = parameters.words[0] as usize;
                    let function = super::kernel_index(
                        parameters.words[0],
                        parameters.words[5],
                        self.functions.len() - 1,
                    )
                    .unwrap();
                    if let Some(profile) = &self.profile {
                        profile.record(slot, false)?;
                    }
                    unsafe {
                        self.functions[function]
                            .clone()
                            .launch(
                                LaunchConfig {
                                    grid_dim: (*groups as u32, 1, 1),
                                    block_dim: (
                                        if operation == 3 {
                                            self.dense_threads
                                        } else if operation == 20 {
                                            self.local_csr_threads
                                        } else if operation == 1 || operation == 18 {
                                            self.generation_threads
                                        } else if operation == 17 || operation == 19 {
                                            self.partition_threads
                                        } else {
                                            128
                                        },
                                        1,
                                        1,
                                    ),
                                    shared_mem_bytes: 0,
                                },
                                (*parameters,),
                            )
                            .map_err(Error::other)?;
                    }
                    if let Some(profile) = &self.profile {
                        profile.record(slot, true)?;
                    }
                }
                Ok::<_, Error>(())
            })();
            let synchronized = self.device.synchronize().map_err(Error::other);
            launched?;
            synchronized?;
            if let Some(profile) = &self.profile {
                profile.collect(batch)?;
            }
        }
        Ok(())
    }
}
