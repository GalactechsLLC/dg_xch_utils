#[tokio::test]
pub async fn test_full_node_client() -> Result<(), Error> {
    use dg_xch_clients::api::full_node::FullnodeAPI;
    use dg_xch_clients::api::full_node::FullnodeExtAPI;
    use dg_xch_clients::rpc::full_node::FullnodeClient;
    use dg_xch_clients::ClientSSLConfig;
    use dg_xch_core::blockchain::sized_bytes::Bytes32;
    use std::env;

    let hostname = env::var("FULLNODE_HOST").unwrap_or_else(|_| String::from("localhost"));
    let port = env::var("FULLNODE_PORT")
        .map(|v| v.parse().unwrap_or(8555))
        .unwrap_or(8555);
    let ssl_path = env::var("FULLNODE_SSL_PATH").ok();
    let client = FullnodeClient::new(
        &hostname,
        port,
        120,
        ssl_path.map(|s| ClientSSLConfig {
            ssl_crt_path: format!("{}/{}", s, "full_node/private_farmer_node.crt"),
            ssl_key_path: format!("{}/{}", s, "full_node/private_farmer_node.key"),
            ssl_ca_crt_path: format!("{}/{}", s, "ca/private_ca.crt"),
        }),
        &None,
    );
    let items = client.get_all_mempool_items().await.unwrap();
    let by_puz = client
        .get_coin_records_by_puzzle_hashes_paginated(
            &[Bytes32::from(
                "1c69feee1fb42ffa6c60fcc222c3aa8fb6cc719937a83f5aa068dc7045e0a633",
            )],
            None,
            None,
            None,
            50,
            None,
        )
        .await
        .unwrap();
    let state = client.get_blockchain_state().await.unwrap();
    assert!(state.space > 0);
    let first_block = client.get_block_record_by_height(1).await.unwrap();
    assert_ne!(Bytes32::default(), first_block.header_hash);
    let full_first_block = client.get_block(&first_block.header_hash).await.unwrap();
    assert_eq!(
        Bytes32::from("0xd780d22c7a87c9e01d98b49a0910f6701c3b95015741316b3fda042e5d7b81d2"),
        full_first_block.foliage.prev_block_hash
    );
    let blocks = client.get_blocks(0, 5, true, true).await.unwrap();
    assert_eq!(blocks.len(), 5);
    let blocks2 = client.get_all_blocks(0, 5).await.unwrap();
    assert_eq!(blocks, blocks2);
    let firet_block_record = client
        .get_block_record(&first_block.header_hash)
        .await
        .unwrap();
    assert_eq!(first_block, firet_block_record);
    let block_records = client.get_block_records(0, 5).await.unwrap();
    assert_eq!(block_records.len(), 5);
    let _ = client.get_unfinished_block_headers().await.unwrap();
    let height = client
        .get_network_space_by_height(1000, 5000)
        .await
        .unwrap(); //this also tests get_network_space and get_block_record_by_height
    assert_eq!(140670610131864768, height);
    let _ = client
        .get_additions_and_removals(&first_block.header_hash)
        .await
        .unwrap();
    let hinted_block = client.get_block_record_by_height(4000001).await.unwrap();
    let add_and_removes_with_hints = client
        .get_additions_and_removals_with_hints(&hinted_block.header_hash)
        .await
        .unwrap();
    let coin_records_by_hints = client
        .get_coin_records_by_hints_paginated(
            &add_and_removes_with_hints
                .0
                .iter()
                .fold(std::collections::HashSet::new(), |mut v, d| {
                    if let Some(h) = d.hint {
                        v.insert(h);
                    }
                    v
                })
                .iter()
                .copied()
                .collect::<Vec<Bytes32>>(),
            Some(true),
            Some(4000000),
            Some(4000010),
            50,
            None,
        )
        .await
        .unwrap();
    println!("{:?}", coin_records_by_hints);
    Ok(())
}

#[tokio::test]
pub async fn test_farmer_ws_client() {
    use dg_xch_clients::websocket::farmer::FarmerClient;
    use dg_xch_clients::websocket::WsClientConfig;
    use dg_xch_core::protocols::farmer::FarmerSharedState;
    use dg_xch_core::protocols::{ChiaMessageFilter, ChiaMessageHandler, ProtocolMessageTypes};
    use futures_util::future::try_join_all;
    use log::{error, info};
    use simple_logger::SimpleLogger;
    use std::env;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use uuid::Uuid;

    SimpleLogger::new().env().init().unwrap_or_default();
    let mut clients = vec![];
    let simulate_count = 10;
    let host = env::var("FULLNODE_HOST").unwrap_or_else(|_| String::from("localhost"));
    let port = env::var("FULLNODE_PORT")
        .map(|v| v.parse().unwrap_or(8444u16))
        .unwrap_or(8444u16);
    let network_id = "mainnet";
    let run_handle = Arc::new(AtomicBool::new(true));
    let config = Arc::new(WsClientConfig {
        host: host.clone(),
        port,
        network_id: network_id.to_string(),
        ssl_info: None,
        software_version: None,
        additional_headers: None,
    });
    let shared_state = Arc::new(FarmerSharedState {
        ..Default::default()
    });
    for _ in 0..simulate_count {
        let client_handle = run_handle.clone();
        let config = config.clone();
        let shared_state = shared_state.clone();
        let thread = tokio::spawn(async move {
            let client_handle = client_handle.clone();
            'retry: loop {
                match FarmerClient::new(config.clone(), shared_state.clone(), client_handle.clone())
                    .await
                {
                    Ok(farmer_client) => {
                        {
                            let signage_handle_id = Uuid::new_v4();
                            let signage_handle = Arc::new(NewSignagePointEcho {
                                id: signage_handle_id,
                            });
                            farmer_client
                                .client
                                .connection
                                .lock()
                                .await
                                .subscribe(
                                    signage_handle_id,
                                    ChiaMessageHandler::new(
                                        Arc::new(ChiaMessageFilter {
                                            msg_type: Some(ProtocolMessageTypes::NewSignagePoint),
                                            id: None,
                                        }),
                                        signage_handle,
                                    ),
                                )
                                .await;
                        }
                        loop {
                            if farmer_client.is_closed() {
                                if !client_handle.load(Ordering::Relaxed) {
                                    info!("Farmer Stopping from global run");
                                    break 'retry;
                                } else {
                                    info!("Farmer Client Closed, Reconnecting");
                                    break;
                                }
                            }
                            tokio::time::sleep(Duration::from_secs(1)).await;
                        }
                    }
                    Err(e) => {
                        error!("Farmer Client Error: {:?}", e);
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                }
                if !client_handle.load(Ordering::Relaxed) {
                    info!("Farmer Stopping from global run");
                    break 'retry;
                }
            }
        });
        tokio::time::sleep(Duration::from_millis(5)).await;
        clients.push(thread);
    }
    let _ = try_join_all(clients).await;
}

use async_trait::async_trait;
use dg_xch_core::protocols::farmer::NewSignagePoint;
use std::io::{Cursor, Error};
use std::sync::Arc;
use uuid::Uuid;

use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::protocols::{ChiaMessage, MessageHandler, PeerMap};

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
        let sp = NewSignagePoint::from_bytes(&mut cursor)?;
        println!(
            "New Signage Point({}): {:?}",
            sp.signage_point_index, sp.challenge_hash
        );
        Ok(())
    }
}
