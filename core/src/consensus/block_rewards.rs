pub const MOJO_PER_CHIA: u64 = 1_000_000_000_000;
pub const BLOCKS_PER_YEAR: u32 = 1_681_920;

#[derive(Debug, Copy, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RewardSchedule {
    pub genesis_pool: u64,
    pub genesis_farmer: u64,
    pub initial_pool: u64,
    pub initial_farmer: u64,
    pub halving_interval: u32,
    pub max_halvings: u8,
}

impl RewardSchedule {
    pub const CHIA: Self = Self {
        genesis_pool: 18_375_000_000_000_000_000,
        genesis_farmer: 2_625_000_000_000_000_000,
        initial_pool: 1_750_000_000_000,
        initial_farmer: 250_000_000_000,
        halving_interval: 3 * BLOCKS_PER_YEAR,
        max_halvings: 4,
    };

    pub const NO_PREFARM: Self = Self {
        genesis_pool: 0,
        genesis_farmer: 0,
        ..Self::CHIA
    };

    pub fn validate(&self, max_coin_amount: u64) -> Result<(), String> {
        if self.halving_interval == 0 || self.max_halvings > 63 {
            return Err("reward halving interval must be positive and max_halvings <= 63".into());
        }
        if [
            self.genesis_pool,
            self.genesis_farmer,
            self.initial_pool,
            self.initial_farmer,
        ]
        .into_iter()
        .any(|amount| amount > max_coin_amount)
        {
            return Err("reward exceeds maximum coin amount".into());
        }
        Ok(())
    }

    pub fn pool_reward(&self, height: u32) -> u64 {
        if height == 0 {
            self.genesis_pool
        } else {
            self.initial_pool >> self.halvings(height)
        }
    }

    pub fn farmer_reward(&self, height: u32) -> u64 {
        if height == 0 {
            self.genesis_farmer
        } else {
            self.initial_farmer >> self.halvings(height)
        }
    }

    fn halvings(&self, height: u32) -> u32 {
        (height / self.halving_interval.max(1)).min(u32::from(self.max_halvings.min(63)))
    }
}

impl Default for RewardSchedule {
    fn default() -> Self {
        Self::CHIA
    }
}

/// Pool reward: 7/8 of the tier reward. Each tier's mojo value is divisible by 8, and the
/// genesis tier is computed as `(7_000_000_000_000 / 8) * 21_000_000` (dividing by 8 first
/// keeps the intermediate below `u64::MAX`).
#[must_use]
pub const fn calculate_pool_reward(height: u32) -> u64 {
    /*
    Returns the pool reward at a certain block height. The pool earns 7/8 of the reward in each block. If the farmer
    is solo farming, they act as the pool, and therefore earn the entire block reward.
    These halving events will not be hit at the exact times
    (3 years, etc), due to fluctuations in difficulty. They will likely come early, if the network space and VDF
    rates increase continuously.
    */
    if height == 0 {
        (7_000_000_000_000 / 8) * 21_000_000
    } else if height < 3 * BLOCKS_PER_YEAR {
        7_000_000_000_000 / 4
    } else if height < 6 * BLOCKS_PER_YEAR {
        7_000_000_000_000 / 8
    } else if height < 9 * BLOCKS_PER_YEAR {
        7_000_000_000_000 / 16
    } else if height < 12 * BLOCKS_PER_YEAR {
        7_000_000_000_000 / 32
    } else {
        7_000_000_000_000 / 64
    }
}

/// Base farmer reward: 1/8 of the tier reward; genesis `(1_000_000_000_000 / 8) * 21_000_000`
/// (divide-by-8-first avoids overflow, as in the pool-reward derivation).
#[must_use]
pub const fn calculate_base_farmer_reward(height: u32) -> u64 {
    /*
    Returns the base farmer reward at a certain block height.
    The base fee reward is 1/8 of total block reward

    Returns the coinbase reward at a certain block height. These halving events will not be hit at the exact times
    (3 years, etc), due to fluctuations in difficulty. They will likely come early, if the network space and VDF
    rates increase continuously.
    */
    if height == 0 {
        (1_000_000_000_000 / 8) * 21_000_000
    } else if height < 3 * BLOCKS_PER_YEAR {
        1_000_000_000_000 / 4
    } else if height < 6 * BLOCKS_PER_YEAR {
        1_000_000_000_000 / 8
    } else if height < 9 * BLOCKS_PER_YEAR {
        1_000_000_000_000 / 16
    } else if height < 12 * BLOCKS_PER_YEAR {
        1_000_000_000_000 / 32
    } else {
        1_000_000_000_000 / 64
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
