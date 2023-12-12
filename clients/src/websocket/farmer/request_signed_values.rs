use async_trait::async_trait;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::protocols::error::RecentErrors;
use dg_xch_core::protocols::farmer::{FarmerIdentifier, RequestSignedValues};
use dg_xch_core::protocols::harvester::RequestSignatures;
use dg_xch_core::protocols::{ChiaMessage, MessageHandler, PeerMap, ProtocolMessageTypes};
use dg_xch_serialize::ChiaSerialize;
use std::collections::HashMap;
use std::io::{Cursor, Error, ErrorKind};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;

pub struct RequestSignedValuesHandle {
    pub quality_to_identifiers: Arc<Mutex<HashMap<Bytes32, FarmerIdentifier>>>,
    pub recent_errors: Arc<Mutex<RecentErrors<String>>>,
    pub harvester_peers: PeerMap,
}
#[async_trait]
impl MessageHandler for RequestSignedValuesHandle {
    async fn handle(
        &self,
        msg: Arc<ChiaMessage>,
        _peer_id: Arc<Bytes32>,
        _peers: PeerMap,
    ) -> Result<(), Error> {
        let mut cursor = Cursor::new(&msg.data);
        let request = RequestSignedValues::from_bytes(&mut cursor)?;
        if let Some(identifier) = self
            .quality_to_identifiers
            .lock()
            .await
            .get(&request.quality_string)
        {
            if let Some(peer) = self
                .harvester_peers
                .lock()
                .await
                .get(&identifier.peer_node_id)
            {
                let _ = peer
                    .websocket
                    .lock()
                    .await
                    .send(Message::Binary(
                        ChiaMessage::new(
                            ProtocolMessageTypes::RequestSignatures,
                            &RequestSignatures {
                                plot_identifier: identifier.plot_identifier.clone(),
                                challenge_hash: identifier.challenge_hash,
                                sp_hash: identifier.sp_hash,
                                messages: vec![
                                    request.foliage_block_data_hash,
                                    request.foliage_transaction_block_hash,
                                ],
                            },
                            None,
                        )
                        .to_bytes(),
                    ))
                    .await;
            }
            Ok(())
        } else {
            self.recent_errors
                .lock()
                .await
                .add(format!("Do not have quality {}", &request.quality_string));
            Err(Error::new(
                ErrorKind::NotFound,
                format!("Do not have quality {}", &request.quality_string),
            ))
        }
    }
}
