//! Owner-only development key custody shared by stack initialization and farming.
use super::{
    SignatureSigner,
    falcon::{FalconSigner, SECRET_KEY_BYTES},
    falcon_block::public_key_id,
};
use crate::blockchain::sized_bytes::Bytes48;
use std::{
    io::{Error, Read},
    path::Path,
    sync::Arc,
};
use zeroize::Zeroizing;

const MAX_PLOT_KEYS: usize = 32;
const MAX_KEY_FILE_BYTES: usize = 9 + (MAX_PLOT_KEYS * 2 + 1) * (897 + SECRET_KEY_BYTES);

#[derive(Clone)]
pub struct FalconPlotKeyPair {
    pub plot: Arc<FalconSigner>,
    pub pool: Arc<FalconSigner>,
}
#[derive(Clone)]
pub struct FalconKeyring {
    pub plots: Vec<FalconPlotKeyPair>,
    pub wallet: Arc<FalconSigner>,
}
impl PartialEq for FalconKeyring {
    fn eq(&self, other: &Self) -> bool {
        self.wallet.public_key() == other.wallet.public_key()
            && self.plots.len() == other.plots.len()
            && self.plots.iter().zip(&other.plots).all(|(a, b)| {
                a.plot.public_key() == b.plot.public_key()
                    && a.pool.public_key() == b.pool.public_key()
            })
    }
}
impl Eq for FalconKeyring {}
impl std::fmt::Debug for FalconKeyring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FalconKeyring")
            .field("plot_count", &self.plots.len())
            .finish_non_exhaustive()
    }
}
impl FalconKeyring {
    pub fn generate(count: u8) -> Result<Self, Error> {
        if !(1..=MAX_PLOT_KEYS).contains(&usize::from(count)) {
            return Err(Error::other("Falcon development plot count must be 1..32"));
        }
        let mut plots = Vec::new();
        for _ in 0..count {
            plots.push(FalconPlotKeyPair {
                plot: Arc::new(FalconSigner::generate().map_err(Error::other)?),
                pool: Arc::new(FalconSigner::generate().map_err(Error::other)?),
            });
        }
        Ok(Self {
            plots,
            wallet: Arc::new(FalconSigner::generate().map_err(Error::other)?),
        })
    }
    pub fn encode(&self) -> Zeroizing<Vec<u8>> {
        let mut bytes = Zeroizing::new(b"DGXKEY02".to_vec());
        bytes.push(self.plots.len() as u8);
        for signer in self
            .plots
            .iter()
            .flat_map(|p| [&p.plot, &p.pool])
            .chain(std::iter::once(&self.wallet))
        {
            bytes.extend_from_slice(&signer.public_key());
            bytes.extend_from_slice(signer.secret_bytes());
        }
        bytes
    }
    pub fn load(path: &Path) -> Result<Self, Error> {
        #[cfg(unix)]
        let file = {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(path)?
        };
        #[cfg(not(unix))]
        let file = std::fs::File::open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(Error::other("Falcon key file must be regular"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            // SAFETY: geteuid has no preconditions or side effects.
            let uid = unsafe { libc::geteuid() };
            if metadata.mode() & 0o077 != 0 || metadata.nlink() != 1 || metadata.uid() != uid {
                return Err(Error::other(
                    "Falcon keys must be owned by this user, owner-only, and singly linked",
                ));
            }
        }
        let mut bytes = Zeroizing::new(Vec::new());
        file.take(MAX_KEY_FILE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_KEY_FILE_BYTES || bytes.get(..8) != Some(b"DGXKEY02") {
            return Err(Error::other(
                "invalid or legacy Falcon key file; regenerate development keys for FN-DSA v2",
            ));
        }
        let count = usize::from(
            *bytes
                .get(8)
                .ok_or_else(|| Error::other("missing key count"))?,
        );
        if !(1..=MAX_PLOT_KEYS).contains(&count)
            || bytes.len() != 9 + (count * 2 + 1) * (897 + SECRET_KEY_BYTES)
        {
            return Err(Error::other("invalid Falcon key count or length"));
        }
        let mut signers = Vec::new();
        for pair in bytes[9..].as_chunks::<{ 897 + SECRET_KEY_BYTES }>().0 {
            signers.push(Arc::new(
                FalconSigner::from_secret_bytes(
                    pair[..897].try_into().map_err(Error::other)?,
                    &pair[897..],
                )
                .map_err(Error::other)?,
            ));
        }
        let wallet = signers
            .pop()
            .ok_or_else(|| Error::other("missing wallet key"))?;
        let plots = signers
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| FalconPlotKeyPair {
                plot: p[0].clone(),
                pool: p[1].clone(),
            })
            .collect();
        Ok(Self { plots, wallet })
    }
    pub fn plot(&self, id: Bytes48) -> Option<Arc<FalconSigner>> {
        self.plots
            .iter()
            .find(|p| public_key_id(&p.plot.public_key()) == id)
            .map(|p| p.plot.clone())
    }
    pub fn pool(&self, id: Bytes48) -> Option<Arc<FalconSigner>> {
        self.plots
            .iter()
            .find(|p| public_key_id(&p.pool.public_key()) == id)
            .map(|p| p.pool.clone())
    }
}
