use crate::{
    AesHash, compact::CompactPlot, device::Config, params::ProofParams, plotting::PlotLimits,
};
use rayon::prelude::*;
use std::io::{Error, ErrorKind};
use std::sync::atomic::{AtomicBool, Ordering};

pub const BATCH_SIZE: usize = 65_536;
pub const GPU_BATCH_SIZE: usize = 262_144;
pub const SCRATCH_BYTES: u64 = 32 * 1024 * 1024;

pub trait HashEngine: Send {
    fn is_accelerated(&self) -> bool {
        false
    }

    fn build_compact(
        &mut self,
        _params: &ProofParams,
        _limits: PlotLimits,
        _cancelled: &AtomicBool,
    ) -> Result<Option<CompactPlot>, Error> {
        Ok(None)
    }

    fn hash(
        &mut self,
        inputs: &[[u32; 4]],
        rounds: u32,
        cancelled: &AtomicBool,
    ) -> Result<Vec<[u32; 4]>, Error>;
}

impl<Engine: HashEngine + ?Sized> HashEngine for Box<Engine> {
    fn is_accelerated(&self) -> bool {
        (**self).is_accelerated()
    }

    fn build_compact(
        &mut self,
        params: &ProofParams,
        limits: PlotLimits,
        cancelled: &AtomicBool,
    ) -> Result<Option<CompactPlot>, Error> {
        (**self).build_compact(params, limits, cancelled)
    }

    fn hash(
        &mut self,
        inputs: &[[u32; 4]],
        rounds: u32,
        cancelled: &AtomicBool,
    ) -> Result<Vec<[u32; 4]>, Error> {
        (**self).hash(inputs, rounds, cancelled)
    }
}

pub struct CpuHasher(AesHash);

impl CpuHasher {
    pub fn new(params: &ProofParams) -> Self {
        Self(AesHash::new(&params.plot_id(), params.k()))
    }

    pub fn hash_into_serial(
        &self,
        inputs: &[[u32; 4]],
        rounds: u32,
        output: &mut [[u32; 4]],
        cancelled: &AtomicBool,
    ) -> Result<(), Error> {
        check_cancelled(cancelled)?;
        if output.len() != inputs.len() || rounds == 0 || rounds > 1024 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "invalid bounded AES batch",
            ));
        }
        for (inputs, output) in inputs.chunks(4096).zip(output.chunks_mut(4096)) {
            check_cancelled(cancelled)?;
            self.0.hash_words_batch(inputs, rounds, output);
        }
        check_cancelled(cancelled)
    }
}

impl HashEngine for CpuHasher {
    fn hash(
        &mut self,
        inputs: &[[u32; 4]],
        rounds: u32,
        cancelled: &AtomicBool,
    ) -> Result<Vec<[u32; 4]>, Error> {
        check_cancelled(cancelled)?;
        if inputs.len() > BATCH_SIZE || rounds == 0 || rounds > 1024 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "invalid bounded AES batch",
            ));
        }
        let mut output = allocate(inputs.len())?;
        output.resize(inputs.len(), [0; 4]);
        if inputs.len() < 8192 {
            self.hash_into_serial(inputs, rounds, &mut output, cancelled)?;
        } else {
            inputs
                .par_chunks(4096)
                .zip(output.par_chunks_mut(4096))
                .try_for_each(|(inputs, output)| {
                    self.hash_into_serial(inputs, rounds, output, cancelled)
                })?;
        }
        check_cancelled(cancelled)?;
        Ok(output)
    }
}

pub fn config(params: &ProofParams) -> Config {
    Config {
        plot_id: *AsRef::<[u8; 32]>::as_ref(&params.plot_id()),
        k: u32::from(params.k()),
        strength: u32::from(params.strength()),
        testnet: u32::from(params.is_testnet()),
    }
}

pub fn allocate<T>(capacity: usize) -> Result<Vec<T>, Error> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(capacity)
        .map_err(|_| Error::other("PoS2 allocation failed"))?;
    Ok(values)
}

pub fn check_cancelled(cancelled: &AtomicBool) -> Result<(), Error> {
    if cancelled.load(Ordering::Relaxed) {
        Err(Error::new(ErrorKind::Interrupted, "PoS2 work cancelled"))
    } else {
        Ok(())
    }
}

pub struct Work<'cancel> {
    remaining: u64,
    pub cancelled: &'cancel AtomicBool,
}

impl<'cancel> Work<'cancel> {
    pub fn new(limits: PlotLimits, cancelled: &'cancel AtomicBool) -> Self {
        Self {
            remaining: limits.max_work,
            cancelled,
        }
    }

    #[cfg(feature = "resident")]
    pub(crate) fn remaining(&self) -> u64 {
        self.remaining
    }

    pub fn charge(&mut self, amount: u64) -> Result<(), Error> {
        check_cancelled(self.cancelled)?;
        self.remaining = self
            .remaining
            .checked_sub(amount)
            .ok_or_else(|| Error::other("PoS2 work budget exceeded"))?;
        Ok(())
    }

    pub fn hash(
        &mut self,
        engine: &mut impl HashEngine,
        inputs: &[[u32; 4]],
        rounds: u32,
    ) -> Result<Vec<[u32; 4]>, Error> {
        if inputs.len() > BATCH_SIZE || rounds < 16 || !rounds.is_multiple_of(16) {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "invalid PoS2 hash request",
            ));
        }
        self.charge(
            (inputs.len() as u64)
                .checked_mul(u64::from(rounds / 16))
                .ok_or_else(|| Error::other("PoS2 work overflow"))?,
        )?;
        if inputs.is_empty() {
            return Ok(Vec::new());
        }
        let mut output = Vec::new();
        let mut remaining = rounds;
        while remaining > 0 {
            check_cancelled(self.cancelled)?;
            let count = remaining.min(1024);
            let source = if remaining == rounds { inputs } else { &output };
            output = engine.hash(source, count, self.cancelled)?;
            if output.len() != inputs.len() {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "hash engine returned wrong batch length",
                ));
            }
            remaining -= count;
        }
        Ok(output)
    }
}
