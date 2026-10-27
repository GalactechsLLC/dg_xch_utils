//! Signature primitives and execution-derived authorization are separate interfaces.
//!
//! Existing chains retain [`AuthorizationRules::LEGACY_BLS`]. Explicit Falcon
//! development chains use the versioned detached envelope in [`falcon_block`].
//! LaBRADOR is an explicitly selected experimental development-chain profile.

mod authorization;
#[cfg(feature = "bls")]
pub mod bls;
pub mod extension;
pub mod falcon;
pub mod falcon_block;
#[cfg(feature = "bls")]
pub mod falcon_condition;
#[cfg(feature = "falcon")]
pub mod falcon_keys;
#[cfg(all(feature = "bls", feature = "falcon"))]
pub mod falcon_wallet;
pub mod labrador;

pub use authorization::{
    AuthorizationError, AuthorizationMode, AuthorizationRules, AuthorizationVerifier,
    AuthorizationWitness, SignatureOccurrence, SignatureRequirement, verify_authorization,
};

/// A complete signature profile, including its message and encoding conventions.
/// No numeric wire identifiers are assigned by this enum.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignatureProfile {
    #[default]
    Bls12381Aug,
    FnDsa512V2,
}

impl SignatureProfile {
    pub fn is_bls(&self) -> bool {
        *self == Self::Bls12381Aug
    }
}

/// Individual verification does not imply key addition or signature aggregation.
pub trait SignatureScheme {
    type PublicKey;
    type Signature;

    const PROFILE: SignatureProfile;

    fn verify(
        &self,
        public_key: &Self::PublicKey,
        message: &[u8],
        signature: &Self::Signature,
    ) -> bool;
}

/// Signers own their key/entropy policy; validators never need a secret-key type.
/// Mutable access permits a future signer to maintain an entropy provider.
pub trait SignatureSigner {
    type Scheme: SignatureScheme;

    fn public_key(&self) -> <Self::Scheme as SignatureScheme>::PublicKey;

    fn sign(
        &mut self,
        message: &[u8],
    ) -> Result<<Self::Scheme as SignatureScheme>::Signature, SigningError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SigningError {
    BackendUnavailable,
    EntropyFailure,
    SigningFailure,
}

impl std::fmt::Display for SigningError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::BackendUnavailable => "signature backend unavailable in this build",
            Self::EntropyFailure => "signature entropy unavailable",
            Self::SigningFailure => "signature generation failed",
        })
    }
}

impl std::error::Error for SigningError {}
