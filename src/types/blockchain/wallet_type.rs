use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum WalletType {
    StandardWallet = 0,
    RateLimited = 1,
    AtomicSwap = 2,
    AuthorizedPayee = 3,
    MultiSig = 4,
    Custody = 5,
    ColouredCoin = 6,
    RECOVERABLE = 7,
    DistributedId = 8,
    PoolingWallet = 9,
}
