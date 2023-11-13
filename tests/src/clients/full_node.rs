#[tokio::test]
pub async fn test_full_node_client() {
    use dg_xch_clients::api::full_node::FullnodeAPI;
    use dg_xch_clients::rpc::full_node::FullnodeClient;
    use dg_xch_core::blockchain::sized_bytes::Bytes32;
    use std::env;

    let hostname = env::var("FULLNODE_HOST").unwrap_or_else(|_| String::from("localhost"));
    let port = env::var("FULLNODE_PORT")
        .map(|v| v.parse().unwrap_or(8555))
        .unwrap_or(8555);
    let ssl_path = env::var("FULLNODE_SSL_PATH").ok();
    let client = FullnodeClient::new(&hostname, port, ssl_path, &None);
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
    let metrics = client.get_block_count_metrics().await.unwrap();
    assert!(metrics.compact_blocks > 0);
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
    let add_and_removes = client
        .get_additions_and_removals(&first_block.header_hash)
        .await
        .unwrap();
    println!("{:?}", add_and_removes);
}

#[tokio::test]
pub async fn test_farmer_ws_client() {
    use dg_xch_clients::protocols::ProtocolMessageTypes;
    use dg_xch_clients::websocket::farmer::FarmerClient;
    use dg_xch_clients::websocket::{ChiaMessageFilter, ChiaMessageHandler, Websocket};
    use futures_util::future::try_join_all;
    use log::{error, info};
    use simple_logger::SimpleLogger;
    use std::collections::HashMap;
    use std::env;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use uuid::Uuid;

    SimpleLogger::new().env().init().unwrap_or_default();
    let mut clients = vec![];
    let simulate_count = 1;
    let host = env::var("FULLNODE_HOST").unwrap_or_else(|_| String::from("localhost"));
    let port = env::var("FULLNODE_PORT").unwrap_or_else(|_| String::from("8444"));
    let network_id = "mainnet";
    let run_handle = Arc::new(AtomicBool::new(true));
    let mut headers = HashMap::new();
    headers.insert(String::from("X-iriga-client"), String::from("evergreen"));
    headers.insert(
        String::from("X-evg-lite-farmer-version"),
        "benchmarker-v1".to_string(),
    );
    headers.insert(
        String::from("X-evg-dg-xch-pos-version"),
        dg_xch_pos::version(),
    );
    headers.insert(String::from("X-evg-device-id"), Uuid::new_v4().to_string());
    for _ in 0..simulate_count {
        let client_handle = run_handle.clone();
        let additional_headers = Some(headers.clone());
        let thread = tokio::spawn(async move {
            let additional_headers = additional_headers;
            let client_handle = client_handle.clone();
            'retry: loop {
                match FarmerClient::new_ssl_generate(
                    host,
                    port,
                    network_id,
                    &additional_headers,
                    client_handle.clone(),
                )
                .await
                {
                    Ok(farmer_client) => {
                        {
                            let client = &mut farmer_client.client.lock().await;
                            client.clear().await;
                            let signage_handle_id = Uuid::new_v4();
                            let signage_handle = Arc::new(NewSignagePointEcho {
                                id: signage_handle_id,
                            });
                            client
                                .subscribe(
                                    signage_handle_id,
                                    ChiaMessageHandler::new(
                                        ChiaMessageFilter {
                                            msg_type: Some(ProtocolMessageTypes::NewSignagePoint),
                                            id: None,
                                        },
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
use dg_xch_clients::protocols::farmer::NewSignagePoint;
use std::io::{Cursor, Error};
use std::sync::Arc;
use uuid::Uuid;

use dg_xch_clients::websocket::{ChiaMessage, MessageHandler};
pub struct NewSignagePointEcho {
    pub id: Uuid,
}
#[async_trait]
impl MessageHandler for NewSignagePointEcho {
    async fn handle(&self, msg: Arc<ChiaMessage>) -> Result<(), Error> {
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
