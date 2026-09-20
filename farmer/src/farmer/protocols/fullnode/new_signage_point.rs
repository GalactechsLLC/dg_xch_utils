use crate::PROTOCOL_VERSION;
use crate::farmer::FarmerSharedState;
use crate::farmer::config::Config;
use crate::harvesters::{Harvester, ProofHandler};
use async_trait::async_trait;
use dg_xch_clients::api::pool::PoolClient;
use dg_xch_clients::websocket::farmer::FarmerClient;
use dg_xch_core::blockchain::proof_of_space::calculate_prefix_bits;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_core::constants::POOL_SUB_SLOT_ITERS;
use dg_xch_core::protocols::farmer::{FarmerPoolState, MostRecentSignagePoint, NewSignagePoint};
use dg_xch_core::protocols::harvester::{NewSignagePointHarvester, PoolDifficulty};
use dg_xch_core::protocols::{ChiaMessage, MessageHandler, PeerMap};
use dg_xch_serialize::ChiaSerialize;
use log::{debug, error, warn};
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::io::{Cursor, Error};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use uuid::Uuid;

fn insert_signage_point(
    points: &mut HashMap<Bytes32, Vec<NewSignagePoint>>,
    point: NewSignagePoint,
) -> bool {
    match points.entry(point.challenge_chain_sp) {
        Entry::Occupied(mut entry) => {
            if entry.get().contains(&point) {
                return false;
            }
            entry.get_mut().push(point);
        }
        Entry::Vacant(entry) => {
            entry.insert(vec![point]);
        }
    }
    true
}

pub struct NewSignagePointHandle<P, O, T, H, C>
where
    P: PoolClient + Default + Sized + Sync + Send + 'static,
    O: ProofHandler<T, H, C> + Sync + Send + 'static,
    T: Sync + Send + 'static,
    H: Harvester<T, H, C> + Sync + Send + 'static,
    C: Sync + Send + Clone + 'static,
{
    pub id: Uuid,
    pub harvester_id: Bytes32,
    pub pool_state: Arc<RwLock<HashMap<Bytes32, FarmerPoolState>>>,
    pub pool_client: Arc<P>,
    pub signage_points: Arc<RwLock<HashMap<Bytes32, Vec<NewSignagePoint>>>>,
    pub cache_time: Arc<RwLock<HashMap<Bytes32, Instant>>>,
    pub shared_state: Arc<FarmerSharedState<T>>,
    pub harvester: Arc<H>,
    pub constants: ConsensusConstants,
    pub config: Arc<RwLock<Config<C>>>,
    pub client: Arc<RwLock<Option<FarmerClient<T>>>>,
    pub proof_handle: Arc<O>,
}
#[async_trait]
impl<P, O, T, H, C> MessageHandler for NewSignagePointHandle<P, O, T, H, C>
where
    P: PoolClient + Default + Sized + Sync + Send + 'static,
    O: ProofHandler<T, H, C> + Sync + Send + 'static,
    T: Sync + Send + 'static,
    H: Harvester<T, H, C> + Sync + Send + 'static,
    C: Sync + Send + Clone + 'static,
{
    async fn handle(
        &self,
        msg: Arc<ChiaMessage>,
        _peer_id: Arc<Bytes32>,
        _peers: PeerMap,
    ) -> Result<(), Error> {
        debug!("NewSignagePoint Message. Starting Deserialization.");
        let mut cursor = Cursor::new(msg.data.as_slice());
        let sp = NewSignagePoint::from_bytes(&mut cursor, PROTOCOL_VERSION)?;
        debug!("NewSignagePoint Message. Finished Deserialization.");
        if !insert_signage_point(&mut *self.signage_points.write().await, sp.clone()) {
            return Ok(());
        }
        if sp.sp_source_data.is_none() {
            error!("No SignagePoint Source Data Included for Farmer which Requires it");
        }
        let mut pool_difficulties = vec![];
        debug!("Generating Pool Difficulties");
        for (p2_singleton_puzzle_hash, pool_dict) in self.pool_state.read().await.iter() {
            if let Some(config) = &pool_dict.pool_config {
                if config.pool_url.is_empty() {
                    debug!("Self Pooling Detected for {p2_singleton_puzzle_hash}");
                    continue;
                } else if let Some(difficulty) = pool_dict.current_difficulty {
                    debug!(
                        "Using Difficulty {difficulty} for p2_singleton_puzzle_hash: {p2_singleton_puzzle_hash}"
                    );
                    pool_difficulties.push(PoolDifficulty {
                        difficulty,
                        sub_slot_iters: POOL_SUB_SLOT_ITERS,
                        pool_contract_puzzle_hash: *p2_singleton_puzzle_hash,
                    })
                } else {
                    warn!(
                        "No pool specific difficulty has been set for {p2_singleton_puzzle_hash}, check communication with the pool, skipping this signage point, pool: {}",
                        config.pool_url
                    );
                    continue;
                }
            }
        }
        debug!(
            "New Signage Point({}): {:?}",
            sp.signage_point_index, sp.challenge_hash
        );
        let now = Instant::now();
        let time_since_last_sp = now
            .duration_since(*self.shared_state.last_sp_timestamp.read().await)
            .as_millis();
        if let Some(m) = &*self.shared_state.metrics.read().await {
            m.signage_point_interval
                .observe(time_since_last_sp as f64 / 1000f64);
        }
        *self.shared_state.last_sp_timestamp.write().await = now;
        let filter_prefix_bits = calculate_prefix_bits(&self.constants, sp.peak_height);
        let sp_hash = sp.challenge_chain_sp;
        let harvester_point = Arc::new(NewSignagePointHarvester {
            challenge_hash: sp.challenge_hash,
            difficulty: sp.difficulty,
            sub_slot_iters: sp.sub_slot_iters,
            signage_point_index: sp.signage_point_index,
            sp_hash,
            pool_difficulties,
            filter_prefix_bits,
            last_tx_height: sp.last_tx_height,
        });
        self.cache_time
            .write()
            .await
            .insert(sp_hash, Instant::now());
        *self.shared_state.most_recent_sp.write().await = MostRecentSignagePoint {
            hash: sp.challenge_hash,
            index: sp.signage_point_index,
            timestamp: Instant::now(),
        };
        debug!("Sending NewSignagePoint to Harvesters, Using Filter Bits: {filter_prefix_bits}");
        let harvester_point = harvester_point.clone();
        let shared_state = self.shared_state.clone();
        let harvester = self.harvester.clone();
        let config = self.config.clone();
        let client = self.client.clone();
        tokio::spawn(async move {
            if let Err(e) = harvester
                .new_signage_point(
                    harvester_point,
                    O::load(shared_state, config, harvester.clone(), client.clone()).await?,
                )
                .await
            {
                error!("Error Handling Signage Point: {e}");
            }
            Ok::<(), Error>(())
        });
        debug!("Finished Processing SignagePoint: {sp_hash}");
        Ok(())
    }
}

#[cfg(test)]
mod cache_tests {
    use super::{HashMap, NewSignagePoint, insert_signage_point};

    #[test]
    fn repeated_signage_points_do_not_duplicate_work() {
        let mut points = HashMap::new();
        let mut point = NewSignagePoint {
            challenge_hash: [1; 32].into(),
            challenge_chain_sp: [2; 32].into(),
            reward_chain_sp: [3; 32].into(),
            difficulty: 1,
            sub_slot_iters: 64,
            signage_point_index: 4,
            peak_height: 100,
            last_tx_height: 100,
            sp_source_data: None,
        };
        assert!(insert_signage_point(&mut points, point.clone()));
        assert!(!insert_signage_point(&mut points, point.clone()));
        point.reward_chain_sp = [4; 32].into();
        assert!(insert_signage_point(&mut points, point.clone()));
        assert_eq!(points[&point.challenge_chain_sp].len(), 2);
    }
}
