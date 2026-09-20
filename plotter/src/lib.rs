#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::todo,
        clippy::unimplemented
    )
)]
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub mod backend;
mod format;
pub mod proving;

use blst::min_pk::{PublicKey, SecretKey};
use dg_xch_core::blockchain::proof_of_space::{calculate_plot_id_v2, generate_plot_public_key};
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48};
use dg_xch_keys::master_sk_to_local_sk;
use dg_xch_pos2::{
    params::ProofParams,
    plotting::{NativePlot, PlotLimits},
};
use std::fs::File;
use std::io::{Error, ErrorKind, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidData, message)
}

#[derive(Clone, Debug)]
pub enum PoolBinding {
    PublicKey([u8; 48]),
    Contract([u8; 32]),
}

pub struct PlotRequest {
    pub farmer_public_key: [u8; 48],
    pub pool: PoolBinding,
    pub k: u8,
    pub strength: u8,
    pub index: u16,
    pub meta_group: u8,
    pub testnet: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlotInfo {
    pub plot_id: [u8; 32],
    pub k: u8,
    pub strength: u8,
    pub index: u16,
    pub meta_group: u8,
    pub portable: bool,
    pub chunks: u64,
    pub file_bytes: u64,
}

fn identity(memo: &[u8], strength: u8, index: u16, meta_group: u8) -> Result<Bytes32, Error> {
    let pool_size = match memo.len() {
        112 => 32,
        128 => 48,
        _ => return Err(invalid("invalid plot memo length")),
    };
    let farmer = PublicKey::key_validate(&memo[pool_size..pool_size + 48])
        .map_err(|_| invalid("invalid farmer public key"))?;
    let master = SecretKey::from_bytes(&memo[pool_size + 48..])
        .map_err(|_| invalid("invalid plot-local master key"))?;
    let local = master_sk_to_local_sk(&master)?;
    let plot_key = generate_plot_public_key(&local.sk_to_pk(), &farmer, pool_size == 32)?;
    let (pool_key, contract) = if pool_size == 48 {
        PublicKey::key_validate(&memo[..48]).map_err(|_| invalid("invalid pool public key"))?;
        (
            Some(Bytes48::from(
                <[u8; 48]>::try_from(&memo[..48])
                    .map_err(|_| invalid("invalid pool key length"))?,
            )),
            None,
        )
    } else {
        (
            None,
            Some(Bytes32::from(
                <[u8; 32]>::try_from(&memo[..32])
                    .map_err(|_| invalid("invalid pool contract length"))?,
            )),
        )
    };
    Ok(calculate_plot_id_v2(
        strength,
        Bytes48::from(plot_key.to_bytes()),
        pool_key,
        contract,
        index,
        meta_group,
    ))
}

fn read_u64(file: &mut File) -> Result<u64, Error> {
    let mut bytes = [0; 8];
    file.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

pub fn inspect(path: &Path) -> Result<PlotInfo, Error> {
    let mut file = File::open(path)?;
    let file_bytes = file.metadata()?.len();
    let mut header = [0; 43];
    file.read_exact(&mut header)?;
    if &header[..4] != b"pos2" || header[4] != 1 {
        return Err(invalid(
            "expected PoS2 fat-plot format 1; legacy and Benes plots are unsupported",
        ));
    }
    let plot_id: [u8; 32] = header[5..37]
        .try_into()
        .map_err(|_| invalid("invalid plot ID length"))?;
    let k = header[37];
    let strength = header[38];
    ProofParams::new(plot_id.into(), k, strength, false)?;
    let index = u16::from_le_bytes([header[39], header[40]]);
    let meta_group = header[41];
    let memo_len = usize::from(header[42]);
    if ![112, 128].contains(&memo_len) {
        return Err(invalid("invalid plot memo length"));
    }
    let mut memo = zeroize::Zeroizing::new(vec![0; memo_len]);
    file.read_exact(&mut memo)?;
    if identity(&memo, strength, index, meta_group)?.as_ref() != plot_id {
        return Err(invalid("plot ID does not match memo and metadata"));
    }
    let chunks = read_u64(&mut file)?;
    if chunks == 0 || chunks > (1u64 << (k - 16)) {
        return Err(invalid("invalid chunk count"));
    }
    let mut expected_offset = file.stream_position()? + chunks * 8;
    let offsets = (0..chunks)
        .map(|_| read_u64(&mut file))
        .collect::<Result<Vec<_>, _>>()?;
    for offset in offsets {
        if offset != expected_offset || offset.checked_add(20).is_none_or(|end| end > file_bytes) {
            return Err(invalid("invalid chunk offset"));
        }
        file.seek(SeekFrom::Start(offset))?;
        let size = read_u64(&mut file)?;
        if !(12..=64 * 1024 * 1024).contains(&size) {
            return Err(invalid("invalid or unsupported chunk size"));
        }
        expected_offset = offset
            .checked_add(8 + size)
            .ok_or_else(|| invalid("chunk overflow"))?;
        if expected_offset > file_bytes {
            return Err(invalid("truncated chunk"));
        }
        let mut sizes = [0; 12];
        file.read_exact(&mut sizes)?;
        let values = u32::from_le_bytes([sizes[0], sizes[1], sizes[2], sizes[3]]);
        let compressed = u32::from_le_bytes([sizes[4], sizes[5], sizes[6], sizes[7]]);
        let stubs = u32::from_le_bytes([sizes[8], sizes[9], sizes[10], sizes[11]]);
        if values > 1_048_576
            || u64::from(stubs) != (u64::from(values) * u64::from(k - 2)).div_ceil(8)
            || size != 12 + u64::from(compressed) + u64::from(stubs)
        {
            return Err(invalid("invalid or unsupported chunk layout"));
        }
    }
    if expected_offset != file_bytes {
        return Err(invalid("unexpected trailing data"));
    }
    Ok(PlotInfo {
        plot_id,
        k,
        strength,
        index,
        meta_group,
        portable: memo_len == 112,
        chunks,
        file_bytes,
    })
}

pub fn create(
    request: &PlotRequest,
    destination: &Path,
    limits: PlotLimits,
    cancelled: &AtomicBool,
) -> Result<PlotInfo, Error> {
    create_with_engine(request, destination, limits, cancelled, NativePlot::build)
}

pub fn create_with_engine(
    request: &PlotRequest,
    destination: &Path,
    limits: PlotLimits,
    cancelled: &AtomicBool,
    engine: impl FnOnce(ProofParams, PlotLimits, &AtomicBool) -> Result<NativePlot, Error>,
) -> Result<PlotInfo, Error> {
    ProofParams::new([0; 32].into(), request.k, request.strength, request.testnet)?;
    if destination.exists() {
        return Err(Error::new(
            ErrorKind::AlreadyExists,
            "destination already exists",
        ));
    }
    let entropy = zeroize::Zeroizing::new(rand::random::<[u8; 32]>());
    let master = SecretKey::key_gen_v3(entropy.as_ref(), &[])
        .map_err(|_| invalid("plot key generation failed"))?;
    let mut memo = zeroize::Zeroizing::new(Vec::with_capacity(128));
    match request.pool {
        PoolBinding::PublicKey(key) => memo.extend_from_slice(&key),
        PoolBinding::Contract(hash) => memo.extend_from_slice(&hash),
    }
    memo.extend_from_slice(&request.farmer_public_key);
    memo.extend_from_slice(&master.to_bytes());
    let plot_id = identity(&memo, request.strength, request.index, request.meta_group)?;
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temporary = tempfile::Builder::new()
        .prefix(".pos2-")
        .suffix(".partial")
        .tempfile_in(parent)?;
    let plot = engine(
        ProofParams::new(plot_id, request.k, request.strength, request.testnet)?,
        limits,
        cancelled,
    )?;
    if plot.params() != &ProofParams::new(plot_id, request.k, request.strength, request.testnet)? {
        return Err(invalid("plot engine returned different parameters"));
    }
    format::write_plot(
        temporary.as_file_mut(),
        &plot,
        request.index,
        request.meta_group,
        &memo,
        cancelled,
    )?;
    let info = inspect(temporary.path())?;
    if Bytes32::from(info.plot_id) != plot_id
        || info.k != request.k
        || info.strength != request.strength
    {
        return Err(invalid("native writer produced unexpected plot metadata"));
    }
    temporary.as_file().sync_all()?;
    if cancelled.load(Ordering::Relaxed) {
        return Err(Error::new(
            ErrorKind::Interrupted,
            "native plot publication cancelled",
        ));
    }
    temporary
        .persist_noclobber(destination)
        .map_err(|error| error.error)?;
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    Ok(info)
}
