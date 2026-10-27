use super::SignatureProfile;
use super::falcon::{FalconPublicKey, FalconSignature};
use crate::blockchain::sized_bytes::{Bytes32, Bytes48, Bytes96};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthorizationMode {
    BlsAggregate,
    FalconDirect,
    FalconLabrador,
}

/// Constructed from trusted consensus policy, never negotiated with a witness.
/// Private fields require an explicit chain context for aggregate Falcon proofs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthorizationRules {
    mode: AuthorizationMode,
    labrador_genesis: Option<Bytes32>,
    labrador_profile: Option<super::labrador::LabradorProfile>,
}

impl AuthorizationRules {
    /// Existing BLS chains retain their original signatures and serialized identities.
    pub const LEGACY_BLS: Self = Self {
        mode: AuthorizationMode::BlsAggregate,
        labrador_genesis: None,
        labrador_profile: None,
    };

    pub fn for_mode(mode: AuthorizationMode) -> Result<Self, AuthorizationError> {
        let rules = Self {
            mode,
            labrador_genesis: None,
            labrador_profile: None,
        };
        rules.ensure_supported()?;
        Ok(rules)
    }

    pub fn for_labrador(genesis: Bytes32) -> Result<Self, AuthorizationError> {
        Self::for_labrador_profile(genesis, super::labrador::LabradorProfile::Packed11V2)
    }

    pub fn for_labrador_profile(
        genesis: Bytes32,
        profile: super::labrador::LabradorProfile,
    ) -> Result<Self, AuthorizationError> {
        let rules = Self {
            mode: AuthorizationMode::FalconLabrador,
            labrador_genesis: Some(genesis),
            labrador_profile: Some(profile),
        };
        rules.ensure_supported()?;
        Ok(rules)
    }

    #[cfg(feature = "labrador")]
    pub(super) fn labrador_genesis(self) -> Option<Bytes32> {
        self.labrador_genesis
    }

    #[cfg(feature = "labrador")]
    pub(super) fn labrador_profile(self) -> Option<super::labrador::LabradorProfile> {
        self.labrador_profile
    }

    #[must_use]
    pub const fn mode(self) -> AuthorizationMode {
        self.mode
    }

    fn ensure_supported(self) -> Result<(), AuthorizationError> {
        match self.mode {
            #[cfg(feature = "bls")]
            AuthorizationMode::BlsAggregate => Ok(()),
            #[cfg(feature = "falcon")]
            AuthorizationMode::FalconDirect => Ok(()),
            #[cfg(feature = "labrador")]
            AuthorizationMode::FalconLabrador if self.labrador_genesis.is_some() => Ok(()),
            _ => Err(AuthorizationError::UnsupportedMode(self.mode)),
        }
    }

    pub(super) fn check_witness(
        self,
        expected: AuthorizationMode,
        witness: &AuthorizationWitness<'_>,
    ) -> Result<(), AuthorizationError> {
        self.ensure_supported()?;
        if self.mode != expected {
            return Err(AuthorizationError::UnsupportedMode(expected));
        }
        if witness.mode() != self.mode {
            return Err(AuthorizationError::WrongWitness {
                expected: self.mode,
                actual: witness.mode(),
            });
        }
        Ok(())
    }
}

/// The position in the complete executed condition sequence, before filtering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SignatureOccurrence {
    pub coin_id: Bytes32,
    pub condition_index: u32,
}

/// Keys and fully derived messages supplied by trusted execution, not by the witness.
/// Equal BLS entries retain their multiplicity. Legacy grouped BLS conditions do not
/// preserve original condition indices, so they must not be relabeled as Falcon
/// occurrences; the Falcon execution adapter supplies those original indices directly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignatureRequirement {
    Bls {
        public_key: Bytes48,
        message: Vec<u8>,
    },
    Falcon {
        occurrence: SignatureOccurrence,
        public_key: Box<FalconPublicKey>,
        message: Vec<u8>,
    },
}

impl SignatureRequirement {
    #[must_use]
    pub const fn profile(&self) -> SignatureProfile {
        match self {
            Self::Bls { .. } => SignatureProfile::Bls12381Aug,
            Self::Falcon { .. } => SignatureProfile::FnDsa512V2,
        }
    }
}

/// Borrowed evidence, separate from the execution-derived requirements. Direct
/// Falcon evidence is carried by the versioned Falcon block envelope.
#[derive(Clone, Copy, Debug)]
pub enum AuthorizationWitness<'a> {
    BlsAggregate(&'a Bytes96),
    FalconDirect(&'a [FalconSignature]),
    FalconLabrador(&'a [u8]),
}

impl AuthorizationWitness<'_> {
    #[must_use]
    pub const fn mode(&self) -> AuthorizationMode {
        match self {
            Self::BlsAggregate(_) => AuthorizationMode::BlsAggregate,
            Self::FalconDirect(_) => AuthorizationMode::FalconDirect,
            Self::FalconLabrador(_) => AuthorizationMode::FalconLabrador,
        }
    }
}

pub trait AuthorizationVerifier {
    fn verify(
        &self,
        rules: &AuthorizationRules,
        requirements: &[SignatureRequirement],
        witness: &AuthorizationWitness<'_>,
    ) -> Result<(), AuthorizationError>;
}

/// Select an implemented backend under trusted chain policy. An untrusted witness
/// cannot change that policy or enable LaBRADOR on an existing direct-only chain.
pub fn verify_authorization(
    rules: &AuthorizationRules,
    requirements: &[SignatureRequirement],
    witness: &AuthorizationWitness<'_>,
) -> Result<(), AuthorizationError> {
    rules.ensure_supported()?;
    match rules.mode() {
        #[cfg(feature = "bls")]
        AuthorizationMode::BlsAggregate => {
            super::bls::BlsAggregate.verify(rules, requirements, witness)
        }
        #[cfg(feature = "falcon")]
        AuthorizationMode::FalconDirect => {
            super::falcon::FalconDirect.verify(rules, requirements, witness)
        }
        #[cfg(feature = "labrador")]
        AuthorizationMode::FalconLabrador => {
            super::falcon::FalconLabrador.verify(rules, requirements, witness)
        }
        #[cfg(not(all(feature = "bls", feature = "labrador")))]
        _ => {
            // Also keeps a build without BLS fail-closed, rather than accepting an
            // empty set or requiring the native BLS dependency for data-only users.
            let _ = (requirements, witness);
            Err(AuthorizationError::UnsupportedMode(rules.mode()))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthorizationError {
    UnsupportedMode(AuthorizationMode),
    WrongWitness {
        expected: AuthorizationMode,
        actual: AuthorizationMode,
    },
    WrongProfile {
        expected: SignatureProfile,
        actual: SignatureProfile,
    },
    InvalidSignature,
}

impl std::fmt::Display for AuthorizationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedMode(mode) => write!(f, "unsupported authorization mode: {mode:?}"),
            Self::WrongWitness { expected, actual } => {
                write!(
                    f,
                    "expected {expected:?} authorization witness, got {actual:?}"
                )
            }
            Self::WrongProfile { expected, actual } => {
                write!(f, "expected {expected:?} signature profile, got {actual:?}")
            }
            Self::InvalidSignature => f.write_str("invalid authorization signature"),
        }
    }
}

impl std::error::Error for AuthorizationError {}
