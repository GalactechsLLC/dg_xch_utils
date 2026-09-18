use super::*;

mod farmer;
mod peer;
mod wallet;

#[async_trait]
impl<S: BlockStore + CoinStore + Send + Sync + 'static> FullNodeApi for StoreApi<S> {
    async fn block_by_height(&self, height: u32) -> Option<Box<FullBlock>> {
        StoreApi::block_by_height(self, height).await
    }

    fn max_block_count_per_requests(&self) -> u32 {
        StoreApi::max_block_count_per_requests(self)
    }

    fn max_subscriptions(&self, peer: &Bytes32, host: Option<IpAddr>) -> u32 {
        StoreApi::max_subscriptions(self, peer, host)
    }

    fn max_subscribe_response_items(&self, peer: &Bytes32, host: Option<IpAddr>) -> u32 {
        StoreApi::max_subscribe_response_items(self, peer, host)
    }

    fn accept_inbound_timelord(&self, host: Option<IpAddr>) -> bool {
        StoreApi::accept_inbound_timelord(self, host)
    }

    async fn gossip_peers(&self) -> Vec<TimestampedPeerInfo> {
        StoreApi::gossip_peers(self).await
    }

    async fn on_new_peak(&self, peer: Bytes32, peak: NewPeak) {
        StoreApi::on_new_peak(self, peer, peak).await
    }

    async fn transaction(&self, id: Bytes32) -> Option<SpendBundle> {
        StoreApi::transaction(self, id).await
    }

    async fn on_new_transaction(
        &self,
        _peer: Bytes32,
        tx: NewTransaction,
    ) -> TransactionAnnounceAction {
        StoreApi::on_new_transaction(self, _peer, tx).await
    }

    async fn on_respond_transaction(&self, peer: Bytes32, host: Option<IpAddr>, tx: SpendBundle) {
        StoreApi::on_respond_transaction(self, peer, host, tx).await
    }

    async fn on_new_signage_point_or_eos(
        &self,
        _peer: Bytes32,
        ann: NewSignagePointOrEndOfSubSlot,
    ) -> Option<RequestSignagePointOrEndOfSubSlot> {
        StoreApi::on_new_signage_point_or_eos(self, _peer, ann).await
    }

    async fn signage_point_or_eos(
        &self,
        req: RequestSignagePointOrEndOfSubSlot,
    ) -> Option<SignagePointResponse> {
        StoreApi::signage_point_or_eos(self, req).await
    }

    async fn on_respond_signage_point(&self, _peer: Bytes32, sp: RespondSignagePoint) {
        StoreApi::on_respond_signage_point(self, _peer, sp).await
    }

    async fn on_respond_end_of_sub_slot(&self, _peer: Bytes32, eos: RespondEndOfSubSlot) {
        StoreApi::on_respond_end_of_sub_slot(self, _peer, eos).await
    }

    async fn on_new_unfinished_block(
        &self,
        _peer: Bytes32,
        ann: NewUnfinishedBlock,
    ) -> Option<RequestUnfinishedBlock> {
        StoreApi::on_new_unfinished_block(self, _peer, ann).await
    }

    async fn on_new_unfinished_block2(
        &self,
        _peer: Bytes32,
        ann: NewUnfinishedBlock2,
    ) -> Option<RequestUnfinishedBlock2> {
        StoreApi::on_new_unfinished_block2(self, _peer, ann).await
    }

    async fn unfinished_block(&self, reward_hash: Bytes32) -> Option<Box<UnfinishedBlock>> {
        StoreApi::unfinished_block(self, reward_hash).await
    }

    async fn unfinished_block2(
        &self,
        reward_hash: Bytes32,
        foliage_hash: Option<Bytes32>,
    ) -> Option<Box<UnfinishedBlock>> {
        StoreApi::unfinished_block2(self, reward_hash, foliage_hash).await
    }

    async fn on_respond_unfinished_block(&self, block: Box<UnfinishedBlock>) {
        StoreApi::on_respond_unfinished_block(self, block).await
    }

    async fn mempool_items(&self, filter: Vec<u8>) -> Vec<NewTransaction> {
        StoreApi::mempool_items(self, filter).await
    }

    async fn on_request_proof_of_weight(
        &self,
        peer: Bytes32,
        req: RequestProofOfWeight,
        id: Option<u16>,
        peers: PeerMap,
    ) {
        StoreApi::on_request_proof_of_weight(self, peer, req, id, peers).await
    }

    async fn compact_vdf(&self, req: RequestCompactVDF) -> Option<RespondCompactVDF> {
        StoreApi::compact_vdf(self, req).await
    }

    async fn on_new_compact_vdf(
        &self,
        _peer: Bytes32,
        ann: NewCompactVDF,
    ) -> Option<RequestCompactVDF> {
        StoreApi::on_new_compact_vdf(self, _peer, ann).await
    }

    async fn on_respond_compact_vdf(&self, _peer: Bytes32, resp: RespondCompactVDF) {
        StoreApi::on_respond_compact_vdf(self, _peer, resp).await
    }

    async fn on_respond_compact_proof_of_time(
        &self,
        _peer: Bytes32,
        resp: RespondCompactProofOfTime,
    ) {
        StoreApi::on_respond_compact_proof_of_time(self, _peer, resp).await
    }

    async fn on_declare_proof_of_space(
        &self,
        peer: Bytes32,
        declare: DeclareProofOfSpace,
    ) -> Option<RequestSignedValues> {
        StoreApi::on_declare_proof_of_space(self, peer, declare).await
    }

    async fn on_signed_values(&self, peer: Bytes32, signed: SignedValues) {
        StoreApi::on_signed_values(self, peer, signed).await
    }

    async fn on_new_infusion_point_vdf(&self, peer: Bytes32, req: NewInfusionPointVDF) {
        StoreApi::on_new_infusion_point_vdf(self, peer, req).await
    }

    async fn on_new_signage_point_vdf(&self, peer: Bytes32, req: NewSignagePointVDF) {
        StoreApi::on_new_signage_point_vdf(self, peer, req).await
    }

    async fn on_new_end_of_sub_slot_vdf(&self, peer: Bytes32, req: NewEndOfSubSlotVDF) {
        StoreApi::on_new_end_of_sub_slot_vdf(self, peer, req).await
    }

    async fn puzzle_solution(
        &self,
        coin_name: Bytes32,
        height: u32,
    ) -> Option<PuzzleSolutionResponse> {
        StoreApi::puzzle_solution(self, coin_name, height).await
    }

    async fn send_transaction(&self, _peer: Bytes32, tx: SendTransaction) -> TransactionAck {
        StoreApi::send_transaction(self, _peer, tx).await
    }

    async fn block_header(&self, height: u32) -> BlockHeaderReply {
        StoreApi::block_header(self, height).await
    }

    async fn header_blocks(&self, start_height: u32, end_height: u32) -> HeaderBlocksReply {
        StoreApi::header_blocks(self, start_height, end_height).await
    }

    async fn block_headers(
        &self,
        start_height: u32,
        end_height: u32,
        return_filter: bool,
    ) -> BlockHeadersReply {
        StoreApi::block_headers(self, start_height, end_height, return_filter).await
    }

    #[cfg(feature = "coin-index")]
    async fn additions(&self, req: RequestAdditions) -> AdditionsReply {
        StoreApi::additions(self, req).await
    }

    #[cfg(feature = "coin-index")]
    async fn removals(&self, req: RequestRemovals) -> RemovalsReply {
        StoreApi::removals(self, req).await
    }

    async fn children(&self, coin_name: Bytes32) -> Vec<CoinState> {
        StoreApi::children(self, coin_name).await
    }

    async fn register_for_ph_updates(
        &self,
        peer: Bytes32,
        host: Option<IpAddr>,
        req: RegisterForPhUpdates,
    ) -> PhRegistration {
        StoreApi::register_for_ph_updates(self, peer, host, req).await
    }

    async fn register_for_coin_updates(
        &self,
        peer: Bytes32,
        host: Option<IpAddr>,
        req: RegisterForCoinUpdates,
    ) -> CoinRegistration {
        StoreApi::register_for_coin_updates(self, peer, host, req).await
    }

    #[cfg(feature = "coin-index")]
    async fn puzzle_state(
        &self,
        peer: Bytes32,
        host: Option<IpAddr>,
        req: RequestPuzzleState,
    ) -> PuzzleStateReply {
        StoreApi::puzzle_state(self, peer, host, req).await
    }

    async fn coin_state(
        &self,
        peer: Bytes32,
        host: Option<IpAddr>,
        req: RequestCoinState,
    ) -> CoinStateReply {
        StoreApi::coin_state(self, peer, host, req).await
    }

    async fn fee_estimates(&self, req: RequestFeeEstimates) -> FeeEstimateGroup {
        StoreApi::fee_estimates(self, req).await
    }

    async fn remove_puzzle_subscriptions(
        &self,
        peer: Bytes32,
        puzzle_hashes: Option<Vec<Bytes32>>,
    ) -> Vec<Bytes32> {
        StoreApi::remove_puzzle_subscriptions(self, peer, puzzle_hashes).await
    }

    async fn remove_coin_subscriptions(
        &self,
        peer: Bytes32,
        coin_ids: Option<Vec<Bytes32>>,
    ) -> Vec<Bytes32> {
        StoreApi::remove_coin_subscriptions(self, peer, coin_ids).await
    }

    async fn wallet_peak(&self) -> Option<NewPeakWallet> {
        StoreApi::wallet_peak(self).await
    }

    async fn full_node_peak(&self) -> Option<NewPeak> {
        StoreApi::full_node_peak(self).await
    }

    async fn timelord_peak(&self) -> Option<Box<NewPeakTimelord>> {
        StoreApi::timelord_peak(self).await
    }

    async fn mempool_sync_filter(&self) -> Option<Vec<u8>> {
        StoreApi::mempool_sync_filter(self).await
    }
}
