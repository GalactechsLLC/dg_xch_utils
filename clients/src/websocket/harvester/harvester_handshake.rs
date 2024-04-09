use async_trait::async_trait;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::protocols::harvester::{HarvesterHandshake, HarvesterState};
use dg_xch_core::protocols::{ChiaMessage, MessageHandler, PeerMap};
use dg_xch_pos::PlotManagerAsync;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use log::{debug, info, warn};
use std::io::{Cursor, Error};
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct HarvesterHandshakeHandle<T: PlotManagerAsync> {
    pub plot_manager: Arc<RwLock<T>>,
    pub harvester_state: Arc<RwLock<HarvesterState>>,
}
#[async_trait]
impl<T: PlotManagerAsync + Send + Sync> MessageHandler for HarvesterHandshakeHandle<T> {
    async fn handle(
        &self,
        msg: Arc<ChiaMessage>,
        peer_id: Arc<Bytes32>,
        peers: PeerMap,
    ) -> Result<(), Error> {
        let mut cursor = Cursor::new(msg.data.clone());
        let peer = peers.read().await.get(&peer_id).cloned();
        let protocol_version = if let Some(peer) = peer.as_ref() {
            *peer.protocol_version.read().await
        } else {
            ChiaProtocolVersion::default()
        };
        let handshake = HarvesterHandshake::from_bytes(&mut cursor, protocol_version)?;
        info!("Handshake from farmer: {:?}", handshake);
        if handshake.farmer_public_keys.is_empty() && handshake.pool_public_keys.is_empty() {
            warn!("Farmer Failed to send keys");
        } else {
            self.plot_manager
                .write()
                .await
                .set_public_keys(handshake.farmer_public_keys, handshake.pool_public_keys);
        }
        info!("Got Keys, Loading Plots.");
        match self
            .plot_manager
            .write()
            .await
            .load_plots(self.harvester_state.clone())
            .await
        {
            Ok(_) => {
                debug!("Done Loading Plots");
            }
            Err(e) => {
                debug!("Error loading plots: {:?}", e);
            }
        }
        Ok(())
    }
}
