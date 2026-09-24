use super::Parameters;
use ash::{Entry, vk};
use std::ffi::{CStr, OsStr};
use std::io::{Cursor, Error, ErrorKind};
use std::sync::Arc;

struct Instance {
    _entry: Entry,
    raw: ash::Instance,
}

impl Drop for Instance {
    fn drop(&mut self) {
        unsafe {
            self.raw.destroy_instance(None);
        }
    }
}

struct Context {
    _instance: Instance,
    device: ash::Device,
    properties: vk::PhysicalDeviceMemoryProperties,
}

impl Drop for Context {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();
            self.device.destroy_device(None);
        }
    }
}

pub struct Memory {
    context: Arc<Context>,
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    address: u64,
    size: u64,
}

impl Drop for Memory {
    fn drop(&mut self) {
        unsafe {
            self.context.device.destroy_buffer(self.buffer, None);
            self.context.device.free_memory(self.memory, None);
        }
    }
}

impl super::Buffer for Memory {
    fn address(&self) -> u64 {
        self.address
    }
}

pub struct Device {
    context: Arc<Context>,
    staging: Memory,
    queue: vk::Queue,
    pool: vk::CommandPool,
    command: vk::CommandBuffer,
    fence: vk::Fence,
    layout: vk::PipelineLayout,
    pipelines: Vec<vk::Pipeline>,
    generation_threads: u32,
    bitmap_summary: bool,
    first_partitions: usize,
    coarse_histogram: bool,
    partition_second: bool,
    partition_threads: u32,
    grouped_partition_scatter: bool,
    local_csr_threads: u32,
}

impl Drop for Device {
    fn drop(&mut self) {
        unsafe {
            let device = &self.context.device;
            let _ = device.device_wait_idle();
            for pipeline in &self.pipelines {
                device.destroy_pipeline(*pipeline, None);
            }
            device.destroy_pipeline_layout(self.layout, None);
            device.destroy_fence(self.fence, None);
            device.destroy_command_pool(self.pool, None);
        }
    }
}

impl Device {
    pub fn new(ordinal: usize) -> Result<Self, Error> {
        let local_csr_value = std::env::var_os("GH_VULKAN_LOCAL_CSR_THREADS");
        let local_csr_override = parse_local_csr_threads(local_csr_value.as_deref())?;
        let matching_subgroup =
            parse_matching_subgroup(std::env::var_os("GH_VULKAN_MATCH_SUBGROUP").as_deref())?;
        let coarse_histogram =
            parse_coarse_histogram(std::env::var_os("GH_VULKAN_COARSE_F2").as_deref())?;
        let partition_second =
            parse_partition_second(std::env::var_os("GH_VULKAN_DIRECT_F2").as_deref())?;
        let grouped_override =
            parse_grouped_partition_scatter(std::env::var_os("GH_VULKAN_GROUPED_F2").as_deref())?;
        let requested_grouped_threads = parse_grouped_partition_threads(
            std::env::var_os("GH_VULKAN_GROUPED_F2_THREADS").as_deref(),
        )?;
        unsafe {
            let entry = Entry::load().map_err(Error::other)?;
            let application = vk::ApplicationInfo::default()
                .application_name(c"GigaHorse harvesting")
                .api_version(vk::API_VERSION_1_2);
            let instance = entry
                .create_instance(
                    &vk::InstanceCreateInfo::default().application_info(&application),
                    None,
                )
                .map_err(Error::other)?;
            let owner = Instance {
                _entry: entry,
                raw: instance,
            };
            let instance = &owner.raw;
            let devices = instance
                .enumerate_physical_devices()
                .map_err(Error::other)?;
            let physical = devices
                .into_iter()
                .filter(|physical| {
                    matches!(
                        instance
                            .get_physical_device_properties(*physical)
                            .device_type,
                        vk::PhysicalDeviceType::DISCRETE_GPU
                            | vk::PhysicalDeviceType::INTEGRATED_GPU
                    )
                })
                .nth(ordinal)
                .ok_or_else(|| {
                    Error::new(ErrorKind::NotFound, "GigaHorse Vulkan device not found")
                })?;
            let properties = instance.get_physical_device_properties(physical);
            let grouped_threads = requested_grouped_threads.unwrap_or_else(|| {
                [512, 256, 128]
                    .into_iter()
                    .find(|threads| {
                        *threads <= properties.limits.max_compute_work_group_invocations
                            && *threads <= properties.limits.max_compute_work_group_size[0]
                    })
                    .unwrap_or(128)
            });
            if requested_grouped_threads.is_some()
                && (grouped_threads > properties.limits.max_compute_work_group_invocations
                    || grouped_threads > properties.limits.max_compute_work_group_size[0])
            {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    "GigaHorse Vulkan grouped F2 workgroup exceeds device limits",
                ));
            }
            let local_csr_threads = if local_csr_override == Some(0) {
                0
            } else {
                super::select_threads(
                    local_csr_value.as_deref(),
                    "GH_VULKAN_LOCAL_CSR_THREADS",
                    &[256, 512],
                    &[512, 256],
                    |threads| {
                        Ok(properties.limits.max_compute_shared_memory_size >= 46_080
                            && threads <= properties.limits.max_compute_work_group_invocations
                            && threads <= properties.limits.max_compute_work_group_size[0])
                    },
                )?
                .unwrap_or(0)
            };
            let grouped_partition_scatter = super::select_feature(
                grouped_override.or((!partition_second).then_some(false)),
                properties.limits.max_compute_shared_memory_size >= 46_080
                    && grouped_threads <= properties.limits.max_compute_work_group_invocations
                    && grouped_threads <= properties.limits.max_compute_work_group_size[0],
                "Vulkan grouped F2 scatter",
            )?;
            let generation_threads = super::select_threads(
                std::env::var_os("GH_VULKAN_F1_THREADS").as_deref(),
                "GH_VULKAN_F1_THREADS",
                &[128, 256, 512],
                &[512, 256, 128],
                |threads| {
                    Ok(
                        threads <= properties.limits.max_compute_work_group_invocations
                            && threads <= properties.limits.max_compute_work_group_size[0],
                    )
                },
            )?
            .ok_or_else(|| Error::other("GigaHorse Vulkan F1 workgroup exceeds device limits"))?;
            let mut extensions = Vec::new();
            if matching_subgroup.is_some() {
                let available = instance
                    .enumerate_device_extension_properties(physical)
                    .map_err(Error::other)?;
                if !available.iter().any(|extension| {
                    CStr::from_ptr(extension.extension_name.as_ptr())
                        == ash::ext::subgroup_size_control::NAME
                }) {
                    return Err(Error::new(
                        ErrorKind::Unsupported,
                        "GigaHorse Vulkan requires VK_EXT_subgroup_size_control for GH_VULKAN_MATCH_SUBGROUP",
                    ));
                }
                extensions.push(ash::ext::subgroup_size_control::NAME.as_ptr());
            }
            let mut subgroups = vk::PhysicalDeviceSubgroupProperties::default();
            let mut subgroup_limits = vk::PhysicalDeviceSubgroupSizeControlProperties::default();
            let mut properties2 =
                vk::PhysicalDeviceProperties2::default().push_next(&mut subgroups);
            if matching_subgroup.is_some() {
                properties2 = properties2.push_next(&mut subgroup_limits);
            }
            instance.get_physical_device_properties2(physical, &mut properties2);
            if !subgroups
                .supported_stages
                .contains(vk::ShaderStageFlags::COMPUTE)
                || !subgroups.supported_operations.contains(
                    vk::SubgroupFeatureFlags::BASIC
                        | vk::SubgroupFeatureFlags::BALLOT
                        | vk::SubgroupFeatureFlags::ARITHMETIC,
                )
            {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    "GigaHorse Vulkan requires compute subgroup ballots and arithmetic",
                ));
            }
            let mut address_features = vk::PhysicalDeviceBufferDeviceAddressFeatures::default();
            let mut subgroup_features = vk::PhysicalDeviceSubgroupSizeControlFeatures::default();
            let mut features =
                vk::PhysicalDeviceFeatures2::default().push_next(&mut address_features);
            if matching_subgroup.is_some() {
                features = features.push_next(&mut subgroup_features);
            }
            instance.get_physical_device_features2(physical, &mut features);
            let supported = features.features;
            if let Some(size) = matching_subgroup {
                validate_matching_subgroup(size, &subgroup_features, &subgroup_limits)?;
            }
            if properties.api_version < vk::API_VERSION_1_2
                || address_features.buffer_device_address == 0
                || supported.shader_int64 == 0
                || properties.limits.max_compute_shared_memory_size < 16384
                || properties.limits.max_compute_work_group_invocations < 128
                || properties.limits.max_compute_work_group_size[0] < 128
            {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    "GigaHorse Vulkan requires buffer addresses, int64 and 16 KiB shared memory",
                ));
            }
            let families = instance.get_physical_device_queue_family_properties(physical);
            let family = families
                .iter()
                .position(|family| {
                    family.queue_flags.contains(vk::QueueFlags::COMPUTE)
                        && !family.queue_flags.contains(vk::QueueFlags::GRAPHICS)
                })
                .or_else(|| {
                    families
                        .iter()
                        .position(|family| family.queue_flags.contains(vk::QueueFlags::COMPUTE))
                })
                .ok_or_else(|| Error::other("GigaHorse Vulkan compute queue unavailable"))?
                as u32;
            let priorities = [1.0];
            let queues = [vk::DeviceQueueCreateInfo::default()
                .queue_family_index(family)
                .queue_priorities(&priorities)];
            let mut address_features = vk::PhysicalDeviceBufferDeviceAddressFeatures::default()
                .buffer_device_address(true);
            let mut subgroup_features = vk::PhysicalDeviceSubgroupSizeControlFeatures::default()
                .subgroup_size_control(true);
            let enabled = vk::PhysicalDeviceFeatures::default().shader_int64(true);
            let mut device_info = vk::DeviceCreateInfo::default()
                .queue_create_infos(&queues)
                .enabled_features(&enabled)
                .enabled_extension_names(&extensions)
                .push_next(&mut address_features);
            if matching_subgroup.is_some() {
                device_info = device_info.push_next(&mut subgroup_features);
            }
            let device = instance
                .create_device(physical, &device_info, None)
                .map_err(Error::other)?;
            let queue = device.get_device_queue(family, 0);
            let memory_properties = instance.get_physical_device_memory_properties(physical);
            let context = Arc::new(Context {
                _instance: owner,
                device,
                properties: memory_properties,
            });
            let staging = allocate(
                &context,
                65536,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            )?;
            let mut output = Self {
                context: context.clone(),
                staging,
                queue,
                pool: vk::CommandPool::null(),
                command: vk::CommandBuffer::null(),
                fence: vk::Fence::null(),
                layout: vk::PipelineLayout::null(),
                pipelines: Vec::new(),
                generation_threads,
                bitmap_summary: first_bitmap_summary_default(properties.vendor_id),
                first_partitions: first_partitions_default(properties.vendor_id),
                coarse_histogram,
                partition_second,
                partition_threads: 128,
                grouped_partition_scatter,
                local_csr_threads,
            };
            let device = &context.device;
            output.pool = device
                .create_command_pool(
                    &vk::CommandPoolCreateInfo::default()
                        .queue_family_index(family)
                        .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                    None,
                )
                .map_err(Error::other)?;
            output.command = device
                .allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(output.pool)
                        .level(vk::CommandBufferLevel::PRIMARY)
                        .command_buffer_count(1),
                )
                .map_err(Error::other)?[0];
            output.fence = device
                .create_fence(&vk::FenceCreateInfo::default(), None)
                .map_err(Error::other)?;
            let ranges = [vk::PushConstantRange::default()
                .stage_flags(vk::ShaderStageFlags::COMPUTE)
                .size(128)];
            output.layout = device
                .create_pipeline_layout(
                    &vk::PipelineLayoutCreateInfo::default().push_constant_ranges(&ranges),
                    None,
                )
                .map_err(Error::other)?;
            let code = ash::util::read_spv(&mut Cursor::new(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/gigahorse.spv"
            ))))
            .map_err(Error::other)?;
            let shader = device
                .create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&code), None)
                .map_err(Error::other)?;
            let dense_threads = super::select_threads(
                std::env::var_os("GH_VULKAN_DENSE_THREADS").as_deref(),
                "GH_VULKAN_DENSE_THREADS",
                &[128, 256, 384, 512],
                &[512, 256, 128],
                |threads| {
                    Ok(
                        threads <= properties.limits.max_compute_work_group_invocations
                            && threads <= properties.limits.max_compute_work_group_size[0],
                    )
                },
            )?
            .ok_or_else(|| {
                Error::other("GigaHorse Vulkan dense workgroup exceeds device limits")
            })?;
            output.partition_threads = [512, 256, 128]
                .into_iter()
                .find(|threads| {
                    *threads <= properties.limits.max_compute_work_group_invocations
                        && *threads <= properties.limits.max_compute_work_group_size[0]
                })
                .ok_or_else(|| {
                    Error::other("GigaHorse Vulkan partition workgroup exceeds device limits")
                })?;
            let operation_count = if local_csr_threads != 0 {
                21
            } else if grouped_partition_scatter {
                20
            } else {
                19
            };
            let operations: Vec<[u32; 2]> = (0..operation_count)
                .map(|operation| {
                    [
                        operation,
                        if operation == 3 {
                            dense_threads
                        } else if operation == 20 {
                            local_csr_threads
                        } else if operation == 17 {
                            output.partition_threads
                        } else if operation == 19 {
                            grouped_threads
                        } else if operation == 1 || operation == 18 {
                            generation_threads
                        } else {
                            128
                        },
                    ]
                })
                .chain(std::iter::once([21, generation_threads]))
                .collect();
            let maps = [
                vk::SpecializationMapEntry::default()
                    .constant_id(0)
                    .offset(0)
                    .size(4),
                vk::SpecializationMapEntry::default()
                    .constant_id(1)
                    .offset(4)
                    .size(4),
            ];
            let specializations: Vec<_> = operations
                .iter()
                .map(|operation| {
                    vk::SpecializationInfo::default()
                        .map_entries(&maps)
                        .data(bytemuck::bytes_of(operation))
                })
                .collect();
            let mut subgroup_sizes = vec![
                vk::PipelineShaderStageRequiredSubgroupSizeCreateInfo::default()
                    .required_subgroup_size(matching_subgroup.unwrap_or(32));
                specializations.len()
            ];
            let creates: Vec<_> = specializations
                .iter()
                .zip(&mut subgroup_sizes)
                .enumerate()
                .map(|(operation, (specialization, subgroup_size))| {
                    let mut stage = vk::PipelineShaderStageCreateInfo::default()
                        .stage(vk::ShaderStageFlags::COMPUTE)
                        .module(shader)
                        .name(c"main")
                        .specialization_info(specialization);
                    if matches!(operation, 7 | 15) && matching_subgroup.is_some() {
                        stage = stage.push_next(subgroup_size);
                    }
                    vk::ComputePipelineCreateInfo::default()
                        .stage(stage)
                        .layout(output.layout)
                })
                .collect();
            let result = device.create_compute_pipelines(vk::PipelineCache::null(), &creates, None);
            device.destroy_shader_module(shader, None);
            output.pipelines = result.map_err(|(pipelines, error)| {
                for pipeline in pipelines {
                    device.destroy_pipeline(pipeline, None);
                }
                Error::other(error)
            })?;
            log::info!(
                "GigaHorse Vulkan: {}; device default subgroup {}, matching subgroup {}",
                CStr::from_ptr(properties.device_name.as_ptr()).to_string_lossy(),
                subgroups.subgroup_size,
                matching_subgroup.map_or_else(|| "default".to_owned(), |size| size.to_string())
            );
            log::info!("GigaHorse Vulkan F1 generation: {generation_threads} threads");
            log::info!(
                "GigaHorse Vulkan direct F2: {partition_second}, grouped scatter: {grouped_partition_scatter}, local CSR threads: {local_csr_threads}"
            );
            log::debug!("GigaHorse Vulkan coarse F2 histogram: {coarse_histogram}");
            Ok(output)
        }
    }

    fn submit(&self, record: impl FnOnce(vk::CommandBuffer)) -> Result<(), Error> {
        unsafe {
            let device = &self.context.device;
            device
                .reset_command_buffer(self.command, vk::CommandBufferResetFlags::empty())
                .map_err(Error::other)?;
            device
                .begin_command_buffer(
                    self.command,
                    &vk::CommandBufferBeginInfo::default()
                        .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                )
                .map_err(Error::other)?;
            let barrier = [vk::MemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
                .dst_access_mask(vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE)];
            device.cmd_pipeline_barrier(
                self.command,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                &barrier,
                &[],
                &[],
            );
            record(self.command);
            device
                .end_command_buffer(self.command)
                .map_err(Error::other)?;
            device.reset_fences(&[self.fence]).map_err(Error::other)?;
            let commands = [self.command];
            device
                .queue_submit(
                    self.queue,
                    &[vk::SubmitInfo::default().command_buffers(&commands)],
                    self.fence,
                )
                .map_err(Error::other)?;
            device
                .wait_for_fences(&[self.fence], true, u64::MAX)
                .map_err(Error::other)
        }
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
            "GigaHorse GH_VULKAN_LOCAL_CSR_THREADS must be 0, 256 or 512",
        )),
    }
}

fn parse_grouped_partition_scatter(value: Option<&OsStr>) -> Result<Option<bool>, Error> {
    super::parse_bool_override(value, "GH_VULKAN_GROUPED_F2")
}

fn parse_grouped_partition_threads(value: Option<&OsStr>) -> Result<Option<u32>, Error> {
    match value {
        None => Ok(None),
        Some(value) if value == "128" => Ok(Some(128)),
        Some(value) if value == "256" => Ok(Some(256)),
        Some(value) if value == "512" => Ok(Some(512)),
        _ => Err(Error::new(
            ErrorKind::InvalidInput,
            "GigaHorse GH_VULKAN_GROUPED_F2_THREADS must be 128, 256 or 512",
        )),
    }
}

fn parse_partition_second(value: Option<&OsStr>) -> Result<bool, Error> {
    match value {
        None => Ok(true),
        Some(value) if value == "0" => Ok(false),
        Some(value) if value == "1" => Ok(true),
        Some(_) => Err(Error::new(
            ErrorKind::InvalidInput,
            "GigaHorse GH_VULKAN_DIRECT_F2 must be 0 or 1",
        )),
    }
}

fn parse_coarse_histogram(value: Option<&OsStr>) -> Result<bool, Error> {
    match value {
        None => Ok(false),
        Some(value) if value == "0" => Ok(false),
        Some(value) if value == "1" => Ok(true),
        Some(_) => Err(Error::new(
            ErrorKind::InvalidInput,
            "GigaHorse GH_VULKAN_COARSE_F2 must be 0 or 1",
        )),
    }
}

fn first_bitmap_summary_default(vendor_id: u32) -> bool {
    vendor_id == 0x1002
}

fn first_partitions_default(vendor_id: u32) -> usize {
    if vendor_id == 0x1002 { 64 } else { 32 }
}

fn parse_matching_subgroup(value: Option<&OsStr>) -> Result<Option<u32>, Error> {
    match value {
        None => Ok(None),
        Some(value) if value == "32" => Ok(Some(32)),
        Some(value) if value == "64" => Ok(Some(64)),
        Some(_) => Err(Error::new(
            ErrorKind::InvalidInput,
            "GigaHorse GH_VULKAN_MATCH_SUBGROUP must be 32 or 64",
        )),
    }
}

fn validate_matching_subgroup(
    size: u32,
    features: &vk::PhysicalDeviceSubgroupSizeControlFeatures<'_>,
    properties: &vk::PhysicalDeviceSubgroupSizeControlProperties<'_>,
) -> Result<(), Error> {
    if features.subgroup_size_control == 0
        || !(properties.min_subgroup_size..=properties.max_subgroup_size).contains(&size)
        || !properties
            .required_subgroup_size_stages
            .contains(vk::ShaderStageFlags::COMPUTE)
        || u64::from(size) * u64::from(properties.max_compute_workgroup_subgroups) < 128
    {
        return Err(Error::new(
            ErrorKind::Unsupported,
            format!("GigaHorse Vulkan matching subgroup size {size} is unsupported by this device"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod subgroup_tests {
    use super::*;

    #[test]
    fn bitmap_summary_defaults_only_to_amd() {
        assert!(first_bitmap_summary_default(0x1002));
        for vendor_id in [0, 0x10de, 0x8086] {
            assert!(!first_bitmap_summary_default(vendor_id));
        }
    }

    #[test]
    fn wide_first_partitions_default_only_to_amd() {
        assert_eq!(first_partitions_default(0x1002), 64);
        for vendor_id in [0, 0x10de, 0x8086] {
            assert_eq!(first_partitions_default(vendor_id), 32);
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
                ErrorKind::InvalidInput
            );
        }
    }

    #[test]
    fn grouped_partition_threads_override_is_strict() {
        assert_eq!(parse_grouped_partition_threads(None).unwrap(), None);
        for threads in [128_u32, 256, 512] {
            assert_eq!(
                parse_grouped_partition_threads(Some(OsStr::new(&threads.to_string()))).unwrap(),
                Some(threads)
            );
        }
        for value in ["", "0", "384", "1024", "0128", " 128", "128 "] {
            assert_eq!(
                parse_grouped_partition_threads(Some(OsStr::new(value)))
                    .unwrap_err()
                    .kind(),
                ErrorKind::InvalidInput
            );
        }
    }

    #[test]
    fn partition_second_override_is_strict() {
        assert!(parse_partition_second(None).unwrap());
        assert!(!parse_partition_second(Some(OsStr::new("0"))).unwrap());
        assert!(parse_partition_second(Some(OsStr::new("1"))).unwrap());
        for value in ["", "2", "true", "false", "01", " 1", "1 "] {
            assert_eq!(
                parse_partition_second(Some(OsStr::new(value)))
                    .unwrap_err()
                    .kind(),
                ErrorKind::InvalidInput,
            );
        }
    }

    #[test]
    fn coarse_histogram_override_accepts_only_explicit_booleans() {
        assert!(!parse_coarse_histogram(None).unwrap());
        assert!(!parse_coarse_histogram(Some(OsStr::new("0"))).unwrap());
        assert!(parse_coarse_histogram(Some(OsStr::new("1"))).unwrap());
        for value in ["", "2", "true", "false", "01", " 1", "1 ", "default"] {
            assert_eq!(
                parse_coarse_histogram(Some(OsStr::new(value)))
                    .unwrap_err()
                    .kind(),
                ErrorKind::InvalidInput,
            );
        }
    }

    #[test]
    fn matching_subgroup_override_accepts_only_explicit_sizes() {
        assert_eq!(parse_matching_subgroup(None).unwrap(), None);
        for size in ["32", "64"] {
            assert_eq!(
                parse_matching_subgroup(Some(OsStr::new(size))).unwrap(),
                Some(size.parse().unwrap())
            );
        }
        for value in ["", "0", "16", "128", " 32", "032", "default"] {
            assert_eq!(
                parse_matching_subgroup(Some(OsStr::new(value)))
                    .unwrap_err()
                    .kind(),
                ErrorKind::InvalidInput,
            );
        }
    }

    #[test]
    fn matching_subgroup_override_requires_device_support() {
        let features =
            vk::PhysicalDeviceSubgroupSizeControlFeatures::default().subgroup_size_control(true);
        let properties = vk::PhysicalDeviceSubgroupSizeControlProperties::default()
            .min_subgroup_size(32)
            .max_subgroup_size(64)
            .max_compute_workgroup_subgroups(4)
            .required_subgroup_size_stages(vk::ShaderStageFlags::COMPUTE);
        for size in [32, 64] {
            validate_matching_subgroup(size, &features, &properties).unwrap();
        }
        assert!(
            validate_matching_subgroup(32, &features.subgroup_size_control(false), &properties,)
                .is_err()
        );
        for unsupported in [
            properties.min_subgroup_size(64),
            properties.max_subgroup_size(16),
            properties.max_compute_workgroup_subgroups(3),
            properties.required_subgroup_size_stages(vk::ShaderStageFlags::FRAGMENT),
        ] {
            assert_eq!(
                validate_matching_subgroup(32, &features, &unsupported)
                    .unwrap_err()
                    .kind(),
                ErrorKind::Unsupported,
            );
        }
    }
}

fn allocate(
    context: &Arc<Context>,
    size: u64,
    flags: vk::MemoryPropertyFlags,
) -> Result<Memory, Error> {
    unsafe {
        let device = &context.device;
        let buffer = device
            .create_buffer(
                &vk::BufferCreateInfo::default()
                    .size(size.max(4))
                    .usage(
                        vk::BufferUsageFlags::STORAGE_BUFFER
                            | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS
                            | vk::BufferUsageFlags::TRANSFER_SRC
                            | vk::BufferUsageFlags::TRANSFER_DST,
                    )
                    .sharing_mode(vk::SharingMode::EXCLUSIVE),
                None,
            )
            .map_err(Error::other)?;
        let requirements = device.get_buffer_memory_requirements(buffer);
        let memory_type = (0..context.properties.memory_type_count).find(|index| {
            requirements.memory_type_bits & (1 << index) != 0
                && context.properties.memory_types[*index as usize]
                    .property_flags
                    .contains(flags)
        });
        let Some(memory_type) = memory_type else {
            device.destroy_buffer(buffer, None);
            return Err(Error::other("GigaHorse Vulkan memory type unavailable"));
        };
        let mut address_flags =
            vk::MemoryAllocateFlagsInfo::default().flags(vk::MemoryAllocateFlags::DEVICE_ADDRESS);
        let allocation = device.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(requirements.size)
                .memory_type_index(memory_type)
                .push_next(&mut address_flags),
            None,
        );
        let memory = match allocation {
            Ok(memory) => memory,
            Err(error) => {
                device.destroy_buffer(buffer, None);
                return Err(Error::other(error));
            }
        };
        if let Err(error) = device.bind_buffer_memory(buffer, memory, 0) {
            device.destroy_buffer(buffer, None);
            device.free_memory(memory, None);
            return Err(Error::other(error));
        }
        let address = device
            .get_buffer_device_address(&vk::BufferDeviceAddressInfo::default().buffer(buffer));
        Ok(Memory {
            context: context.clone(),
            buffer,
            memory,
            address,
            size,
        })
    }
}

impl super::Device for Device {
    type Memory = Memory;

    fn prefer_first_bitmap_summary(&self) -> bool {
        self.bitmap_summary
    }

    fn default_first_partitions(&self) -> usize {
        self.first_partitions
    }

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

    fn prefer_coarse_histogram(&self) -> bool {
        self.coarse_histogram
    }

    fn generation_threads(&self) -> usize {
        self.generation_threads as usize
    }

    fn allocate(&self, words: usize) -> Result<Memory, Error> {
        allocate(
            &self.context,
            words.max(1) as u64 * 4,
            vk::MemoryPropertyFlags::DEVICE_LOCAL,
        )
    }

    fn upload(&self, buffer: &Memory, words: &[u32]) -> Result<(), Error> {
        let bytes = bytemuck::cast_slice::<u32, u8>(words);
        if bytes.len() as u64 > buffer.size {
            return Err(Error::other("GigaHorse Vulkan upload out of bounds"));
        }
        if bytes.is_empty() {
            return Ok(());
        }
        let temporary = if bytes.len() as u64 > self.staging.size {
            Some(allocate(
                &self.context,
                bytes.len() as u64,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            )?)
        } else {
            None
        };
        let staging = temporary.as_ref().unwrap_or(&self.staging);
        unsafe {
            let mapped = self
                .context
                .device
                .map_memory(
                    staging.memory,
                    0,
                    bytes.len() as u64,
                    vk::MemoryMapFlags::empty(),
                )
                .map_err(Error::other)?;
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), mapped.cast(), bytes.len());
            self.context.device.unmap_memory(staging.memory);
            self.submit(|command| {
                self.context.device.cmd_copy_buffer(
                    command,
                    staging.buffer,
                    buffer.buffer,
                    &[vk::BufferCopy::default().size(bytes.len() as u64)],
                )
            })
        }
    }

    fn download(&self, buffer: &Memory, words: usize) -> Result<Vec<u32>, Error> {
        let size = words as u64 * 4;
        if size > buffer.size {
            return Err(Error::other("GigaHorse Vulkan readback out of bounds"));
        }
        if words == 0 {
            return Ok(Vec::new());
        }
        let temporary = if size > self.staging.size {
            Some(allocate(
                &self.context,
                size,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            )?)
        } else {
            None
        };
        let staging = temporary.as_ref().unwrap_or(&self.staging);
        unsafe {
            self.submit(|command| {
                self.context.device.cmd_copy_buffer(
                    command,
                    buffer.buffer,
                    staging.buffer,
                    &[vk::BufferCopy::default().size(size)],
                )
            })?;
            let mapped = self
                .context
                .device
                .map_memory(staging.memory, 0, size, vk::MemoryMapFlags::empty())
                .map_err(Error::other)?;
            let result = std::slice::from_raw_parts(mapped.cast::<u32>(), words).to_vec();
            self.context.device.unmap_memory(staging.memory);
            Ok(result)
        }
    }

    fn launch(&self, parameters: &Parameters, groups: usize) -> Result<(), Error> {
        self.launch_batch(&[(*parameters, groups)])
    }

    fn launch_batch(&self, launches: &[(Parameters, usize)]) -> Result<(), Error> {
        if launches.is_empty() {
            return Ok(());
        }
        if launches.iter().any(|(parameters, _)| {
            super::kernel_index(
                parameters.words[0],
                parameters.words[5],
                self.pipelines.len() - 1,
            )
            .is_none()
        }) {
            return Err(Error::other("Invalid GigaHorse Vulkan operation"));
        }
        unsafe {
            let device = &self.context.device;
            self.submit(|command| {
                for (index, (parameters, groups)) in launches.iter().enumerate() {
                    if index != 0 {
                        device.cmd_pipeline_barrier(
                            command,
                            vk::PipelineStageFlags::COMPUTE_SHADER,
                            vk::PipelineStageFlags::COMPUTE_SHADER,
                            vk::DependencyFlags::empty(),
                            &[vk::MemoryBarrier::default()
                                .src_access_mask(vk::AccessFlags::SHADER_WRITE)
                                .dst_access_mask(
                                    vk::AccessFlags::SHADER_READ | vk::AccessFlags::SHADER_WRITE,
                                )],
                            &[],
                            &[],
                        );
                    }
                    let pipeline_index = super::kernel_index(
                        parameters.words[0],
                        parameters.words[5],
                        self.pipelines.len() - 1,
                    )
                    .unwrap();
                    let pipeline = self.pipelines[pipeline_index];
                    let mut push = [0u32; 32];
                    push[..24].copy_from_slice(bytemuck::cast_slice(&parameters.pointers));
                    push[24..].copy_from_slice(&parameters.words[..8]);
                    if parameters.words[0] == 1 || parameters.words[0] == 18 {
                        for (packed, source) in [1, 3, 5, 6].into_iter().enumerate() {
                            push[packed * 2] = parameters.pointers[source] as u32;
                            push[packed * 2 + 1] = (parameters.pointers[source] >> 32) as u32;
                        }
                        push[8] = parameters.words[1];
                        push[9] = parameters.words[3];
                        push[10] = parameters.words[4];
                        push[11..31].copy_from_slice(&parameters.words[8..28]);
                        push[31] = parameters.words[5];
                    }
                    device.cmd_bind_pipeline(command, vk::PipelineBindPoint::COMPUTE, pipeline);
                    device.cmd_push_constants(
                        command,
                        self.layout,
                        vk::ShaderStageFlags::COMPUTE,
                        0,
                        bytemuck::cast_slice(&push),
                    );
                    device.cmd_dispatch(
                        command,
                        (*groups).min(65535) as u32,
                        groups.div_ceil(65535) as u32,
                        1,
                    );
                }
            })
        }
    }
}
