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
    let ssl_path = env::var("FULLNODE_SSL_PATH")
        .map(Some)
        .unwrap_or(Some(String::from("/home/chia/.chia/mainnet/config/ssl")));
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
