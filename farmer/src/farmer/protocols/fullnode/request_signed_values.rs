use crate::PROTOCOL_VERSION;
use crate::farmer::FarmerSharedState;
use crate::farmer::config::Config;
use crate::harvesters::{Harvester, SignatureHandler};
use async_trait::async_trait;
use dg_xch_clients::api::pool::PoolClient;
use dg_xch_clients::websocket::farmer::FarmerClient;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_core::protocols::farmer::RequestSignedValues;
use dg_xch_core::protocols::harvester::{
    RequestSignatures, SignatureRequestSourceData, SigningDataKind,
};
use dg_xch_core::protocols::{ChiaMessage, MessageHandler, PeerMap};
use dg_xch_serialize::ChiaSerialize;
use log::{debug, error};
use std::io::{Cursor, Error, ErrorKind};
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

pub struct RequestSignedValuesHandle<P, S, T, H, C>
where
    P: PoolClient + Sized + Sync + Send + 'static,
    S: SignatureHandler<T, H, C> + Sync + Send + 'static,
    T: Sync + Send + 'static,
    H: Harvester<T, H, C> + Sync + Send + 'static,
    C: Sync + Send + Clone + 'static,
{
    pub id: Uuid,
    pub shared_state: Arc<FarmerSharedState<T>>,
    pub pool_client: Arc<P>,
    pub harvester: Arc<H>,
    pub constants: ConsensusConstants,
    pub config: Arc<RwLock<Config<C>>>,
    pub client: Arc<RwLock<Option<FarmerClient<T>>>>,
    pub signature_handle: Arc<S>,
}
#[async_trait]
impl<P, S, T, H, C> MessageHandler for RequestSignedValuesHandle<P, S, T, H, C>
where
    P: PoolClient + Default + Sized + Sync + Send + 'static,
    S: SignatureHandler<T, H, C> + Sync + Send + 'static,
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
        debug!("RequestSignedValues Message. Starting Deserialization.");
        let mut cursor = Cursor::new(msg.data.as_slice());
        let request = RequestSignedValues::from_bytes(&mut cursor, PROTOCOL_VERSION)?;
        debug!("RequestSignedValues Message. Finished Deserialization.");
        if let Some(identifier) = self
            .shared_state
            .quality_to_identifiers
            .read()
            .await
            .get(&request.quality_string)
        {
            debug!("Found Identifier for {}", request.quality_string);
            let mut foliage_block_data = None;
            let mut foliage_transaction_block = None;
            if let Some(data) = request.foliage_block_data {
                foliage_block_data = Some(SignatureRequestSourceData {
                    kind: SigningDataKind::FoliageBlockData,
                    data: data.to_bytes(PROTOCOL_VERSION)?,
                });
            }
            if let Some(data) = request.foliage_transaction_block_data {
                foliage_transaction_block = Some(SignatureRequestSourceData {
                    kind: SigningDataKind::FoliageTransactionBlock,
                    data: data.to_bytes(PROTOCOL_VERSION)?,
                });
            }
            let request = RequestSignatures {
                plot_identifier: identifier.plot_identifier.clone(),
                challenge_hash: identifier.challenge_hash,
                sp_hash: identifier.sp_hash,
                messages: vec![
                    request.foliage_block_data_hash,
                    request.foliage_transaction_block_hash,
                ],
                message_data: Some(vec![foliage_block_data, foliage_transaction_block]),
                rc_block_unfinished: request.rc_block_unfinished,
            };
            let harvester = self.harvester.clone();
            let shared_state = self.shared_state.clone();
            let config = self.config.clone();
            let client = self.client.clone();
            tokio::spawn(async move {
                let handle = S::load(shared_state, config, harvester.clone(), client).await?;
                if let Err(e) = harvester.request_signatures(request, handle).await {
                    error!("Error Requesting Signature: {e}");
                }
                Ok::<(), Error>(())
            });
            Ok(())
        } else {
            error!("Do not have quality {}", request.quality_string);
            Err(Error::new(
                ErrorKind::NotFound,
                format!("Do not have quality {}", request.quality_string),
            ))
        }
    }
}
