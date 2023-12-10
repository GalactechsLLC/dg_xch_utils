use async_trait::async_trait;
use dg_xch_core::blockchain::sized_bytes::{Bytes32};
use dg_xch_core::protocols::harvester::{HarvesterHandshake, HarvesterState};
use dg_xch_core::protocols::{ChiaMessage, MessageHandler, PeerMap};
use dg_xch_pos::PlotManagerAsync;
use dg_xch_serialize::ChiaSerialize;
use log::{debug, info, warn};
use std::io::{Cursor, Error};
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct HarvesterHandshakeHandle<T: PlotManagerAsync> {
    pub plot_manager: Arc<Mutex<T>>,
    pub harvester_state: Arc<Mutex<HarvesterState>>,
}
#[async_trait]
impl<T: PlotManagerAsync + Send + Sync> MessageHandler for HarvesterHandshakeHandle<T> {
    async fn handle(
        &self,
        msg: Arc<ChiaMessage>,
        _peer_id: Arc<Bytes32>,
        _peers: PeerMap,
    ) -> Result<(), Error> {
        let mut cursor = Cursor::new(msg.data.clone());
        let handshake = HarvesterHandshake::from_bytes(&mut cursor)?;
        info!("Handshake from farmer: {:?}", handshake);
        if handshake.farmer_public_keys.is_empty() && handshake.pool_public_keys.is_empty() {
            warn!("Farmer Failed to send keys");
        } else {
            self.plot_manager
                .lock()
                .await
                .set_public_keys(handshake.farmer_public_keys, handshake.pool_public_keys);
        }
        debug!("Set Key... Loading Plots");
        match self
            .plot_manager
            .lock()
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
