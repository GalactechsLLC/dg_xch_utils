use crate::api::responses::{AutoFarmResp, EmptyResponse};
use async_trait::async_trait;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use std::io::Error;

#[async_trait]
pub trait SimulatorAPI {
    async fn farm_blocks(
        &self,
        address: Bytes32,
        blocks: i64,
        transaction_block: bool,
    ) -> Result<EmptyResponse, Error>;
    async fn set_auto_farming(&self, should_auto_farm: bool) -> Result<AutoFarmResp, Error>;
    async fn get_auto_farming(&self) -> Result<AutoFarmResp, Error>;
}
