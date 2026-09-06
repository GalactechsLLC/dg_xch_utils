use blst::min_pk::SecretKey;
use dg_xch_core::blockchain::proof_of_space::{calculate_plot_id_v2, generate_plot_public_key};
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48, Bytes96};
use dg_xch_core::clvm::bls_bindings::sign_prepend;
use dg_xch_core::traits::SizedBytes;
use dg_xch_core::utils::hash_256;
use dg_xch_keys::{master_sk_to_farmer_sk, master_sk_to_local_sk, master_sk_to_pool_sk};
use std::io::Error;

/// Framing tag for the master key pre-image, so plot keys cannot collide with another value
/// derived from the same campaign seed.
const PLOT_KEY_DOMAIN_SEP: &[u8] = b"dg_xch_simulator/plot/v1";

/// The key material behind one plot, derived from `(campaign_seed, plot_index)` so a campaign
/// re-derives the same plot ids and can reuse plots an earlier run already wrote.
///
/// These are pool public key plots: `pool_contract_puzzle_hash` is unset and the taproot term is
/// left out of the plot public key, which is what a plotter does for an OG plot.
#[derive(Debug, Clone)]
pub struct PlotKeys {
    pub master: SecretKey,
    pub farmer: SecretKey,
    pub pool: SecretKey,
    pub local: SecretKey,
    pub plot_public_key: Bytes48,
    pub pool_public_key: Bytes48,
}

impl PlotKeys {
    pub fn derive(campaign_seed: u64, plot_index: u32) -> Result<Self, Error> {
        let mut pre_image = Vec::with_capacity(PLOT_KEY_DOMAIN_SEP.len() + 12);
        pre_image.extend_from_slice(PLOT_KEY_DOMAIN_SEP);
        pre_image.extend_from_slice(&campaign_seed.to_le_bytes());
        pre_image.extend_from_slice(&plot_index.to_le_bytes());
        let master = SecretKey::key_gen_v3(&hash_256(&pre_image), &[])
            .map_err(|e| Error::other(format!("{e:?}")))?;

        let farmer = master_sk_to_farmer_sk(&master)?;
        let pool = master_sk_to_pool_sk(&master)?;
        let local = master_sk_to_local_sk(&master)?;

        let plot_public_key = Bytes48::parse(
            &generate_plot_public_key(&local.sk_to_pk(), &farmer.sk_to_pk(), false)?.to_bytes(),
        )?;
        let pool_public_key = Bytes48::parse(&pool.sk_to_pk().to_bytes())?;

        Ok(Self {
            master,
            farmer,
            pool,
            local,
            plot_public_key,
            pool_public_key,
        })
    }

    /// The aggregate plot signature over `msg`: the local and farmer keys each sign with the plot
    /// public key prepended, aggregated. Verifies under `plot_public_key`, which is what the header
    /// validator checks for the signage-point and foliage signatures.
    pub fn sign(&self, msg: Bytes32) -> Result<Bytes96, Error> {
        let plot_pk = self.local.sk_to_pk();
        let farmer_pk = self.farmer.sk_to_pk();
        // The prepend is the aggregate plot public key: local + farmer.
        let mut agg = blst::min_pk::AggregatePublicKey::from_public_key(&plot_pk);
        agg.add_public_key(&farmer_pk, false)
            .map_err(|e| Error::other(format!("{e:?}")))?;
        let prepend = agg.to_public_key();
        let sig_local = sign_prepend(&self.local, msg.as_ref(), &prepend);
        let sig_farmer = sign_prepend(&self.farmer, msg.as_ref(), &prepend);
        let mut agg_sig = blst::min_pk::AggregateSignature::from_signature(&sig_local);
        agg_sig
            .add_signature(&sig_farmer, false)
            .map_err(|e| Error::other(format!("{e:?}")))?;
        Bytes96::parse(&agg_sig.to_signature().to_bytes())
            .map_err(|e| Error::other(format!("{e:?}")))
    }

    /// The plot id a v2 proof for this plot derives, and so the id the plot must be created under.
    #[must_use]
    pub fn plot_id(&self, strength: u8, plot_index: u16, meta_group: u8) -> Bytes32 {
        calculate_plot_id_v2(
            strength,
            self.plot_public_key,
            Some(self.pool_public_key),
            None,
            plot_index,
            meta_group,
        )
    }
}

#[cfg(test)]
#[path = "../tests/unit/plots/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/unit/plots/sign_tests.rs"]
mod sign_tests;
