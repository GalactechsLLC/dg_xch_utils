pub const MOJO_PER_CHIA: u64 = 1000000000000;
pub const BLOCKS_PER_YEAR: u32 = 1681920;

pub const fn calculate_pool_reward(height: u32) -> u64 {
    /*
    Returns the pool reward at a certain block height. The pool earns 7/8 of the reward in each block. If the farmer
    is solo farming, they act as the pool, and therefore earn the entire block reward.
    These halving events will not be hit at the exact times
    (3 years, etc), due to fluctuations in difficulty. They will likely come early, if the network space and VDF
    rates increase continuously.
    */
    if height == 0 {
        (7000000000000 / 8) * 21000000
    } else if height < 3 * BLOCKS_PER_YEAR {
        7000000000000 / 4
    } else if height < 6 * BLOCKS_PER_YEAR {
        7000000000000 / 8
    } else if height < 9 * BLOCKS_PER_YEAR {
        7000000000000 / 16
    } else if height < 12 * BLOCKS_PER_YEAR {
        7000000000000 / 32
    } else {
        7000000000000 / 64
    }
}

pub const fn calculate_base_farmer_reward(height: u32) -> u64 {
    /*
    Returns the base farmer reward at a certain block height.
    The base fee reward is 1/8 of total block reward

    Returns the coinbase reward at a certain block height. These halving events will not be hit at the exact times
    (3 years, etc), due to fluctuations in difficulty. They will likely come early, if the network space and VDF
    rates increase continuously.
    */
    if height == 0 {
        (1000000000000 / 8) * 21000000
    } else if height < 3 * BLOCKS_PER_YEAR {
        1000000000000 / 4
    } else if height < 6 * BLOCKS_PER_YEAR {
        1000000000000 / 8
    } else if height < 9 * BLOCKS_PER_YEAR {
        1000000000000 / 16
    } else if height < 12 * BLOCKS_PER_YEAR {
        1000000000000 / 32
    } else {
        1000000000000 / 64
    }
}

#[test]
fn test_reward_heights() {
    //Pool Rewards
    assert_eq!(calculate_pool_reward(0), 18_375_000_000_000_000_000);
    assert_eq!(calculate_pool_reward(1), 1_750_000_000_000);
    assert_eq!(calculate_pool_reward(3 * BLOCKS_PER_YEAR), 875_000_000_000);
    assert_eq!(calculate_pool_reward(6 * BLOCKS_PER_YEAR), 437_500_000_000);
    assert_eq!(calculate_pool_reward(9 * BLOCKS_PER_YEAR), 218_750_000_000);
    assert_eq!(calculate_pool_reward(12 * BLOCKS_PER_YEAR), 109_375_000_000);
    //Farmer Rewards
    assert_eq!(calculate_base_farmer_reward(0), 2_625_000_000_000_000_000);
    assert_eq!(calculate_base_farmer_reward(1), 250_000_000_000);
    assert_eq!(
        calculate_base_farmer_reward(3 * BLOCKS_PER_YEAR),
        125_000_000_000
    );
    assert_eq!(
        calculate_base_farmer_reward(6 * BLOCKS_PER_YEAR),
        62_500_000_000
    );
    assert_eq!(
        calculate_base_farmer_reward(9 * BLOCKS_PER_YEAR),
        31_250_000_000
    );
    assert_eq!(
        calculate_base_farmer_reward(12 * BLOCKS_PER_YEAR),
        15_625_000_000
    );
    //Added Together Are Correct Total
    assert_eq!(
        calculate_base_farmer_reward(BLOCKS_PER_YEAR) + calculate_pool_reward(BLOCKS_PER_YEAR),
        2_000_000_000_000
    );
    assert_eq!(
        calculate_base_farmer_reward(3 * BLOCKS_PER_YEAR)
            + calculate_pool_reward(3 * BLOCKS_PER_YEAR),
        1_000_000_000_000
    );
    assert_eq!(
        calculate_base_farmer_reward(6 * BLOCKS_PER_YEAR)
            + calculate_pool_reward(6 * BLOCKS_PER_YEAR),
        500_000_000_000
    );
    assert_eq!(
        calculate_base_farmer_reward(9 * BLOCKS_PER_YEAR)
            + calculate_pool_reward(9 * BLOCKS_PER_YEAR),
        250_000_000_000
    );
    assert_eq!(
        calculate_base_farmer_reward(12 * BLOCKS_PER_YEAR)
            + calculate_pool_reward(12 * BLOCKS_PER_YEAR),
        125_000_000_000
    );
}
