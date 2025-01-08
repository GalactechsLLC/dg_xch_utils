use async_trait::async_trait;
use dg_xch_core::blockchain::proof_of_space::calculate_prefix_bits;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_core::constants::POOL_SUB_SLOT_ITERS;
#[cfg(feature = "metrics")]
use dg_xch_core::protocols::farmer::FarmerMetrics;
use dg_xch_core::protocols::farmer::{
    FarmerPoolState, FarmerRunningState, MostRecentSignagePoint, NewSignagePoint,
};
use dg_xch_core::protocols::harvester::{NewSignagePointHarvester, PoolDifficulty};
use dg_xch_core::protocols::{
    ChiaMessage, MessageHandler, NodeType, PeerMap, ProtocolMessageTypes, SocketPeer,
};
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use log::{debug, info, warn};
use std::collections::HashMap;
use std::io::{Cursor, Error};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use tokio_tungstenite::tungstenite::Message;

pub struct NewSignagePointHandle {
    pub constants: Arc<ConsensusConstants>,
    pub harvester_peers: PeerMap,
    pub signage_points: Arc<RwLock<HashMap<Bytes32, Vec<NewSignagePoint>>>>,
    pub pool_state: Arc<RwLock<HashMap<Bytes32, FarmerPoolState>>>,
    pub cache_time: Arc<RwLock<HashMap<Bytes32, Instant>>>,
    pub running_state: Arc<RwLock<FarmerRunningState>>,
    pub most_recent_sp: Arc<RwLock<MostRecentSignagePoint>>,
    #[cfg(feature = "metrics")]
    pub metrics: Arc<RwLock<Option<FarmerMetrics>>>,
}
#[async_trait]
impl MessageHandler for NewSignagePointHandle {
    #[allow(clippy::too_many_lines)]
    async fn handle(
        &self,
        msg: Arc<ChiaMessage>,
        peer_id: Arc<Bytes32>,
        peers: PeerMap,
    ) -> Result<(), Error> {
        let mut cursor = Cursor::new(&msg.data);
        let peer = peers.read().await.get(&peer_id).cloned();
        let protocol_version = if let Some(peer) = peer.as_ref() {
            *peer.protocol_version.read().await
        } else {
            ChiaProtocolVersion::default()
        };
        let sp = NewSignagePoint::from_bytes(&mut cursor, protocol_version)?;
        let mut pool_difficulties = vec![];
        for (p2_singleton_puzzle_hash, pool_dict) in self.pool_state.read().await.iter() {
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
                    });
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
            filter_prefix_bits: calculate_prefix_bits(self.constants.as_ref(), sp.peak_height),
        };
        let peers: Vec<Arc<SocketPeer>> = self
            .harvester_peers
            .read()
            .await
            .values()
            .cloned()
            .collect();
        for peer in peers {
            if *peer.node_type.read().await == NodeType::Harvester {
                let protocol_version = *peer.protocol_version.read().await;
                let _ = peer
                    .websocket
                    .write()
                    .await
                    .send(
                        Message::Binary(
                            ChiaMessage::new(
                                ProtocolMessageTypes::NewSignagePointHarvester,
                                protocol_version,
                                &harvester_point,
                                None,
                            )
                            .to_bytes(protocol_version),
                        )
                        .clone(),
                    )
                    .await;
            }
        }
        {
            //Lock Scope
            let mut signage_points = self.signage_points.write().await;
            if signage_points.get(&sp.challenge_chain_sp).is_none() {
                signage_points.insert(sp.challenge_chain_sp, vec![]);
            }
        }
        #[cfg(feature = "metrics")]
        {
            let now = Instant::now();
            for (v, s) in self.pool_state.write().await.iter_mut() {
                s.points_acknowledged_24h
                    .retain(|(i, _)| now.duration_since(*i).as_secs() <= 60 * 60 * 24);
                s.points_found_24h
                    .retain(|(i, _)| now.duration_since(*i).as_secs() <= 60 * 60 * 24);
                if let Some(r) = self.metrics.read().await.as_ref() {
                    if let Some(c) = &r.points_acknowledged_24h {
                        c.with_label_values(&[&v.to_string()])
                            .set(s.points_acknowledged_24h.iter().map(|(_, v)| *v).sum());
                    }
                    if let Some(c) = &r.points_found_24h {
                        c.with_label_values(&[&v.to_string()])
                            .set(s.points_found_24h.iter().map(|(_, v)| *v).sum());
                    }
                    if let Some(c) = &r.last_signage_point_index {
                        c.set(sp.signage_point_index as u64);
                    }
                }
            }
        }
        if let Some(sps) = self
            .signage_points
            .write()
            .await
            .get_mut(&sp.challenge_chain_sp)
        {
            sps.push(sp.clone());
        }
        *self.most_recent_sp.write().await = MostRecentSignagePoint {
            hash: sp.challenge_chain_sp,
            index: sp.signage_point_index,
            timestamp: Instant::now(),
        };
        self.cache_time
            .write()
            .await
            .insert(sp.challenge_chain_sp, Instant::now());
        Ok(())
    }
}
