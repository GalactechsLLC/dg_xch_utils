use std::io::{Error, ErrorKind};

pub const NVIDIA_VENDOR: u32 = 0x10de;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuPreference {
    Auto,
    Cuda(usize),
    Vulkan(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuBackend {
    Cuda,
    Vulkan,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuDevice {
    pub ordinal: usize,
    pub vendor: u32,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuSelection {
    pub backend: GpuBackend,
    pub device: GpuDevice,
}

pub fn select_gpu(
    preference: GpuPreference,
    cuda: Option<&GpuDevice>,
    vulkan: &[GpuDevice],
) -> Result<GpuSelection, Error> {
    let cuda = cuda.filter(|device| device.vendor == NVIDIA_VENDOR);
    let selection = match preference {
        GpuPreference::Auto => cuda.map(|device| (GpuBackend::Cuda, device)).or_else(|| {
            vulkan
                .iter()
                .filter(|device| device.vendor != NVIDIA_VENDOR)
                .min_by_key(|device| device.ordinal)
                .or_else(|| vulkan.iter().min_by_key(|device| device.ordinal))
                .map(|device| (GpuBackend::Vulkan, device))
        }),
        GpuPreference::Cuda(ordinal) => cuda
            .filter(|device| device.ordinal == ordinal)
            .map(|device| (GpuBackend::Cuda, device)),
        GpuPreference::Vulkan(ordinal) => vulkan
            .iter()
            .find(|device| device.ordinal == ordinal)
            .map(|device| (GpuBackend::Vulkan, device)),
    };
    let (backend, device) = selection.ok_or_else(|| {
        Error::new(
            ErrorKind::NotFound,
            "requested GPU backend/device unavailable; choose CPU explicitly or configure a supported GPU",
        )
    })?;
    Ok(GpuSelection {
        backend,
        device: device.clone(),
    })
}

pub fn parse_cuda_probe(output: &str, expected_ordinal: usize) -> Result<GpuDevice, Error> {
    let invalid = || Error::new(ErrorKind::InvalidData, "invalid CUDA device probe response");
    if output.len() > 4096 {
        return Err(invalid());
    }
    let mut fields = output.trim_end_matches(['\r', '\n']).split('\t');
    if fields.next() != Some("DGX_CUDA_DEVICE_V1") {
        return Err(invalid());
    }
    let ordinal: usize = fields
        .next()
        .ok_or_else(invalid)?
        .parse()
        .map_err(|_| invalid())?;
    let name =
        String::from_utf8(hex::decode(fields.next().ok_or_else(invalid)?).map_err(|_| invalid())?)
            .map_err(|_| invalid())?;
    if ordinal != expected_ordinal
        || name.is_empty()
        || name.len() > 256
        || name.chars().any(char::is_control)
        || fields.next().is_some()
    {
        return Err(invalid());
    }
    Ok(GpuDevice {
        ordinal,
        vendor: NVIDIA_VENDOR,
        name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(ordinal: usize, vendor: u32) -> GpuDevice {
        GpuDevice {
            ordinal,
            vendor,
            name: format!("GPU {vendor:x}"),
        }
    }

    #[test]
    fn auto_prefers_available_cuda_without_cross_backend_ordinal_mapping() {
        let cuda = device(0, NVIDIA_VENDOR);
        let vulkan = [device(0, 0x1002), device(1, NVIDIA_VENDOR)];
        let selection = select_gpu(GpuPreference::Auto, Some(&cuda), &vulkan).unwrap();
        assert_eq!(selection.backend, GpuBackend::Cuda);
        assert_eq!(selection.device, cuda);
        let selection = select_gpu(GpuPreference::Vulkan(0), Some(&cuda), &vulkan).unwrap();
        assert_eq!(selection.backend, GpuBackend::Vulkan);
        assert_eq!(selection.device.vendor, 0x1002);
    }

    #[test]
    fn auto_without_cuda_prefers_other_vendors_and_then_nvidia_vulkan() {
        let vulkan = [device(0, NVIDIA_VENDOR), device(1, 0x1002)];
        assert_eq!(
            select_gpu(GpuPreference::Auto, None, &vulkan)
                .unwrap()
                .device
                .ordinal,
            1
        );
        assert_eq!(
            select_gpu(GpuPreference::Auto, None, &vulkan[..1])
                .unwrap()
                .device
                .ordinal,
            0
        );
    }

    #[test]
    fn explicit_unavailable_backend_does_not_fall_back() {
        let vulkan = [device(0, 0x1002)];
        assert!(select_gpu(GpuPreference::Cuda(0), None, &vulkan).is_err());
        assert!(select_gpu(GpuPreference::Vulkan(1), None, &vulkan).is_err());
        assert!(select_gpu(GpuPreference::Auto, None, &[]).is_err());
        assert!(
            select_gpu(
                GpuPreference::Cuda(1),
                Some(&device(0, NVIDIA_VENDOR)),
                &vulkan
            )
            .is_err()
        );
        assert!(select_gpu(GpuPreference::Cuda(0), Some(&vulkan[0]), &[]).is_err());
    }

    #[test]
    fn cuda_probe_requires_exact_protocol_device_and_safe_name() {
        let valid = format!("DGX_CUDA_DEVICE_V1\t2\t{}\n", hex::encode("NVIDIA test"));
        assert_eq!(parse_cuda_probe(&valid, 2).unwrap().name, "NVIDIA test");
        assert!(parse_cuda_probe(&valid, 0).is_err());
        assert!(parse_cuda_probe("DGX_CUDA_DEVICE_V1\t0\t1b", 0).is_err());
        assert!(parse_cuda_probe("DGX_CUDA_DEVICE_V1\t0\t\n", 0).is_err());
        assert!(parse_cuda_probe("DGX_CUDA_DEVICE_V1\t0\t4e\textra", 0).is_err());
        assert!(parse_cuda_probe(&"x".repeat(4097), 0).is_err());
    }
}
