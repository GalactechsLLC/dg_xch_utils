use dg_xch_clients::rpc::full_node::{FullnodeAPI, FullnodeClient};
use dg_xch_core::blockchain::{
    block_record::BlockRecord, coin_record::CoinRecord, full_block::FullBlock, sized_bytes::Bytes32,
};
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_node::{engine::Engine, primitives::NativePrimitives};
use dg_xch_stores::{BlockStore, CoinStore, SqliteStore};
use serde::{Deserialize, Serialize};
use std::io::Error;
use std::path::Path;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncMode {
    #[default]
    Trusted,
    Untrusted,
}

// This store belongs exclusively to the wallet. Every block is validated from genesis;
// it is never seeded from another node's database or an assume-valid checkpoint.
pub(crate) struct VerifiedChain {
    engine: Engine<SqliteStore, NativePrimitives>,
    genesis: Bytes32,
    fetching: Option<(Bytes32, u32)>,
}

impl VerifiedChain {
    pub async fn open(
        path: &Path,
        constants: ConsensusConstants,
        genesis: Bytes32,
    ) -> Result<Self, Error> {
        drop(crate::storage::private_file(path)?);
        let path = path.to_owned();
        // Keep SQLx's migration future on one worker; it cannot cross task threads.
        let store = tokio::task::spawn_blocking(move || {
            tokio::runtime::Handle::current().block_on(SqliteStore::open(&path))
        })
        .await
        .map_err(Error::other)?
        .map_err(Error::other)?;
        if let Some(record) = store
            .get_block_record_by_height(0)
            .await
            .map_err(Error::other)?
        {
            if record.header_hash != genesis {
                return Err(Error::other("validated wallet chain has the wrong genesis"));
            }
        } else if store.get_peak().await.map_err(Error::other)?.is_some() {
            return Err(Error::other("validated wallet chain must start at genesis"));
        }
        let mut engine = Engine::new(store, NativePrimitives, constants).with_enforced_coin_rules();
        engine.warm_cache_from_store().await.map_err(Error::other)?;
        Ok(Self {
            engine,
            genesis,
            fetching: None,
        })
    }

    async fn add_block(&mut self, block: &FullBlock) -> Result<(), Error> {
        if block.height() == 0 && block.header_hash()? != self.genesis {
            return Err(Error::other("untrusted node supplied the wrong genesis"));
        }
        self.engine.add_block(block).await.map_err(Error::other)?;
        Ok(())
    }

    pub async fn sync(&mut self, client: &FullnodeClient, peak: &BlockRecord) -> Result<(), Error> {
        let store = self.engine.store();
        let mut start = 0;
        if let Some((local_hash, local_height)) = store.get_peak().await.map_err(Error::other)? {
            if local_hash == peak.header_hash {
                if local_height != peak.height {
                    return Err(Error::other("untrusted peak height mismatch"));
                }
                return Ok(());
            }
            // Find the common ancestor. Remote height records guide fetching only;
            // the engine authenticates every fetched block and selects by chain weight.
            let mut low = 0;
            let mut high = local_height.min(peak.height);
            while low <= high {
                let height = low + (high - low) / 2;
                let remote = client.get_block_record_by_height(height).await?;
                let local = store
                    .get_block_record_by_height(height)
                    .await
                    .map_err(Error::other)?;
                if local.is_some_and(|record| record.header_hash == remote.header_hash) {
                    start = height
                        .checked_add(1)
                        .ok_or_else(|| Error::other("chain height overflow"))?;
                    low = start;
                } else if height == 0 {
                    break;
                } else {
                    high = height - 1;
                }
            }
        }
        if let Some((last_hash, next)) = self.fetching
            && next > 0
            && next <= peak.height.saturating_add(1)
            && client
                .get_block_record_by_height(next - 1)
                .await?
                .header_hash
                == last_hash
        {
            start = next;
        }
        let end = peak.height.min(start.saturating_add(63));
        for height in start..=end {
            let record = client.get_block_record_by_height(height).await?;
            let block = client.get_block(&record.header_hash).await?;
            if block.height() != height || block.header_hash()? != record.header_hash {
                return Err(Error::other("untrusted node returned a different block"));
            }
            self.add_block(&block).await?;
            self.fetching = Some((record.header_hash, height.saturating_add(1)));
        }
        if self
            .engine
            .store()
            .get_peak()
            .await
            .map_err(Error::other)?
            .map(|p| p.0)
            != Some(peak.header_hash)
        {
            return Err(Error::other(format!(
                "wallet validation reached height {end}; awaiting validated peak {}",
                peak.height
            )));
        }
        self.fetching = None;
        Ok(())
    }

    pub async fn coins(&self, hashes: &[Bytes32]) -> Result<Vec<CoinRecord>, Error> {
        const LIMIT: usize = 100_000;
        let states = self
            .engine
            .store()
            .get_coin_states_by_puzzle_hashes(hashes, 0, true, LIMIT)
            .await
            .map_err(Error::other)?;
        if states.len() == LIMIT {
            return Err(Error::other(
                "validated wallet coin discovery limit exceeded",
            ));
        }
        let ids: Vec<_> = states.iter().map(|state| state.coin.name()).collect();
        self.engine
            .store()
            .get_coin_records(&ids)
            .await
            .map_err(Error::other)
    }

    pub async fn hinted_coins(&self, hint: &Bytes32) -> Result<Vec<CoinRecord>, Error> {
        let ids = self
            .engine
            .store()
            .get_coins_for_hint(hint, 1001)
            .await
            .map_err(Error::other)?;
        if ids.len() > 1000 {
            return Err(Error::other("validated asset discovery limit exceeded"));
        }
        self.engine
            .store()
            .get_coin_records(&ids)
            .await
            .map_err(Error::other)
    }

    pub async fn coin(&self, id: &Bytes32) -> Result<Option<CoinRecord>, Error> {
        self.engine
            .store()
            .get_coin_record(id)
            .await
            .map_err(Error::other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dg_xch_core::consensus::{
        constants::SIMULATOR,
        overrides::{ConsensusOverrides, apply_overrides},
    };
    use dg_xch_simulator_lib::{
        factory::{build_genesis_full, build_genesis_unfinished, farm_genesis},
        pos2::PlotSet,
    };

    #[tokio::test]
    async fn untrusted_chain_validates_genesis_and_reopens() {
        let constants = apply_overrides(
            SIMULATOR,
            &ConsensusOverrides {
                hard_fork2_height: Some(0),
                plot_size_v2: Some(18),
                number_zero_bits_plot_filter_v2: Some(0),
                difficulty_constant_factor: Some(2u128.pow(25)),
                difficulty_starting: Some(7),
                discriminant_size_bits: Some(16.into()),
                sub_slot_iters_starting: Some(65_536),
                ..Default::default()
            },
        );
        let directory = tempfile::tempdir().unwrap();
        let plots = PlotSet::setup(&directory.path().join("plots"), 5, 4, 18, 2, false).unwrap();
        let proof = farm_genesis(
            &constants,
            &plots,
            constants.difficulty_starting,
            constants.sub_slot_iters_starting,
        )
        .unwrap();
        let unfinished = build_genesis_unfinished(
            &constants,
            &proof,
            &plots.plots[proof.plot_index].keys,
            1_700_000_000,
        )
        .unwrap();
        let block = build_genesis_full(&constants, &unfinished, &proof).unwrap();
        let genesis = block.header_hash().unwrap();
        let path = directory.path().join("validated.sqlite");
        let mut chain = VerifiedChain::open(&path, constants, genesis)
            .await
            .unwrap();
        let mut invalid = block.clone();
        invalid.reward_chain_block.weight += 1;
        assert!(chain.add_block(&invalid).await.is_err());
        assert!(chain.engine.store().get_peak().await.unwrap().is_none());
        let mut invalid_proof = block.clone();
        invalid_proof.challenge_chain_ip_proof.witness.bytes = vec![0u8; 100];
        assert_eq!(invalid_proof.header_hash().unwrap(), genesis);
        assert!(chain.add_block(&invalid_proof).await.is_err());
        assert!(chain.engine.store().get_peak().await.unwrap().is_none());
        chain.add_block(&block).await.unwrap();
        assert_eq!(
            chain.engine.store().get_peak().await.unwrap(),
            Some((genesis, 0))
        );
        drop(chain);
        let chain = VerifiedChain::open(&path, constants, genesis)
            .await
            .unwrap();
        assert_eq!(
            chain.engine.store().get_peak().await.unwrap(),
            Some((genesis, 0))
        );
        drop(chain);
        assert!(
            VerifiedChain::open(&path, constants, Bytes32::default())
                .await
                .is_err()
        );
    }
}
