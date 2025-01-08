// #[tokio::test]
// pub async fn test_simulator() {
//     use dg_xch_cli_lib::simulator::Simulator;
//     use dg_xch_cli_lib::wallets::{Wallet, WalletStore};
//     use dg_xch_clients::api::full_node::FullnodeAPI;
//
//     use log::{info, LevelFilter};
//     use simple_logger::SimpleLogger;
//     use std::env;
//     SimpleLogger::new()
//         .with_level(LevelFilter::Debug)
//         .init()
//         .unwrap();
//     let hostname = env::var("SIMULATOR_HOSTNAME").unwrap_or("localhost".to_string());
//     let port = env::var("SIMULATOR_PORT")
//         .map(|s| s.parse().unwrap())
//         .unwrap_or(5000u16);
//     let simulator = Simulator::new(&hostname, port, 30, &None, None);
//     let state_1 = simulator.client().get_blockchain_state().await.unwrap();
//     info!("{:#?}", state_1);
//     simulator.next_blocks(1, false).await.unwrap();
//     let state_2 = simulator.client().get_blockchain_state().await.unwrap();
//     info!("{:#?}", state_2);
//     //Test we moved the simulator forward
//     assert!(state_1.peak.unwrap().height < state_2.peak.unwrap().height);
//     let bob = simulator.new_user("bob", None).unwrap();
//     let alice = simulator.new_user("alice", None).unwrap();
//     //Bob should start with no XCH
//     assert_eq!(
//         bob.wallet
//             .wallet_store()
//             .lock()
//             .await
//             .get_spendable_balance()
//             .await,
//         0
//     );
//     //Bob Farms 2 blocks (4 coins)
//     bob.farm_coins(2).await.unwrap();
//     //Bob Sends 1_000_000 mojos to alice
//     bob.send_xch(1_000_000, &alice).await.unwrap();
//     //Assert that Alice now has 1_000_000 and Bob has 3_999_999_000_000
//     assert_eq!(
//         alice
//             .wallet
//             .wallet_store()
//             .lock()
//             .await
//             .get_spendable_balance()
//             .await,
//         1_000_000
//     );
//     assert_eq!(
//         bob.wallet
//             .wallet_store()
//             .lock()
//             .await
//             .get_spendable_balance()
//             .await,
//         3_999_999_000_000
//     );
// }
