pub mod druid_garden;
pub mod pos2;
use crate::farmer::config::Config;
use async_trait::async_trait;
use dg_xch_clients::websocket::farmer::FarmerClient;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48};
use dg_xch_core::protocols::farmer::FarmerSharedState;
use dg_xch_core::protocols::harvester::{
    NewProofOfSpace, NewSignagePointHarvester, RequestSignatures, RespondSignatures,
};
use std::io::Error;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;

#[async_trait]
pub trait SignatureHandler<T: 'static, H: 'static, C: Clone + 'static> {
    async fn load(
        shared_state: Arc<FarmerSharedState<T>>,
        config: Arc<RwLock<Config<C>>>,
        harvester: Arc<H>,
        client: Arc<RwLock<Option<FarmerClient<T>>>>,
    ) -> Result<Arc<Self>, Error>;
    async fn handle_signature(&self, respond_signatures: RespondSignatures) -> Result<(), Error>;
}
#[async_trait]
impl<
    T: Sync + Send + 'static,
    H: Sync + Send + 'static,
    C: Sync + Send + Clone + 'static,
    S: SignatureHandler<T, H, C> + Send + Sync + 'static,
> SignatureHandler<T, H, C> for Arc<S>
{
    async fn load(
        shared_state: Arc<FarmerSharedState<T>>,
        config: Arc<RwLock<Config<C>>>,
        harvester: Arc<H>,
        client: Arc<RwLock<Option<FarmerClient<T>>>>,
    ) -> Result<Arc<Self>, Error> {
        <S as SignatureHandler<T, H, C>>::load(shared_state, config, harvester, client)
            .await
            .map(Arc::new)
    }
    async fn handle_signature(&self, respond_signatures: RespondSignatures) -> Result<(), Error> {
        self.as_ref().handle_signature(respond_signatures).await
    }
}

#[async_trait]
pub trait ProofHandler<T: 'static, H: 'static, C: Clone + 'static> {
    async fn load(
        shared_state: Arc<FarmerSharedState<T>>,
        config: Arc<RwLock<Config<C>>>,
        harvester: Arc<H>,
        client: Arc<RwLock<Option<FarmerClient<T>>>>,
    ) -> Result<Arc<Self>, Error>;
    async fn handle_proof(&self, new_pos: NewProofOfSpace) -> Result<(), Error>;
}
#[async_trait]
impl<
    T: Sync + Send + 'static,
    H: Sync + Send + 'static,
    C: Sync + Send + Clone + 'static,
    O: ProofHandler<T, H, C> + Send + Sync + 'static,
> ProofHandler<T, H, C> for Arc<O>
{
    async fn load(
        shared_state: Arc<FarmerSharedState<T>>,
        config: Arc<RwLock<Config<C>>>,
        harvester: Arc<H>,
        client: Arc<RwLock<Option<FarmerClient<T>>>>,
    ) -> Result<Arc<Self>, Error> {
        <O as ProofHandler<T, H, C>>::load(shared_state, config, harvester, client)
            .await
            .map(Arc::new)
    }
    async fn handle_proof(&self, new_pos: NewProofOfSpace) -> Result<(), Error> {
        self.as_ref().handle_proof(new_pos).await
    }
}

#[async_trait]
pub trait Harvester<T: 'static, H: 'static, C: Clone + 'static> {
    async fn load(
        shared_state: Arc<FarmerSharedState<T>>,
        config: Arc<RwLock<Config<C>>>,
    ) -> Result<Arc<Self>, Error>;
    async fn new_signage_point<O>(
        &self,
        signage_point: Arc<NewSignagePointHarvester>,
        proof_handle: O,
    ) -> Result<(), Error>
    where
        O: ProofHandler<T, H, C> + Sync + Send + 'static;
    async fn request_signatures<S>(
        &self,
        request_signatures: RequestSignatures,
        response_handle: S,
    ) -> Result<(), Error>
    where
        S: SignatureHandler<T, H, C> + Sync + Send + 'static;
    fn uuid(&self) -> Bytes32;
}

pub struct FarmingKeys {
    pub farmer_public_keys: Vec<Bytes48>,
    pub pool_public_keys: Vec<Bytes48>,
    pub pool_contract_hashes: Vec<Bytes32>,
}

async fn count_plots(
    path: &Path,
    count_total: &mut u64,
    size_total: &mut u64,
) -> Result<(), Error> {
    if !path.is_dir() {
        return Ok(());
    }
    let mut dir = tokio::fs::read_dir(path).await?;
    while let Ok(Some(e)) = dir.next_entry().await {
        if e.file_name().to_string_lossy().ends_with(".plot") {
            let file_size = e.metadata().await?.len();
            *size_total += file_size;
            *count_total += 1;
        }
    }
    Ok(())
}
