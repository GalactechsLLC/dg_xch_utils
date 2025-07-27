use async_trait::async_trait;
use blst::min_pk::{PublicKey, SecretKey};
use dg_xch_core::blockchain::proof_of_space::generate_plot_public_key;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::clvm::bls_bindings::sign_prepend;
use dg_xch_core::protocols::harvester::{RequestSignatures, RespondSignatures};
use dg_xch_core::protocols::{ChiaMessage, MessageHandler, PeerMap, ProtocolMessageTypes};
use dg_xch_keys::master_sk_to_local_sk;
use dg_xch_pos::{PathInfo, PlotManagerAsync};
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use log::{debug, error};
use std::io::{Cursor, Error, ErrorKind};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_tungstenite::tungstenite::Message;

pub struct RequestSignaturesHandle<T> {
    pub plot_manager: Arc<RwLock<T>>,
}
#[async_trait]
impl<T: PlotManagerAsync + Send + Sync> MessageHandler for RequestSignaturesHandle<T> {
    async fn handle(
        &self,
        msg: Arc<ChiaMessage>,
        peer_id: Arc<Bytes32>,
        peers: PeerMap,
    ) -> Result<(), Error> {
        debug!("{:?}", msg.msg_type);
        let mut cursor = Cursor::new(msg.data.clone());
        let peer = peers.read().await.get(&peer_id).cloned();
        let protocol_version = if let Some(peer) = peer.as_ref() {
            *peer.protocol_version.read().await
        } else {
            ChiaProtocolVersion::default()
        };
        let request_signatures = RequestSignatures::from_bytes(&mut cursor, protocol_version)?;
        let file_name = request_signatures.plot_identifier.split_at(64).1;
        let memo = match self.plot_manager.read().await.plots().get(&PathInfo {
            path: PathBuf::default(),
            file_name: file_name.to_string(),
        }) {
            None => {
                debug!("Failed to find plot info for plot: {file_name}");
                return Err(Error::new(
                    ErrorKind::NotFound,
                    format!("Failed to find plot info for plot: {file_name}"),
                ));
            }
            Some(info) => *info.reader.header().memo(),
        };
        let local_master_secret = SecretKey::from_bytes(memo.local_master_secret_key.as_ref())
            .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{e:?}")))?;
        let local_sk = master_sk_to_local_sk(&local_master_secret)?;
        let agg_pk = generate_plot_public_key(
            &local_sk.sk_to_pk(),
            &PublicKey::from_bytes(memo.farmer_public_key.as_ref())
                .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{e:?}")))?,
            memo.pool_contract_puzzle_hash.is_some(),
        )?;
        let mut message_signatures = vec![];
        for msg in request_signatures.messages {
            let sig = sign_prepend(&local_sk, msg.as_ref(), &agg_pk);
            message_signatures.push((msg, sig.to_bytes().into()));
        }
        if let Some(peer) = peers.read().await.get(peer_id.as_ref()).cloned() {
            let _ = peer
                .websocket
                .write()
                .await
                .send(Message::Binary(
                    ChiaMessage::new(
                        ProtocolMessageTypes::RespondSignatures,
                        protocol_version,
                        &RespondSignatures {
                            plot_identifier: request_signatures.plot_identifier,
                            challenge_hash: request_signatures.challenge_hash,
                            sp_hash: request_signatures.sp_hash,
                            local_pk: local_sk.sk_to_pk().to_bytes().into(),
                            farmer_pk: memo.farmer_public_key,
                            message_signatures,
                            include_source_signature_data: false,
                            farmer_reward_address_override: None,
                        },
                        msg.id,
                    )?
                    .to_bytes(protocol_version)?
                    .into(),
                ))
                .await;
        } else {
            error!("Failed to find client in PeerMap");
        }
        Ok(())
    }
}
