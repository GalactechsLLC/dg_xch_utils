use crate::blockchain::sized_bytes::Bytes32;
use std::fmt::Display;
use std::str::FromStr;

/// Serde uses the same lowercase names as [`FromStr`], so a network reads the same from a config
/// file as from a command line.
#[derive(Copy, Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChiaNetwork {
    Mainnet = 0,
    Simulator = 1,
    Testnet0 = 2,
    Testnet2 = 3,
    Testnet3 = 4,
    Testnet4 = 5,
    Testnet5 = 6,
    Testnet7 = 7,
    Testnet10 = 8,
    Testnet11 = 9,
}
impl FromStr for ChiaNetwork {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "mainnet" => Ok(ChiaNetwork::Mainnet),
            "simulator" => Ok(ChiaNetwork::Simulator),
            "testnet0" => Ok(ChiaNetwork::Testnet0),
            "testnet2" => Ok(ChiaNetwork::Testnet2),
            "testnet3" => Ok(ChiaNetwork::Testnet3),
            "testnet4" => Ok(ChiaNetwork::Testnet4),
            "testnet5" => Ok(ChiaNetwork::Testnet5),
            "testnet7" => Ok(ChiaNetwork::Testnet7),
            "testnet10" => Ok(ChiaNetwork::Testnet10),
            "testnet11" => Ok(ChiaNetwork::Testnet11),
            _ => Err(format!("{s} Is not a Valid Network")),
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ConsensusConstants {
    #[serde(default)]
    pub rewards: crate::consensus::block_rewards::RewardSchedule,
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

    pub significant_bits: u64, //The number of bits to look at in difficulty and min iters. The rest are zeroed
    pub discriminant_size_bits: u64, //Max is 1024 (based on ClassGroupElement int size)
    pub number_zero_bits_plot_filter: u8, //H(plot id + challenge hash + signage point) must start with these many zeroes
    pub min_plot_size: u8,
    pub max_plot_size: u8,
    pub sub_slot_time_target: u64, //The target number of seconds per sub-slot
    pub num_sp_intervals_extra: u64, //The difference between signage point and infusion point (plus required_iters)
    pub max_future_time: u64, //The next block can have a timestamp of at most these many seconds more
    pub max_future_time2: u64, //After soft-fork2, this is the new max_future_time
    pub number_of_timestamps: u64, //Than the average of the last number_of_timestamps blocks
    //Used as the initial cc rc challenges, as well as first block back pointers, and first SES back pointer
    //We override this value based on the chain being run (testnet0, testnet1, mainnet, etc)
    pub genesis_challenge: Bytes32,
    //Forks of chia should change this value to provide replay attack protection
    pub agg_sig_me_additional_data: Bytes32,
    pub genesis_pre_farm_pool_puzzle_hash: Bytes32, //The block at height must pay out to this pool puzzle hash
    pub genesis_pre_farm_farmer_puzzle_hash: Bytes32, //The block at height must pay out to this farmer puzzle hash
    pub max_vdf_witness_size: u64, //The maximum number of class group elements within an n-wesolowski proof
    //Size of mempool = 10x the size of block
    pub mempool_block_buffer: u64,
    //Max coin amount u(1 << 64). This allows coin amounts to fit in 64 bits. This is around 18M chia.
    pub max_coin_amount: u64,
    //Max block cost in clvm cost units
    pub max_block_cost_clvm: u64,
    //Cost per byte of generator program
    pub cost_per_byte: u64,

    pub weight_proof_threshold: u8,
    pub weight_proof_recent_blocks: u32,
    pub max_block_count_per_requests: u32,
    pub blocks_cache_size: u32,
    pub max_generator_size: u32,
    pub max_generator_ref_list_size: u32,
    pub pool_sub_slot_iters: u64,

    // soft fork initiated in 1.8.0 release
    pub soft_fork2_height: u32,

    // soft fork initiated in 2.0 release
    pub soft_fork3_height: u32,

    // the hard fork planned with the 2.0 release
    // this is the block with the first plot filter adjustment
    pub hard_fork_height: u32,
    pub hard_fork_fix_height: u32,

    // The soft fork staged by the 2.6/2.7 releases:
    // DISABLE_OP (modpow disabled until hard fork 2) + LIMITS (division-family operand-size caps).
    pub soft_fork8_height: u32,
    // Same release train: SIMPLE_GENERATOR + CANONICAL_INTS + LIMIT_SPENDS.
    pub soft_fork9_height: u32,
    pub hard_fork2_height: u32,

    // the plot filter adjustment heights
    pub plot_filter_128_height: u32,
    pub plot_filter_64_height: u32,
    pub plot_filter_32_height: u32,

    // proof of space 2 (activates with hard fork 2): the fixed v2 plot size, the strength range a
    // plot may choose, the v1 phase out window, and the v2 plot filter with its own schedule
    pub number_zero_bits_plot_filter_v2: u8,
    pub plot_size_v2: u8,
    pub min_plot_strength: u8,
    pub max_plot_strength: u8,
    pub plot_v1_phase_out_epoch_bits: u8,
    pub plot_filter_v2_first_adjustment_height: u32,
    pub plot_filter_v2_second_adjustment_height: u32,
    pub plot_filter_v2_third_adjustment_height: u32,

    //This is NOT standard, but makes some things easier
    pub bech32_prefix: &'static str,
    pub is_testnet: bool,
    pub simulated: bool,
}
impl Default for ConsensusConstants {
    fn default() -> Self {
        MAINNET
    }
}
impl Display for ConsensusConstants {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        if self.simulated {
            write!(f, "Simulator")
        } else if self.is_testnet {
            write!(f, "Testnet")
        } else if self.bech32_prefix == "xch" {
            write!(f, "Mainnet")
        } else {
            write!(f, "Unknown")
        }
    }
}
impl From<ChiaNetwork> for ConsensusConstants {
    fn from(n: ChiaNetwork) -> Self {
        match n {
            ChiaNetwork::Mainnet => MAINNET,
            ChiaNetwork::Simulator => SIMULATOR,
            ChiaNetwork::Testnet0 => TESTNET_0,
            ChiaNetwork::Testnet2 => TESTNET_2,
            ChiaNetwork::Testnet3 => TESTNET_3,
            ChiaNetwork::Testnet4 => TESTNET_4,
            ChiaNetwork::Testnet5 => TESTNET_5,
            ChiaNetwork::Testnet7 => TESTNET_7,
            ChiaNetwork::Testnet10 => TESTNET_10,
            ChiaNetwork::Testnet11 => TESTNET_11,
        }
    }
}
pub const MAINNET: ConsensusConstants = ConsensusConstants {
    rewards: crate::consensus::block_rewards::RewardSchedule::CHIA,
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
    significant_bits: 8,
    discriminant_size_bits: 1024,
    number_zero_bits_plot_filter: 9,
    min_plot_size: 32,
    max_plot_size: 50,
    sub_slot_time_target: 600,
    num_sp_intervals_extra: 3,
    max_future_time: 5 * 60,
    max_future_time2: 2 * 60,
    number_of_timestamps: 11,
    genesis_challenge: Bytes32::const_hex(
        "ccd5bb71183532bff220ba46c268991a3ff07eb358e8255a65c30a2dce0e5fbb",
    ),
    agg_sig_me_additional_data: Bytes32::const_hex(
        "ccd5bb71183532bff220ba46c268991a3ff07eb358e8255a65c30a2dce0e5fbb",
    ),
    genesis_pre_farm_pool_puzzle_hash: Bytes32::const_hex(
        "d23da14695a188ae5708dd152263c4db883eb27edeb936178d4d988b8f3ce5fc",
    ),
    genesis_pre_farm_farmer_puzzle_hash: Bytes32::const_hex(
        "3d8765d3a597ec1d99663f6c9816d915b9f68613ac94009884c4addaefcce6af",
    ),
    max_vdf_witness_size: 64,
    mempool_block_buffer: 10,
    max_coin_amount: u64::MAX,
    max_block_cost_clvm: 11_000_000_000_u64,
    cost_per_byte: 12000,
    weight_proof_threshold: 2,
    weight_proof_recent_blocks: 1000,
    max_block_count_per_requests: 32,
    blocks_cache_size: 4608 + (128 * 4),
    max_generator_size: 1_000_000,
    max_generator_ref_list_size: 512,
    pool_sub_slot_iters: 37_600_000_000,
    soft_fork2_height: 0,
    soft_fork3_height: 4_510_000,
    hard_fork_height: 5_496_000,
    hard_fork_fix_height: 5_496_000,
    soft_fork8_height: 8_655_000,
    soft_fork9_height: 8_655_000,
    hard_fork2_height: 0xFFFF_FFFA,
    plot_filter_128_height: 10_542_000,
    plot_filter_64_height: 15_592_000,
    plot_filter_32_height: 20_643_000,
    number_zero_bits_plot_filter_v2: 5,
    plot_size_v2: 28,
    min_plot_strength: 2,
    max_plot_strength: 32,
    plot_v1_phase_out_epoch_bits: 8,
    plot_filter_v2_first_adjustment_height: 0xFFFF_FFFB,
    plot_filter_v2_second_adjustment_height: 0xFFFF_FFFC,
    plot_filter_v2_third_adjustment_height: 0xFFFF_FFFD,
    bech32_prefix: "xch",
    is_testnet: false,
    simulated: false,
};

pub const SIMULATOR: ConsensusConstants = ConsensusConstants {
    genesis_challenge: Bytes32::const_hex(
        "eb8c4d20b322be8d9fddbf9412016bdffe9a2901d7edb0e364e94266d0e095f7",
    ),
    agg_sig_me_additional_data: Bytes32::const_hex(
        "eb8c4d20b322be8d9fddbf9412016bdffe9a2901d7edb0e364e94266d0e095f7",
    ),
    genesis_pre_farm_farmer_puzzle_hash: Bytes32::const_hex(
        "1e2e27c26e3ca085de2dd0732b63ccbd6fe54983110a045dcb28f6f146b6ad1d",
    ),
    genesis_pre_farm_pool_puzzle_hash: Bytes32::const_hex(
        "1e2e27c26e3ca085de2dd0732b63ccbd6fe54983110a045dcb28f6f146b6ad1d",
    ),
    hard_fork_height: 0,
    min_plot_size: 18,
    bech32_prefix: "txch",
    simulated: true,
    ..MAINNET
};

pub const TESTNET_0: ConsensusConstants = ConsensusConstants {
    genesis_challenge: Bytes32::const_hex(
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    ),
    genesis_pre_farm_farmer_puzzle_hash: Bytes32::const_hex(
        "3d8765d3a597ec1d99663f6c9816d915b9f68613ac94009884c4addaefcce6af",
    ),
    genesis_pre_farm_pool_puzzle_hash: Bytes32::const_hex(
        "d23da14695a188ae5708dd152263c4db883eb27edeb936178d4d988b8f3ce5fc",
    ),
    min_plot_size: 18,
    bech32_prefix: "txch",
    is_testnet: true,
    ..MAINNET
};
pub const TESTNET_2: ConsensusConstants = ConsensusConstants {
    difficulty_constant_factor: 10_052_721_566_054,
    genesis_challenge: Bytes32::const_hex(
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    ),
    genesis_pre_farm_farmer_puzzle_hash: Bytes32::const_hex(
        "3d8765d3a597ec1d99663f6c9816d915b9f68613ac94009884c4addaefcce6af",
    ),
    genesis_pre_farm_pool_puzzle_hash: Bytes32::const_hex(
        "d23da14695a188ae5708dd152263c4db883eb27edeb936178d4d988b8f3ce5fc",
    ),
    min_plot_size: 18,
    bech32_prefix: "txch",
    is_testnet: true,
    ..MAINNET
};
pub const TESTNET_3: ConsensusConstants = ConsensusConstants {
    difficulty_constant_factor: 10_052_721_566_054,
    genesis_challenge: Bytes32::const_hex(
        "ca7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015af",
    ),
    genesis_pre_farm_farmer_puzzle_hash: Bytes32::const_hex(
        "3d8765d3a597ec1d99663f6c9816d915b9f68613ac94009884c4addaefcce6af",
    ),
    genesis_pre_farm_pool_puzzle_hash: Bytes32::const_hex(
        "d23da14695a188ae5708dd152263c4db883eb27edeb936178d4d988b8f3ce5fc",
    ),
    mempool_block_buffer: 10,
    min_plot_size: 18,
    bech32_prefix: "txch",
    is_testnet: true,
    ..MAINNET
};
pub const TESTNET_4: ConsensusConstants = ConsensusConstants {
    difficulty_constant_factor: 10_052_721_566_054,
    difficulty_starting: 30,
    epoch_blocks: 768,
    genesis_challenge: Bytes32::const_hex(
        "dd7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015af",
    ),
    genesis_pre_farm_farmer_puzzle_hash: Bytes32::const_hex(
        "3d8765d3a597ec1d99663f6c9816d915b9f68613ac94009884c4addaefcce6af",
    ),
    genesis_pre_farm_pool_puzzle_hash: Bytes32::const_hex(
        "d23da14695a188ae5708dd152263c4db883eb27edeb936178d4d988b8f3ce5fc",
    ),
    mempool_block_buffer: 10,
    min_plot_size: 18,
    bech32_prefix: "txch",
    is_testnet: true,
    ..MAINNET
};
pub const TESTNET_5: ConsensusConstants = ConsensusConstants {
    difficulty_constant_factor: 10_052_721_566_054,
    difficulty_starting: 30,
    epoch_blocks: 768,
    genesis_challenge: Bytes32::const_hex(
        "ee7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015af",
    ),
    genesis_pre_farm_farmer_puzzle_hash: Bytes32::const_hex(
        "3d8765d3a597ec1d99663f6c9816d915b9f68613ac94009884c4addaefcce6af",
    ),
    genesis_pre_farm_pool_puzzle_hash: Bytes32::const_hex(
        "d23da14695a188ae5708dd152263c4db883eb27edeb936178d4d988b8f3ce5fc",
    ),
    mempool_block_buffer: 10,
    min_plot_size: 18,
    bech32_prefix: "txch",
    is_testnet: true,
    ..MAINNET
};
pub const TESTNET_7: ConsensusConstants = ConsensusConstants {
    difficulty_constant_factor: 10_052_721_566_054,
    difficulty_starting: 30,
    epoch_blocks: 768,
    genesis_challenge: Bytes32::const_hex(
        "117816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015af",
    ),
    genesis_pre_farm_farmer_puzzle_hash: Bytes32::const_hex(
        "3d8765d3a597ec1d99663f6c9816d915b9f68613ac94009884c4addaefcce6af",
    ),
    genesis_pre_farm_pool_puzzle_hash: Bytes32::const_hex(
        "d23da14695a188ae5708dd152263c4db883eb27edeb936178d4d988b8f3ce5fc",
    ),
    mempool_block_buffer: 10,
    min_plot_size: 18,
    bech32_prefix: "txch",
    is_testnet: true,
    ..MAINNET
};
pub const TESTNET_10: ConsensusConstants = ConsensusConstants {
    agg_sig_me_additional_data: Bytes32::const_hex(
        "ae83525ba8d1dd3f09b277de18ca3e43fc0af20d20c4b3e92ef2a48bd291ccb2",
    ),
    difficulty_constant_factor: 10_052_721_566_054,
    difficulty_starting: 30,
    epoch_blocks: 768,
    genesis_challenge: Bytes32::const_hex(
        "ae83525ba8d1dd3f09b277de18ca3e43fc0af20d20c4b3e92ef2a48bd291ccb2",
    ),
    genesis_pre_farm_farmer_puzzle_hash: Bytes32::const_hex(
        "3d8765d3a597ec1d99663f6c9816d915b9f68613ac94009884c4addaefcce6af",
    ),
    genesis_pre_farm_pool_puzzle_hash: Bytes32::const_hex(
        "d23da14695a188ae5708dd152263c4db883eb27edeb936178d4d988b8f3ce5fc",
    ),
    mempool_block_buffer: 10,
    min_plot_size: 18,
    soft_fork2_height: 3_000_000,
    soft_fork3_height: 2_997_292,
    // planned 2.0 release is July 26, height 2965036 on testnet
    //1 week later
    hard_fork_height: 2_997_292,
    // November 2023
    hard_fork_fix_height: 3_426_000,
    // another 2 weeks later
    plot_filter_128_height: 3_061_804,
    // 3 years later
    plot_filter_64_height: 8_010_796,
    // 3 years later
    plot_filter_32_height: 13_056_556,
    bech32_prefix: "txch",
    is_testnet: true,
    ..MAINNET
};
pub const TESTNET_11: ConsensusConstants = ConsensusConstants {
    agg_sig_me_additional_data: Bytes32::const_hex(
        "37a90eb5185a9c4439a91ddc98bbadce7b4feba060d50116a067de66bf236615",
    ),
    difficulty_constant_factor: 10_052_721_566_054,
    difficulty_starting: 30,
    epoch_blocks: 768,
    genesis_challenge: Bytes32::const_hex(
        "37a90eb5185a9c4439a91ddc98bbadce7b4feba060d50116a067de66bf236615",
    ),
    genesis_pre_farm_farmer_puzzle_hash: Bytes32::const_hex(
        "08296fc227decd043aee855741444538e4cc9a31772c4d1a9e6242d1e777e42a",
    ),
    genesis_pre_farm_pool_puzzle_hash: Bytes32::const_hex(
        "3ef7c233fc0785f3c0cae5992c1d35e7c955ca37a423571c1607ba392a9d12f7",
    ),
    mempool_block_buffer: 10,
    min_plot_size: 18,
    sub_slot_iters_starting: 67_108_864,
    //Forks activated from the beginning on this network
    hard_fork_height: 0,
    hard_fork_fix_height: 0,
    soft_fork8_height: 3_755_000,
    soft_fork9_height: 3_924_000,
    plot_filter_128_height: 6_029_568,
    plot_filter_64_height: 11_075_328,
    plot_filter_32_height: 16_121_088,
    bech32_prefix: "txch",
    is_testnet: true,
    ..MAINNET
};

#[must_use]
pub const fn get_pool_reward_prefix(consensus_constants: &'static ConsensusConstants) -> Bytes32 {
    let mut buf: [u8; 32] = [0; 32];
    let challenge = consensus_constants.genesis_challenge.const_bytes();
    let mut i = 0;
    while i < 16 {
        buf[i] = challenge[i];
        i += 1;
    }
    Bytes32::const_new(buf)
}

#[must_use]
pub const fn get_farmer_reward_prefix(consensus_constants: &'static ConsensusConstants) -> Bytes32 {
    let mut buf: [u8; 32] = [0; 32];
    let challenge = consensus_constants.genesis_challenge.const_bytes();
    let mut i = 0;
    while i < 16 {
        buf[i] = challenge[i + 16];
        i += 1;
    }
    Bytes32::const_new(buf)
}

pub const CONSENSUS_CONSTANTS: [ConsensusConstants; 10] = [
    MAINNET, SIMULATOR, TESTNET_0, TESTNET_2, TESTNET_3, TESTNET_4, TESTNET_5, TESTNET_7,
    TESTNET_10, TESTNET_11,
];

pub const FARMER_REWARD_PREFIXES: [(Bytes32, Bytes32); 10] = [
    (
        get_pool_reward_prefix(&MAINNET),
        get_farmer_reward_prefix(&MAINNET),
    ),
    (
        get_pool_reward_prefix(&SIMULATOR),
        get_farmer_reward_prefix(&SIMULATOR),
    ),
    (
        get_pool_reward_prefix(&TESTNET_0),
        get_farmer_reward_prefix(&TESTNET_0),
    ),
    (
        get_pool_reward_prefix(&TESTNET_2),
        get_farmer_reward_prefix(&TESTNET_2),
    ),
    (
        get_pool_reward_prefix(&TESTNET_3),
        get_farmer_reward_prefix(&TESTNET_3),
    ),
    (
        get_pool_reward_prefix(&TESTNET_4),
        get_farmer_reward_prefix(&TESTNET_4),
    ),
    (
        get_pool_reward_prefix(&TESTNET_5),
        get_farmer_reward_prefix(&TESTNET_5),
    ),
    (
        get_pool_reward_prefix(&TESTNET_7),
        get_farmer_reward_prefix(&TESTNET_7),
    ),
    (
        get_pool_reward_prefix(&TESTNET_10),
        get_farmer_reward_prefix(&TESTNET_10),
    ),
    (
        get_pool_reward_prefix(&TESTNET_11),
        get_farmer_reward_prefix(&TESTNET_11),
    ),
];
