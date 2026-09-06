use super::*;
use dg_xch_core::clvm::program::Program;
use dg_xch_core::consensus::block_rewards::calculate_base_farmer_reward;
use dg_xch_core::consensus::coinbase::create_farmer_coin;
use dg_xch_stores::traits::{BlockStore, CoinStore};

async fn start_server(dir: &Path) -> SimulatorServer {
    let db = dir.join("sim.sqlite");
    let plots = PlotSet::setup(dir, 15, 12, 18, 2, false).expect("plots");
    SimulatorServer::start(
        &db,
        "127.0.0.1:0",
        "127.0.0.1:0",
        "simulator0",
        simulator_constants(),
        plots,
        Duration::from_millis(50),
    )
    .await
    .expect("server starts")
}

#[tokio::test]
async fn farm_block_funds_an_address_through_the_shared_store() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = start_server(dir.path()).await;

    // Genesis peak is live before anything else.
    let (_, peak) = server
        .node()
        .store
        .get_peak()
        .await
        .expect("peak")
        .expect("has peak");
    assert_eq!(peak, 0);

    // Farm reward blocks to a wallet's puzzle hash; the height-1 reward is claimed by height 2,
    // creating a spendable coin at that address, visible through the served store.
    let wallet_ph = Program::to(1_u8).tree_hash();
    server.farm_to(wallet_ph, 3).await.expect("farm to address");
    let genesis = simulator_constants().genesis_challenge;
    let funded = create_farmer_coin(1, wallet_ph, calculate_base_farmer_reward(1), genesis);
    assert!(
        server
            .node()
            .store
            .get_coin_record(&funded.name())
            .await
            .expect("store")
            .is_some_and(|r| !r.spent),
        "farm_block did not fund the address"
    );
    server.stop().await;
}

#[tokio::test]
async fn farming_past_a_sub_slot_keeps_the_node_running() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server = start_server(dir.path()).await;
    let wallet_ph = Program::to(1_u8).tree_hash();
    // Farm past one sub-slot's worth of signage points; the node must cross rather than stall.
    server
        .farm_to(wallet_ph, 70)
        .await
        .expect("farm across a sub-slot");
    let (_, height) = server
        .node()
        .store
        .get_peak()
        .await
        .expect("peak")
        .expect("has peak");
    assert_eq!(
        height, 70,
        "the node stalled instead of crossing a sub-slot"
    );
    let mut crossed = false;
    for h in 1..=height {
        if let Some(rec) = server
            .node()
            .store
            .get_block_record_by_height(h)
            .await
            .expect("store")
            && rec.first_in_sub_slot()
        {
            crossed = true;
            break;
        }
    }
    assert!(crossed, "no sub-slot crossing occurred");
    server.stop().await;
}
