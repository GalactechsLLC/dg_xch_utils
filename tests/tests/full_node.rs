use dg_xch_clients::api::full_node::FullnodeAPI;
// #[tokio::test]
// #[allow(clippy::too_many_lines)]
// pub async fn test_full_node_client() -> Result<(), Error> {
//     use dg_xch_clients::api::full_node::FullnodeAPI;
//     use dg_xch_clients::api::full_node::FullnodeExtAPI;
//     use dg_xch_clients::rpc::full_node::FullnodeClient;
//     use dg_xch_core::blockchain::sized_bytes::Bytes32;
//     use std::env;
//
//     let hostname = env::var("FULLNODE_HOST").unwrap_or_else(|_| String::from("localhost"));
//     let port = env::var("FULLNODE_PORT")
//         .map(|v| v.parse().unwrap_or(8555))
//         .unwrap_or(8555);
//     let client = FullnodeClient::new(&hostname, port, 120, None, &None);
//     let hinted_block = client.get_block_record_by_height(4_000_001).await?;
//     let add_and_removes_with_hints = client
//         .get_additions_and_removals_with_hints(&hinted_block.header_hash)
//         .await?;
//     let _ = client.get_all_mempool_items().await?;
//     let _ = client
//         .get_coin_records_by_puzzle_hashes_paginated(
//             &[Bytes32::from(
//                 "1c69feee1fb42ffa6c60fcc222c3aa8fb6cc719937a83f5aa068dc7045e0a633",
//             )],
//             None,
//             None,
//             None,
//             50,
//             None,
//         )
//         .await?;
//     let state = client.get_blockchain_state().await?;
//     assert!(state.space > 0);
//     let first_block = client.get_block_record_by_height(1).await?;
//     assert_ne!(Bytes32::default(), first_block.header_hash);
//     let full_first_block = client.get_block(&first_block.header_hash).await?;
//     assert_eq!(
//         Bytes32::from("0xd780d22c7a87c9e01d98b49a0910f6701c3b95015741316b3fda042e5d7b81d2"),
//         full_first_block.foliage.prev_block_hash
//     );
//     let blocks = client.get_blocks(0, 5, true, true).await?;
//     assert_eq!(blocks.len(), 5);
//     let blocks2 = client.get_all_blocks(0, 5).await?;
//     assert_eq!(blocks, blocks2);
//     let firet_block_record = client.get_block_record(&first_block.header_hash).await?;
//     assert_eq!(first_block, firet_block_record);
//     let block_records = client.get_block_records(0, 5).await?;
//     assert_eq!(block_records.len(), 5);
//     let _ = client.get_unfinished_block_headers().await?;
//     let height = client.get_network_space_by_height(1000, 5000).await?; //this also tests get_network_space and get_block_record_by_height
//     assert_eq!(140_670_610_131_864_768, height);
//     let coins = client
//         .get_coin_records_by_hints(
//             &[
//                 "6240759ea957a932e32b7ff28611c2dc70d087520323fabfe6b5ec7b8c060097",
//                 "f896b9ad8e6d644e6f6877d06de12473d5e370ff93ddf2066a57811c54fc40bd",
//                 "a9e173a3600db00fda09ad45934c02a8230b5b0478f74221faa8d52cff4f09ce",
//                 "a436040e0180cb8885f8fa14d61079dc87f9a5d376a1757d137edaf309f65b3f",
//                 "8925e95611307d918fb76c3a25d4115ed2a585a4bae5d5f69954a2ec0dc463f7",
//                 "d7efb93500033639863b3c2248b42c0d61e9c3157cea474b9ec877aa099cdc62",
//                 "d82d511a98d20b146c462a1dc5ad29b1d59018c742a514fbf7d73ea2670a1b37",
//                 "74ecebcf4e3c38e8ec337bbfa854b62ce40027c3073e1b5b6100d90fcea6a92d",
//                 "43bc11f49edef45736434879016574fb32c750be5e051b00109d379aff57956c",
//                 "cd24cd73b67ea2577c58c3d0093cc8816d965aa637945ab3d5446d8b3122582a",
//                 "95d09badc05dd138cf56bd2d8cf653e82c731da57478fdd56c050f9e4ea81d0d",
//                 "32a55d46b08d1e54a14fdc8dbb01577605512a8b6a52173d95e5af3213f656c7",
//                 "8cc604c6a1aec251d7b69fbf6edd3f011df4d2a5dcf6ec70621b4484e59cdef5",
//                 "9a53f63a29e1f1f5f33c52bd4608060067c4c6d4ec2d7857adeb439596decf22",
//                 "5bcafbd5b44da8891e5a3ecbc6162e2341f4afd565626d1634b15be465795129",
//                 "9261ce337f122a7a305b0ef1b8594916f7c8e40429576846b863a0099558234e",
//                 "50e38db1b36d55050a19aca3928573431f8c2dedbb8d956b6894fbd9e55989d9",
//                 "db3795f2bc6069a5c1ea737d44005ab011c190753b7a3d40502bdc9dc796a168",
//                 "7d1b208b571cadf5cd853f139191110aef2c31ba01214b0df409101c7522af4d",
//             ]
//             .map(Bytes32::from),
//             Some(true),
//             Some(3_068_715),
//             Some(3_468_715),
//         )
//         .await?;
//     for coin in coins {
//         if !coin.spent {
//             let _parent_spend = get_parent_spend(&client, &coin).await?;
//         }
//     }
//     let coin_records_by_hints = client
//         .get_coin_records_by_hints_paginated(
//             &add_and_removes_with_hints
//                 .0
//                 .iter()
//                 .fold(std::collections::HashSet::new(), |mut v, d| {
//                     if let Some(h) = d.hint {
//                         v.insert(h);
//                     }
//                     v
//                 })
//                 .iter()
//                 .copied()
//                 .collect::<Vec<Bytes32>>(),
//             Some(true),
//             Some(4_000_000),
//             Some(4_000_010),
//             50,
//             None,
//         )
//         .await?;
//     println!("{coin_records_by_hints:?}");
//     Ok(())
// }

pub async fn get_parent_spend(
    client: &FullnodeClient,
    coin_record: &CoinRecord,
) -> Result<CoinSpend, Error> {
    client
        .get_puzzle_and_solution(
            &coin_record.coin.parent_coin_info,
            coin_record.confirmed_block_index,
        )
        .await
}

// #[tokio::test]
// pub async fn test_farmer_ws_client() {
//     use dg_xch_clients::websocket::farmer::FarmerClient;
//     use dg_xch_clients::websocket::WsClientConfig;
//     use dg_xch_core::protocols::farmer::FarmerSharedState;
//     use dg_xch_core::protocols::{ChiaMessageFilter, ChiaMessageHandler, ProtocolMessageTypes};
//     use futures_util::future::try_join_all;
//     use log::{error, info};
//     use simple_logger::SimpleLogger;
//     use std::env;
//     use std::sync::atomic::{AtomicBool, Ordering};
//     use std::sync::Arc;
//     use std::time::Duration;
//     use uuid::Uuid;
//
//     SimpleLogger::new().env().init().unwrap_or_default();
//     let mut clients = vec![];
//     let simulate_count = 10;
//     let host = env::var("FULLNODE_HOST").unwrap_or_else(|_| String::from("localhost"));
//     let port = env::var("FULLNODE_PORT")
//         .map(|v| v.parse().unwrap_or(8444u16))
//         .unwrap_or(8444u16);
//     let network_id = "mainnet";
//     let run_handle = Arc::new(AtomicBool::new(true));
//     let config = Arc::new(WsClientConfig {
//         host: host.clone(),
//         port,
//         network_id: network_id.to_string(),
//         ssl_info: None,
//         software_version: None,
//         protocol_version: ChiaProtocolVersion::default(),
//         additional_headers: None,
//     });
//     let shared_state = Arc::new(FarmerSharedState::<()> {
//         ..Default::default()
//     });
//     for _ in 0..simulate_count {
//         let client_handle = run_handle.clone();
//         let config = config.clone();
//         let shared_state = shared_state.clone();
//         let thread = tokio::spawn(async move {
//             let client_handle = client_handle.clone();
//             'retry: loop {
//                 match FarmerClient::new(config.clone(), shared_state.clone(), client_handle.clone())
//                     .await
//                 {
//                     Ok(farmer_client) => {
//                         {
//                             let signage_handle_id = Uuid::new_v4();
//                             let signage_handle = Arc::new(NewSignagePointEcho {
//                                 id: signage_handle_id,
//                             });
//                             farmer_client
//                                 .client
//                                 .connection
//                                 .read()
//                                 .await
//                                 .subscribe(
//                                     signage_handle_id,
//                                     ChiaMessageHandler::new(
//                                         Arc::new(ChiaMessageFilter {
//                                             msg_type: Some(ProtocolMessageTypes::NewSignagePoint),
//                                             id: None,
//                                         }),
//                                         signage_handle,
//                                     ),
//                                 )
//                                 .await;
//                         }
//                         loop {
//                             if farmer_client.is_closed() {
//                                 if !client_handle.load(Ordering::Relaxed) {
//                                     info!("Farmer Stopping from global run");
//                                     break 'retry;
//                                 }
//                                 info!("Farmer Client Closed, Reconnecting");
//                                 break;
//                             }
//                             tokio::time::sleep(Duration::from_secs(1)).await;
//                         }
//                     }
//                     Err(e) => {
//                         error!("Farmer Client Error: {:?}", e);
//                         tokio::time::sleep(Duration::from_secs(1)).await;
//                     }
//                 }
//                 if !client_handle.load(Ordering::Relaxed) {
//                     info!("Farmer Stopping from global run");
//                     break 'retry;
//                 }
//             }
//         });
//         tokio::time::sleep(Duration::from_millis(5)).await;
//         clients.push(thread);
//     }
//     let _ = try_join_all(clients).await;
// }

use async_trait::async_trait;
use dg_xch_clients::rpc::full_node::FullnodeClient;
use dg_xch_core::blockchain::coin_record::CoinRecord;
use dg_xch_core::blockchain::coin_spend::CoinSpend;
use dg_xch_core::protocols::farmer::NewSignagePoint;
use std::io::{Cursor, Error};
use std::sync::Arc;
use uuid::Uuid;

use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::protocols::{ChiaMessage, MessageHandler, PeerMap};
use dg_xch_serialize::ChiaProtocolVersion;

pub struct NewSignagePointEcho {
    pub id: Uuid,
}
#[async_trait]
impl MessageHandler for NewSignagePointEcho {
    async fn handle(
        &self,
        msg: Arc<ChiaMessage>,
        _: Arc<Bytes32>,
        _: PeerMap,
    ) -> Result<(), Error> {
        use dg_xch_serialize::ChiaSerialize;
        let mut cursor = Cursor::new(&msg.data);
        let sp = NewSignagePoint::from_bytes(&mut cursor, ChiaProtocolVersion::default())?;
        println!(
            "New Signage Point({}): {:?}",
            sp.signage_point_index, sp.challenge_hash
        );
        Ok(())
    }
}

// #[tokio::test]
// async fn test_extended_functions() {
//     let fnc = FullnodeClient::new("localhost", 8555, 10, None, &None);
//     let _by_puz = fnc
//         .get_coin_records_by_puzzle_hashes_paginated(
//             &[Bytes32::from(
//                 "1c69feee1fb42ffa6c60fcc222c3aa8fb6cc719937a83f5aa068dc7045e0a633",
//             )],
//             None,
//             None,
//             None,
//             10,
//             None,
//         )
//         .await
//         .unwrap();
//     fnc.get_blockchain_state().await.unwrap();
//     let (additions, _removals) = fnc
//         .get_additions_and_removals_with_hints(&Bytes32::from(
//             "0x499c034d9761ab329c0ce293006a55628bb9ea62cae3836901628f6a1afb0031",
//         ))
//         .await
//         .unwrap();
//     let mut hints = vec![];
//     let mut puz_hashes = vec![];
//     let mut coin_ids = vec![];
//     for add in additions {
//         if let Some(hint) = add.hint {
//             hints.push(hint);
//             puz_hashes.push(add.coin.puzzle_hash);
//             coin_ids.push(add.coin.coin_id());
//         }
//     }
//     let coin_hints = fnc.get_hints_by_coin_ids(&coin_ids).await.unwrap();
//     for h in &hints {
//         assert!(coin_hints.values().any(|v| v == h));
//     }
//     let (coin_records, _last_id, _total_coin_count) = fnc
//         .get_coin_records_by_hints_paginated(
//             &hints,
//             Some(true),
//             Some(4_540_000),
//             Some(4_542_825),
//             10,
//             None,
//         )
//         .await
//         .unwrap();
//     assert!(!coin_records.is_empty());
//     let by_puz = fnc
//         .get_coin_records_by_puzzle_hashes_paginated(
//             &puz_hashes,
//             Some(true),
//             Some(4_540_000),
//             Some(4_542_825),
//             2,
//             None,
//         )
//         .await
//         .unwrap();
//     assert!(!by_puz.0.is_empty());
//     assert!(by_puz.0.iter().all(|v| coin_records.contains(v)));
//     assert!(!fnc
//         .get_puzzles_and_solutions_by_names(&coin_ids, Some(true), Some(4_540_000), Some(4_542_825))
//         .await
//         .unwrap()
//         .is_empty());
// }
