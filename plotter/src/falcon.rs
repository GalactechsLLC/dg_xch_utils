//! Falcon plots bind key identifiers directly; there is no BLS aggregate or
//! taproot key derivation. Only public identifiers are written into the memo.
use crate::{PlotInfo, PoolBinding, format, publish_with_writer};
use dg_xch_core::blockchain::proof_of_space::calculate_plot_id_v2;
use dg_xch_pos2::{compact::CompactPlot, params::ProofParams, plotting::PlotLimits};
use std::{io::Error, path::Path, sync::atomic::AtomicBool};

pub struct FalconPlotRequest {
    pub plot_public_key_id: [u8; 48],
    pub pool: PoolBinding,
    pub k: u8,
    pub strength: u8,
    pub index: u16,
    pub meta_group: u8,
    pub testnet: bool,
}

impl FalconPlotRequest {
    pub fn identity(&self) -> Result<(ProofParams, Vec<u8>), Error> {
        let (pool, contract) = match self.pool {
            PoolBinding::PublicKey(id) => (Some(id.into()), None),
            PoolBinding::Contract(hash) => (None, Some(hash.into())),
        };
        let id = calculate_plot_id_v2(
            self.strength,
            self.plot_public_key_id.into(),
            pool,
            contract,
            self.index,
            self.meta_group,
        );
        let mut memo = b"DGXFAL02".to_vec();
        memo.resize(32, 0);
        match self.pool {
            PoolBinding::PublicKey(id) => memo.extend_from_slice(&id),
            PoolBinding::Contract(hash) => memo.extend_from_slice(&hash),
        }
        memo.extend_from_slice(&self.plot_public_key_id);
        Ok((
            ProofParams::new(id, self.k, self.strength, self.testnet)?,
            memo,
        ))
    }
}

pub fn create_with_engine(
    request: &FalconPlotRequest,
    destination: &Path,
    limits: PlotLimits,
    cancelled: &AtomicBool,
    engine: impl FnOnce(ProofParams, PlotLimits, &AtomicBool) -> Result<CompactPlot, Error>,
) -> Result<PlotInfo, Error> {
    let (params, memo) = request.identity()?;
    publish_with_writer(
        params,
        &memo,
        destination,
        cancelled,
        |params, output, memo| {
            let plot = engine(params.clone(), limits, cancelled)?;
            if plot.params() != &params {
                return Err(Error::other(
                    "plot engine returned different Falcon parameters",
                ));
            }
            format::write_compact(
                output,
                &plot,
                request.index,
                request.meta_group,
                memo,
                cancelled,
            )
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use dg_xch_core::signatures::SignatureProfile;

    #[test]
    fn solo_and_portable_memos_bind_keys_contract_and_plot_index() {
        let mut request = FalconPlotRequest {
            plot_public_key_id: [1; 48],
            pool: PoolBinding::PublicKey([2; 48]),
            k: 28,
            strength: 2,
            index: 3,
            meta_group: 0,
            testnet: true,
        };
        let (solo, memo) = request.identity().unwrap();
        assert_eq!(memo.len(), 128);
        assert_eq!(
            crate::identity_for_profile(&memo, 2, 3, 0, SignatureProfile::FnDsa512V2).unwrap(),
            solo.plot_id()
        );
        request.pool = PoolBinding::Contract([4; 32]);
        let (portable, memo) = request.identity().unwrap();
        assert_eq!(memo.len(), 112);
        assert_ne!(solo.plot_id(), portable.plot_id());
        assert_eq!(
            crate::identity_for_profile(&memo, 2, 3, 0, SignatureProfile::FnDsa512V2).unwrap(),
            portable.plot_id()
        );
        request.index += 1;
        assert_ne!(portable.plot_id(), request.identity().unwrap().0.plot_id());
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("existing.plot");
        std::fs::write(&destination, b"preserve existing data").unwrap();
        let result = create_with_engine(
            &request,
            &destination,
            PlotLimits {
                memory_bytes: 1024,
                max_entries: 1,
                max_work: 1,
            },
            &AtomicBool::new(false),
            |_, _, _| panic!("existing destination must be rejected before plotting"),
        );
        assert!(result.is_err());
        assert_eq!(
            std::fs::read(destination).unwrap(),
            b"preserve existing data"
        );
    }
}

#[cfg(test)]
mod shared_writer_tests {
    use super::*;
    use crate::{PlotRequest, SignatureProfile, create_with_writer_for_profile};
    use std::cell::Cell;

    #[test]
    fn shared_writer_selects_falcon_identity_without_bls_key_derivation() {
        let directory = tempfile::tempdir().unwrap();
        for pool in [
            PoolBinding::PublicKey([2; 48]),
            PoolBinding::Contract([3; 32]),
        ] {
            let request = PlotRequest {
                farmer_public_key: [1; 48],
                pool,
                k: 28,
                strength: 2,
                index: 7,
                meta_group: 0,
                testnet: true,
            };
            let called = Cell::new(false);
            let path = directory.path().join("plot.plot");
            let result = create_with_writer_for_profile(
                &request,
                SignatureProfile::FnDsa512V2,
                &path,
                &AtomicBool::new(false),
                |params, _, memo| {
                    called.set(true);
                    assert!(memo.starts_with(b"DGXFAL02"));
                    assert_eq!(
                        crate::identity_for_profile(memo, 2, 7, 0, SignatureProfile::FnDsa512V2)
                            .unwrap(),
                        params.plot_id()
                    );
                    Err(Error::other("stop before expensive plotting"))
                },
            );
            assert!(result.is_err());
            assert!(called.get());
            assert!(!path.exists());
            // The same bytes are not a valid BLS farmer identity.
            assert!(
                create_with_writer_for_profile(
                    &request,
                    SignatureProfile::Bls12381Aug,
                    &path,
                    &AtomicBool::new(false),
                    |_, _, _| panic!("invalid BLS key reached writer")
                )
                .is_err()
            );
        }
    }
}
