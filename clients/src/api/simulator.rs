use crate::api::responses::{AutoFarmResp, EmptyResponse};
use crate::rpc::ChiaRpcError;
use async_trait::async_trait;
use dg_xch_core::blockchain::sized_bytes::Bytes32;

#[async_trait]
pub trait SimulatorAPI {
    async fn farm_blocks(
        &self,
        address: Bytes32,
        blocks: i64,
        transaction_block: bool,
    ) -> Result<EmptyResponse, ChiaRpcError>;
    async fn set_auto_farming(&self, should_auto_farm: bool) -> Result<AutoFarmResp, ChiaRpcError>;
    async fn get_auto_farming(&self) -> Result<AutoFarmResp, ChiaRpcError>;
}
