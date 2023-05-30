use crate::blockchain::sized_bytes::Bytes32;
use lazy_static::lazy_static;
use num_bigint::BigInt;
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct ConsensusConstants {
    pub slot_blocks_target: u32, //How many blocks to target per sub-slot
    pub min_blocks_per_challenge_block: u8, //How many blocks must be created per slot (to make challenge sb)
    //Max number of blocks that can be infused into a sub-slot.
    //Note: this must be less than sub_epoch_blocks/2, and > slot_blocks_target
    pub max_sub_slot_blocks: u32,
    pub num_sps_sub_slot: u32, //The number of signage points per sub-slot (including the 0th sp at the sub-slot start)

    pub sub_slot_iters_starting: u64, //The sub_slot_iters for the first epoch
    pub difficulty_constant_factor: u128, //Multiplied by the difficulty to get iterations
    pub difficulty_starting: u64,     //The difficulty for the first epoch
    //The maximum factor by which difficulty and sub_slot_iters can change per epoch
    pub difficulty_change_max_factor: u32,
    pub sub_epoch_blocks: u32, //The number of blocks per sub-epoch
    pub epoch_blocks: u32, //The number of blocks per sub-epoch, must be a multiple of sub_epoch_blocks

    pub significant_bits: BigInt, //The number of bits to look at in difficulty and min iters. The rest are zeroed
    pub discriminant_size_bits: BigInt, //Max is 1024 (based on ClassGroupElement int size)
    pub number_zero_bits_plot_filter: usize, //H(plot id + challenge hash + signage point) must start with these many zeroes
    pub min_plot_size: u8,
    pub max_plot_size: u8,
    pub sub_slot_time_target: BigInt, //The target number of seconds per sub-slot
    pub num_sp_intervals_extra: u64, //The difference between signage point and infusion point (plus required_iters)
    pub max_future_time: BigInt, //The next block can have a timestamp of at most these many seconds more
    pub number_of_timestamps: BigInt, //Than the average of the last number_of_timestamps blocks
    //Used as the initial cc rc challenges, as well as first block back pointers, and first SES back pointer
    //We override this value based on the chain being run (testnet0, testnet1, mainnet, etc)
    pub genesis_challenge: Bytes32,
    //Forks of chia should change this value to provide replay attack protection
    pub agg_sig_me_additional_data: Vec<u8>,
    pub genesis_pre_farm_pool_puzzle_hash: Bytes32, //The block at height must pay out to this pool puzzle hash
    pub genesis_pre_farm_farmer_puzzle_hash: Bytes32, //The block at height must pay out to this farmer puzzle hash
    pub max_vdf_witness_size: BigInt, //The maximum number of class group elements within an n-wesolowski proof
    //Size of mempool = 10x the size of block
    pub mempool_block_buffer: BigInt,
    //Max coin amount u(1 << 64). This allows coin amounts to fit in 64 bits. This is around 18M chia.
    pub max_coin_amount: BigInt,
    //Max block cost in clvm cost units
    pub max_block_cost_clvm: BigInt,
    //Cost per byte of generator program
    pub cost_per_byte: BigInt,

    pub weight_proof_threshold: u8,
    pub weight_proof_recent_blocks: u32,
    pub max_block_count_per_requests: u32,
    pub blocks_cache_size: u32,
    pub max_generator_size: u32,
    pub max_generator_ref_list_size: u32,
    pub pool_sub_slot_iters: u64,

    //This is NOT standard, but makes some things easier
    pub bech32_prefix: String,
    pub is_testnet: bool,
}
impl Default for ConsensusConstants {
    fn default() -> Self {
        MAINNET.clone()
    }
}
lazy_static! {
    pub static ref MAINNET: ConsensusConstants = ConsensusConstants {
        slot_blocks_target: 32,
        min_blocks_per_challenge_block: 16,
        max_sub_slot_blocks: 128,
        num_sps_sub_slot: 64,
        sub_slot_iters_starting: 2u64.pow(27),
        difficulty_constant_factor: 2u128.pow(67),
        difficulty_starting: 7,
        difficulty_change_max_factor: 3,
        sub_epoch_blocks: 384,
        epoch_blocks: 4608,
        significant_bits: BigInt::from(8),
        discriminant_size_bits: BigInt::from(1024),
        number_zero_bits_plot_filter: 9,
        min_plot_size: 32,
        max_plot_size: 50,
        sub_slot_time_target: BigInt::from(600),
        num_sp_intervals_extra: 3,
        max_future_time: BigInt::from(5 * 60),
        number_of_timestamps: BigInt::from(11),
        genesis_challenge: Bytes32::from(
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        ),
        agg_sig_me_additional_data: hex::decode(
            "ccd5bb71183532bff220ba46c268991a3ff07eb358e8255a65c30a2dce0e5fbb"
        )
        .expect("Failed to parse known good hex"),
        genesis_pre_farm_pool_puzzle_hash: Bytes32::from(
            "d23da14695a188ae5708dd152263c4db883eb27edeb936178d4d988b8f3ce5fc"
        ),
        genesis_pre_farm_farmer_puzzle_hash: Bytes32::from(
            "3d8765d3a597ec1d99663f6c9816d915b9f68613ac94009884c4addaefcce6af"
        ),
        max_vdf_witness_size: BigInt::from(64),
        mempool_block_buffer: BigInt::from(50),
        max_coin_amount: BigInt::from(u64::MAX),
        max_block_cost_clvm: BigInt::from(11000000000i64),
        cost_per_byte: BigInt::from(12000),
        weight_proof_threshold: 2,
        weight_proof_recent_blocks: 1000,
        max_block_count_per_requests: 32,
        blocks_cache_size: 4608 + (128 * 4),
        max_generator_size: 1000000,
        max_generator_ref_list_size: 512,
        pool_sub_slot_iters: 37600000000,
        bech32_prefix: String::from("xch"),
        is_testnet: false
    };
    pub static ref TESTNET_0: ConsensusConstants = ConsensusConstants {
        genesis_challenge: Bytes32::from(
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        ),
        genesis_pre_farm_farmer_puzzle_hash: Bytes32::from(
            "3d8765d3a597ec1d99663f6c9816d915b9f68613ac94009884c4addaefcce6af"
        ),
        genesis_pre_farm_pool_puzzle_hash: Bytes32::from(
            "d23da14695a188ae5708dd152263c4db883eb27edeb936178d4d988b8f3ce5fc"
        ),
        min_plot_size: 18,
        bech32_prefix: String::from("txch"),
        is_testnet: true,
        ..Default::default()
    };
    pub static ref TESTNET_2: ConsensusConstants = ConsensusConstants {
        difficulty_constant_factor: 10052721566054,
        genesis_challenge: Bytes32::from(
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        ),
        genesis_pre_farm_farmer_puzzle_hash: Bytes32::from(
            "3d8765d3a597ec1d99663f6c9816d915b9f68613ac94009884c4addaefcce6af"
        ),
        genesis_pre_farm_pool_puzzle_hash: Bytes32::from(
            "d23da14695a188ae5708dd152263c4db883eb27edeb936178d4d988b8f3ce5fc"
        ),
        min_plot_size: 18,
        bech32_prefix: String::from("txch"),
        is_testnet: true,
        ..Default::default()
    };
    pub static ref TESTNET_3: ConsensusConstants = ConsensusConstants {
        difficulty_constant_factor: 10052721566054,
        genesis_challenge: Bytes32::from(
            "ca7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015af"
        ),
        genesis_pre_farm_farmer_puzzle_hash: Bytes32::from(
            "3d8765d3a597ec1d99663f6c9816d915b9f68613ac94009884c4addaefcce6af"
        ),
        genesis_pre_farm_pool_puzzle_hash: Bytes32::from(
            "d23da14695a188ae5708dd152263c4db883eb27edeb936178d4d988b8f3ce5fc"
        ),
        mempool_block_buffer: BigInt::from(10),
        min_plot_size: 18,
        bech32_prefix: String::from("txch"),
        is_testnet: true,
        ..Default::default()
    };
    pub static ref TESTNET_4: ConsensusConstants = ConsensusConstants {
        difficulty_constant_factor: 10052721566054,
        difficulty_starting: 30,
        epoch_blocks: 768,
        genesis_challenge: Bytes32::from(
            "dd7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015af"
        ),
        genesis_pre_farm_farmer_puzzle_hash: Bytes32::from(
            "3d8765d3a597ec1d99663f6c9816d915b9f68613ac94009884c4addaefcce6af"
        ),
        genesis_pre_farm_pool_puzzle_hash: Bytes32::from(
            "d23da14695a188ae5708dd152263c4db883eb27edeb936178d4d988b8f3ce5fc"
        ),
        mempool_block_buffer: BigInt::from(10),
        min_plot_size: 18,
        bech32_prefix: String::from("txch"),
        is_testnet: true,
        ..Default::default()
    };
    pub static ref TESTNET_5: ConsensusConstants = ConsensusConstants {
        difficulty_constant_factor: 10052721566054,
        difficulty_starting: 30,
        epoch_blocks: 768,
        genesis_challenge: Bytes32::from(
            "ee7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015af"
        ),
        genesis_pre_farm_farmer_puzzle_hash: Bytes32::from(
            "3d8765d3a597ec1d99663f6c9816d915b9f68613ac94009884c4addaefcce6af"
        ),
        genesis_pre_farm_pool_puzzle_hash: Bytes32::from(
            "d23da14695a188ae5708dd152263c4db883eb27edeb936178d4d988b8f3ce5fc"
        ),
        mempool_block_buffer: BigInt::from(10),
        min_plot_size: 18,
        bech32_prefix: String::from("txch"),
        is_testnet: true,
        ..Default::default()
    };
    pub static ref TESTNET_7: ConsensusConstants = ConsensusConstants {
        difficulty_constant_factor: 10052721566054,
        difficulty_starting: 30,
        epoch_blocks: 768,
        genesis_challenge: Bytes32::from(
            "117816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015af"
        ),
        genesis_pre_farm_farmer_puzzle_hash: Bytes32::from(
            "3d8765d3a597ec1d99663f6c9816d915b9f68613ac94009884c4addaefcce6af"
        ),
        genesis_pre_farm_pool_puzzle_hash: Bytes32::from(
            "d23da14695a188ae5708dd152263c4db883eb27edeb936178d4d988b8f3ce5fc"
        ),
        mempool_block_buffer: BigInt::from(50),
        min_plot_size: 18,
        bech32_prefix: String::from("txch"),
        is_testnet: true,
        ..Default::default()
    };
    pub static ref TESTNET_10: ConsensusConstants = ConsensusConstants {
        agg_sig_me_additional_data: hex::decode(
            "ae83525ba8d1dd3f09b277de18ca3e43fc0af20d20c4b3e92ef2a48bd291ccb2"
        )
        .expect("Failed to parse known good hex"),
        difficulty_constant_factor: 10052721566054,
        difficulty_starting: 30,
        epoch_blocks: 768,
        genesis_challenge: Bytes32::from(
            "ae83525ba8d1dd3f09b277de18ca3e43fc0af20d20c4b3e92ef2a48bd291ccb2"
        ),
        genesis_pre_farm_farmer_puzzle_hash: Bytes32::from(
            "3d8765d3a597ec1d99663f6c9816d915b9f68613ac94009884c4addaefcce6af"
        ),
        genesis_pre_farm_pool_puzzle_hash: Bytes32::from(
            "d23da14695a188ae5708dd152263c4db883eb27edeb936178d4d988b8f3ce5fc"
        ),
        mempool_block_buffer: BigInt::from(10),
        min_plot_size: 18,
        bech32_prefix: String::from("txch"),
        is_testnet: true,
        ..Default::default()
    };
    pub static ref CONSENSUS_CONSTANTS_MAP: HashMap<String, ConsensusConstants> = HashMap::from([
        ("testnet0".to_string(), TESTNET_0.clone()),
        ("testnet2".to_string(), TESTNET_2.clone()),
        ("testnet3".to_string(), TESTNET_3.clone()),
        ("testnet4".to_string(), TESTNET_4.clone()),
        ("testnet4".to_string(), TESTNET_5.clone()),
        ("testnet7".to_string(), TESTNET_7.clone()),
        ("testnet10".to_string(), TESTNET_10.clone()),
        ("mainnet".to_string(), MAINNET.clone()),
    ]);
}
