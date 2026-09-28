//! Fixed sizes of a proof of space 2 proof.

/// A proof carries 128 x values, paired up through three tables.
pub const TOTAL_XS_IN_PROOF: usize = 128;
pub const TOTAL_T1_PAIRS_IN_PROOF: usize = 64;
pub const TOTAL_T2_PAIRS_IN_PROOF: usize = 32;
pub const TOTAL_T3_PAIRS_IN_PROOF: usize = 16;
pub const TOTAL_PROOF_FRAGMENTS_IN_PROOF: usize = 16;

/// Length of the quality chain: each link consumes one proof fragment.
pub const NUM_CHAIN_LINKS: usize = 16;

/// Chaining sets hold `1 << CHAIN_SET_BITS` entries.
pub const CHAIN_SET_BITS: u32 = 6;
pub const CHAIN_FACTOR_FRONT_LOAD_BITS: u32 = CHAIN_SET_BITS;

/// Distinct fragment sets a challenge selects. A chain may start in any of them and then cycles
/// through consecutive sets modulo this count, so `NUM_CHAIN_LINKS` must divide evenly by it for
/// every set to be visited the same number of times.
pub const NUM_CHALLENGE_SETS: usize = 4;

/// Zero low bits required of a fragment's iteration zero chain hash for it to start a chain. Set to
/// `log2(NUM_CHALLENGE_SETS)` so the expected number of starters does not move when the number of
/// challenge sets changes.
pub const CHAIN_STARTER_FILTER_BITS: u32 = 2;

/// Mixed into `g` on testnets so a testnet plot is not a valid mainnet plot.
pub const TESTNET_G_XOR_CONST: u32 = 0xA3B1_C4D7;

const _: () = assert!(NUM_CHAIN_LINKS.is_multiple_of(NUM_CHALLENGE_SETS));
const _: () = assert!((1usize << CHAIN_STARTER_FILTER_BITS) == NUM_CHALLENGE_SETS);
const _: () = assert!(TOTAL_XS_IN_PROOF == 8 * NUM_CHAIN_LINKS);
