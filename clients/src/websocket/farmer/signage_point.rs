use async_trait::async_trait;
use dg_xch_core::blockchain::proof_of_space::calculate_prefix_bits;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_core::consensus::pot_iterations::POOL_SUB_SLOT_ITERS;
#[cfg(feature = "metrics")]
use dg_xch_core::protocols::farmer::FarmerMetrics;
use dg_xch_core::protocols::farmer::{
    FarmerPoolState, FarmerRunningState, MostRecentSignagePoint, NewSignagePoint,
};
use dg_xch_core::protocols::harvester::{NewSignagePointHarvester, PoolDifficulty};
use dg_xch_core::protocols::{
    ChiaMessage, MessageHandler, NodeType, PeerMap, ProtocolMessageTypes, SocketPeer,
};
use dg_xch_serialize::ChiaSerialize;
use log::{debug, info, warn};
use std::collections::HashMap;
use std::io::{Cursor, Error};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;

pub struct NewSignagePointHandle {
    pub constants: &'static ConsensusConstants,
    pub harvester_peers: PeerMap,
    pub signage_points: Arc<Mutex<HashMap<Bytes32, Vec<NewSignagePoint>>>>,
    pub pool_state: Arc<Mutex<HashMap<Bytes32, FarmerPoolState>>>,
    pub cache_time: Arc<Mutex<HashMap<Bytes32, Instant>>>,
    pub running_state: Arc<Mutex<FarmerRunningState>>,
    pub most_recent_sp: Arc<Mutex<MostRecentSignagePoint>>,
    #[cfg(feature = "metrics")]
    pub metrics: Arc<Mutex<Option<FarmerMetrics>>>,
}
#[async_trait]
impl MessageHandler for NewSignagePointHandle {
    async fn handle(
        &self,
        msg: Arc<ChiaMessage>,
        _peer_id: Arc<Bytes32>,
        _peers: PeerMap,
    ) -> Result<(), Error> {
        let mut cursor = Cursor::new(&msg.data);
        let sp = NewSignagePoint::from_bytes(&mut cursor)?;
        let mut pool_difficulties = vec![];
        for (p2_singleton_puzzle_hash, pool_dict) in self.pool_state.lock().await.iter() {
            if let Some(config) = &pool_dict.pool_config {
                if config.pool_url.is_empty() {
                    //Self Pooling
                    continue;
                }
                if let Some(difficulty) = pool_dict.current_difficulty {
                    debug!("Setting Difficulty for pool: {}", difficulty);
                    pool_difficulties.push(PoolDifficulty {
                        difficulty,
                        sub_slot_iters: POOL_SUB_SLOT_ITERS,
                        pool_contract_puzzle_hash: *p2_singleton_puzzle_hash,
                    })
                } else {
                    warn!("No pool specific difficulty has been set for {p2_singleton_puzzle_hash}, check communication with the pool, skipping this signage point, pool: {}", &config.pool_url);
                    continue;
                }
            }
        }
        info!(
            "New Signage Point({}): {:?}",
            sp.signage_point_index, sp.challenge_hash
        );
        let harvester_point = NewSignagePointHarvester {
            challenge_hash: sp.challenge_hash,
            difficulty: sp.difficulty,
            sub_slot_iters: sp.sub_slot_iters,
            signage_point_index: sp.signage_point_index,
            sp_hash: sp.challenge_chain_sp,
            pool_difficulties,
            filter_prefix_bits: calculate_prefix_bits(self.constants, sp.peak_height),
        };
        let msg = Message::Binary(
            ChiaMessage::new(
                ProtocolMessageTypes::NewSignagePointHarvester,
                &harvester_point,
                None,
            )
            .to_bytes(),
        );
        let peers: Vec<Arc<SocketPeer>> = self
            .harvester_peers
            .lock()
            .await
            .values()
            .cloned()
            .collect();
        for peer in peers {
            if *peer.node_type.lock().await == NodeType::Harvester {
                let _ = peer.websocket.lock().await.send(msg.clone()).await;
            }
        }
        {
            //Lock Scope
            let mut signage_points = self.signage_points.lock().await;
            if signage_points.get(&sp.challenge_chain_sp).is_none() {
                signage_points.insert(sp.challenge_chain_sp, vec![]);
            }
        }
        #[cfg(feature = "metrics")]
        {
            let now = Instant::now();
            let sums = self
                .pool_state
                .lock()
                .await
                .iter_mut()
                .map(|(_, s)| {
                    s.points_acknowledged_24h
                        .retain(|(i, _)| now.duration_since(*i).as_secs() <= 60 * 60 * 24);
                    s.points_found_24h
                        .retain(|(i, _)| now.duration_since(*i).as_secs() <= 60 * 60 * 24);
                    (
                        s.points_acknowledged_24h.iter().map(|(_, v)| *v).sum(),
                        s.points_found_24h.iter().map(|(_, v)| *v).sum(),
                    )
                })
                .collect::<Vec<(u64, u64)>>();
            if let Some(r) = self.metrics.lock().await.as_mut() {
                if let Some(c) = &mut r.points_acknowledged_24h {
                    c.set(sums.iter().map(|(v, _)| *v).sum());
                }
                if let Some(c) = &mut r.points_found_24h {
                    c.set(sums.iter().map(|(_, v)| *v).sum());
                }
                if let Some(c) = &mut r.last_signage_point_index {
                    c.set(sp.signage_point_index as u64);
                }
            }
        }
        if let Some(sps) = self
            .signage_points
            .lock()
            .await
            .get_mut(&sp.challenge_chain_sp)
        {
            sps.push(sp.clone());
        }
        *self.most_recent_sp.lock().await = MostRecentSignagePoint {
            hash: sp.challenge_chain_sp,
            index: sp.signage_point_index,
            timestamp: Instant::now(),
        };
        self.cache_time
            .lock()
            .await
            .insert(sp.challenge_chain_sp, Instant::now());
        Ok(())
    }
}
