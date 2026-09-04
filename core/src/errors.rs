use crate::blockchain::sized_bytes::Bytes48;
use std::error::Error;
use std::fmt;

#[derive(Debug)]
pub enum ClvmError {
    AtomNotValidU64(String),
    BadEncoding,
    /// A condition parsed successfully but is invalid, carrying the exact error code the
    /// condition parser assigns (e.g. `RESERVE_FEE_CONDITION_FAILED`, `COIN_AMOUNT_NEGATIVE`).
    /// The code is carried through the `ClvmError` → `ChiaError` mapping rather than collapsed.
    ConditionFailure(ChiaError),
    CostExceeded(u64, u64),
    DoubleSpend(String),
    DuplicateCreate(String),
    ExpectedPairGotAtom(String),
    ExpectedAtomGotPair(String),
    InvalidApplyArgs(String),
    InvalidArgCount(String),
    InvalidHex(String),
    InvalidInput(String),
    InvalidOperator(String),
    InvalidOperandList(String),
    InvalidOperandArgs(&'static str, usize),
    InvalidPublicKey(Bytes48),
    InvalidSyntax(String),
    InvalidSignature(String),
    InvalidSpendbundle(String),
    IoError(std::io::Error),
    NoOperatorFound(String),
    NoPostEval,
    Overflow(String),
    PathIntoAtom(String),
    PostEvalStackEmpty,
    Raise(String),
    ReservedOperator(String),
    SerializationError(String),
    TooManyAnnouncements,
    TooManyAtoms,
    TooManyPairs,
    OutOfMemory,
    UnexpectedEndOfValues(String),
    Unimplemented(u8),
    Unsupported(String),
    ValueStackEmpty,
}

impl From<ClvmError> for std::io::Error {
    fn from(e: ClvmError) -> std::io::Error {
        std::io::Error::other(e)
    }
}

impl From<std::io::Error> for ClvmError {
    fn from(e: std::io::Error) -> ClvmError {
        ClvmError::IoError(e)
    }
}

impl fmt::Display for ClvmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl Error for ClvmError {}

// See Source Here https://github.com/Chia-Network/chia-blockchain/blob/main/chia/util/errors.py
#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub enum ChiaError {
    DoesNotExtend = -1,
    BadHeaderSignature = -2,
    MissingFromStorage = -3,
    InvalidProtocolMessage = -4,
    SelfConnection = -5,
    InvalidHandshake = -6,
    InvalidAck = -7,
    IncompatibleProtocolVersion = -8,
    DuplicateConnection = -9,
    BlockNotInBlockchain = -10,
    NoProofOfSpaceFound = -11,
    PeersDontHaveBlock = -12,
    MaxInboundConnectionsReached = -13,
    UNKNOWN = 1,
    InvalidBlockSolution = 2,
    InvalidCoinSolution = 3,
    DuplicateOutput = 4,
    DoubleSpend = 5,
    UnknownUnspent = 6,
    BadAggregateSignature = 7,
    WrongPuzzleHash = 8,
    BadFarmerCoinAmount = 9,
    InvalidCondition = 10,
    AssertMyCoinIdFailed = 11,
    AssertAnnounceConsumedFailed = 12,
    AssertHeightRelativeFailed = 13,
    AssertHeightAbsoluteFailed = 14,
    AssertSecondsAbsoluteFailed = 15,
    CoinAmountExceedsMaximum = 16,
    SexpError = 17,
    InvalidFeeLowFee = 18,
    MempoolConflict = 19,
    MintingCoin = 20,
    ExtendsUnknownBlock = 21,
    CoinbaseNotYetSpendable = 22,
    BlockCostExceedsMax = 23,
    BadAdditionRoot = 24,
    BadRemovalRoot = 25,
    InvalidPospaceHash = 26,
    InvalidCoinbaseSignature = 27,
    InvalidPlotSignature = 28,
    TimestampTooFarInPast = 29,
    TimestampTooFarInFuture = 30,
    InvalidTransactionsFilterHash = 31,
    InvalidPospaceChallenge = 32,
    InvalidPospace = 33,
    InvalidHeight = 34,
    InvalidCoinbaseAmount = 35,
    InvalidMerkleRoot = 36,
    InvalidBlockFeeAmount = 37,
    InvalidWeight = 38,
    InvalidTotalIters = 39,
    BlockIsNotFinished = 40,
    InvalidNumIterations = 41,
    InvalidPot = 42,
    InvalidPotChallenge = 43,
    InvalidTransactionsGeneratorHash = 44,
    InvalidPoolTarget = 45,
    InvalidCoinbaseParent = 46,
    InvalidFeesCoinParent = 47,
    ReserveFeeConditionFailed = 48,
    NotBlockButHasData = 49,
    IsTransactionBlockButNoData = 50,
    InvalidPrevBlockHash = 51,
    InvalidTransactionsInfoHash = 52,
    InvalidFoliageBlockHash = 53,
    InvalidRewardCoins = 54,
    InvalidBlockCost = 55,
    NoEndOfSlotInfo = 56,
    InvalidPrevChallengeSlotHash = 57,
    InvalidSubEpochSummaryHash = 58,
    NoSubEpochSummaryHash = 59,
    ShouldNotMakeChallengeBlock = 60,
    ShouldMakeChallengeBlock = 61,
    InvalidChallengeChainData = 62,
    InvalidCcEosVdf = 65,
    InvalidRcEosVdf = 66,
    InvalidChallengeSlotHashRc = 67,
    InvalidPriorPointRc = 68,
    InvalidDeficit = 69,
    InvalidSubEpochSummary = 70,
    InvalidPrevSubEpochSummaryHash = 71,
    InvalidRewardChainHash = 72,
    InvalidSubEpochOverflow = 73,
    InvalidNewDifficulty = 74,
    InvalidNewSubSlotIters = 75,
    InvalidCcSpVdf = 76,
    InvalidRcSpVdf = 77,
    InvalidCcSignature = 78,
    InvalidRcSignature = 79,
    CannotMakeCcBlock = 80,
    InvalidRcSpPrevIp = 81,
    InvalidRcIpPrevIp = 82,
    InvalidIsTransactionBlock = 83,
    InvalidUrsbHash = 84,
    OldPoolTarget = 85,
    InvalidPoolSignature = 86,
    InvalidFoliageBlockPresence = 87,
    InvalidCcIpVdf = 88,
    InvalidRcIpVdf = 89,
    IpShouldBeNone = 90,
    InvalidRewardBlockHash = 91,
    InvalidMadeNonOverflowInfusions = 92,
    NoOverflowsInFirstSubSlotNewEpoch = 93,
    MempoolNotInitialized = 94,
    ShouldNotHaveIcc = 95,
    ShouldHaveIcc = 96,
    InvalidIccVdf = 97,
    InvalidIccHashCc = 98,
    InvalidIccHashRc = 99,
    InvalidIccEosVdf = 100,
    InvalidSpIndex = 101,
    TooManyBlocks = 102,
    InvalidCcChallenge = 103,
    InvalidPrefarm = 104,
    AssertSecondsRelativeFailed = 105,
    BadCoinbaseSignature = 106,
    //INITIAL_TRANSACTION_FREEZE = 107 // removed
    NoTransactionsWhileSyncing = 108,
    AlreadyIncludingTransaction = 109,
    IncompatibleNetworkId = 110,
    PreSoftForkMaxGeneratorSize = 111,
    InvalidRequiredIters = 112,
    TooManyGeneratorRefs = 113,
    AssertMyParentIdFailed = 114,
    AssertMyPuzzlehashFailed = 115,
    AssertMyAmountFailed = 116,
    GeneratorRuntimeError = 117,
    InvalidCostResult = 118,
    InvalidTransactionsGeneratorRefsRoot = 119,
    FutureGeneratorRefs = 120,
    GeneratorRefHasNoGenerator = 121,
    DoubleSpendInFork = 122,
    InvalidFeeTooCloseToZero = 123,
    CoinAmountNegative = 124,
    InternalProtocolError = 125,
    InvalidSpendBundle = 126,
    FailedGettingGeneratorMultiprocessing = 127,
    AssertConcurrentSpendFailed = 132,
    AssertConcurrentPuzzleFailed = 133,
    AssertEphemeralFailed = 140,
    MessageNotSentOrReceived = 147,
    // INVALID_TRANSACTIONS_GENERATOR_ENCODING: the SF9 canonical-serialization rule.
    ComplexGeneratorReceived = 148,
    // TOO_MANY_SPENDS: the SF9 6,000-spend block limit.
    TooManySpends = 149,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum ErrorBand {
    Consensus = 1,
    Clvm = 2,
    Store = 3,
    Node = 4,
    Sync = 5,
    Mempool = 6,
    Vdf = 7,
    Peer = 8,
    Rpc = 9,
    Wallet = 10,
    Io = 15,
}

impl ErrorBand {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            ErrorBand::Consensus => "consensus",
            ErrorBand::Clvm => "clvm",
            ErrorBand::Store => "store",
            ErrorBand::Node => "node",
            ErrorBand::Sync => "sync",
            ErrorBand::Mempool => "mempool",
            ErrorBand::Vdf => "vdf",
            ErrorBand::Peer => "peer",
            ErrorBand::Rpc => "rpc",
            ErrorBand::Wallet => "wallet",
            ErrorBand::Io => "io",
        }
    }
}

/// The common error interface: every error family in the workspace reports a stable numeric
/// code and the band it belongs to, so logs, RPC responses, and cross-crate handling can key
/// on numbers instead of downcasting concrete types.
pub trait ErrorCode {
    fn band(&self) -> ErrorBand;
    /// The variant's stable index inside its band. Append-only.
    fn variant(&self) -> u16;
    /// `band << 16 | variant` — globally unique and stable.
    fn error_code(&self) -> u32 {
        (self.band() as u32) << 16 | u32::from(self.variant())
    }
}

impl ErrorCode for ChiaError {
    fn band(&self) -> ErrorBand {
        ErrorBand::Consensus
    }
    fn variant(&self) -> u16 {
        (*self as i16) as u16
    }
}

impl ErrorCode for ClvmError {
    fn band(&self) -> ErrorBand {
        match self {
            ClvmError::ConditionFailure(inner) => inner.band(),
            ClvmError::IoError(_) => ErrorBand::Io,
            _ => ErrorBand::Clvm,
        }
    }
    fn variant(&self) -> u16 {
        match self {
            ClvmError::ConditionFailure(inner) => inner.variant(),
            ClvmError::AtomNotValidU64(_) => 1,
            ClvmError::BadEncoding => 2,
            ClvmError::CostExceeded(_, _) => 3,
            ClvmError::DoubleSpend(_) => 4,
            ClvmError::DuplicateCreate(_) => 5,
            ClvmError::ExpectedPairGotAtom(_) => 6,
            ClvmError::ExpectedAtomGotPair(_) => 7,
            ClvmError::InvalidApplyArgs(_) => 8,
            ClvmError::InvalidArgCount(_) => 9,
            ClvmError::InvalidHex(_) => 10,
            ClvmError::InvalidInput(_) => 11,
            ClvmError::InvalidOperator(_) => 12,
            ClvmError::InvalidOperandList(_) => 13,
            ClvmError::InvalidOperandArgs(_, _) => 14,
            ClvmError::InvalidPublicKey(_) => 15,
            ClvmError::InvalidSyntax(_) => 16,
            ClvmError::InvalidSignature(_) => 17,
            ClvmError::InvalidSpendbundle(_) => 18,
            ClvmError::IoError(_) => 19,
            ClvmError::NoOperatorFound(_) => 20,
            ClvmError::NoPostEval => 21,
            ClvmError::Overflow(_) => 22,
            ClvmError::PathIntoAtom(_) => 23,
            ClvmError::PostEvalStackEmpty => 24,
            ClvmError::Raise(_) => 25,
            ClvmError::ReservedOperator(_) => 26,
            ClvmError::SerializationError(_) => 27,
            ClvmError::TooManyAnnouncements => 28,
            ClvmError::TooManyAtoms => 29,
            ClvmError::TooManyPairs => 30,
            ClvmError::Unimplemented(_) => 31,
            ClvmError::OutOfMemory => 32,
            ClvmError::UnexpectedEndOfValues(_) => 33,
            ClvmError::Unsupported(_) => 34,
            ClvmError::ValueStackEmpty => 35,
        }
    }
}

/// The unified carrier: a code plus context, convertible from every family so boundaries
/// (RPC handlers, task returns, cross-crate joins) can hold one type without erasing the
/// numeric identity.
#[derive(Debug)]
pub struct DgError {
    pub code: u32,
    pub band: ErrorBand,
    pub message: String,
}

impl DgError {
    pub fn new<E: ErrorCode + fmt::Display>(e: &E) -> Self {
        Self {
            code: e.error_code(),
            band: e.band(),
            message: e.to_string(),
        }
    }
}

impl fmt::Display for DgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{}:{:#010x}] {}",
            self.band.name(),
            self.code,
            self.message
        )
    }
}

impl Error for DgError {}

impl From<DgError> for std::io::Error {
    fn from(e: DgError) -> Self {
        std::io::Error::other(e.to_string())
    }
}

impl<E: ErrorCode + fmt::Display> From<&E> for DgError {
    fn from(e: &E) -> Self {
        DgError::new(e)
    }
}

impl fmt::Display for ChiaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?} ({})", *self as i16)
    }
}

impl Error for ChiaError {}

#[cfg(test)]
#[path = "../tests/unit/errors/error_code_tests.rs"]
mod error_code_tests;
