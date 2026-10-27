//! Development FN-DSA-512 profile pinned to the maintained fn-dsa 0.4 format.
//! This is not a claim of compatibility with a future final FN-DSA standard.
//! Legacy PQClean Falcon keys/signatures are deliberately not accepted.

use super::{
    AuthorizationError, AuthorizationRules, AuthorizationVerifier, AuthorizationWitness,
    SignatureProfile, SignatureRequirement, SignatureScheme, SignatureSigner, SigningError,
};
#[cfg(feature = "falcon")]
use fn_dsa::{KeyPairGenerator, SigningKey, VerifyingKey};
#[cfg(feature = "falcon")]
use zeroize::Zeroizing;

pub type FalconPublicKey = [u8; 897];
pub type FalconSignature = [u8; 666];
pub const SECRET_KEY_BYTES: usize = 1345;
/// Shared with the LaBRADOR verifier's message representative computation.
pub const FN_DSA_CONTEXT: &[u8] = b"dg_xch/fn-dsa-512/v2";

pub struct FnDsa512V2;

impl FnDsa512V2 {
    pub fn valid_public_key(public_key: &FalconPublicKey) -> bool {
        #[cfg(feature = "falcon")]
        {
            fn_dsa::VerifyingKey512::decode(public_key).is_some()
        }
        #[cfg(not(feature = "falcon"))]
        {
            let _ = public_key;
            false
        }
    }
}

impl SignatureScheme for FnDsa512V2 {
    type PublicKey = FalconPublicKey;
    type Signature = FalconSignature;
    const PROFILE: SignatureProfile = SignatureProfile::FnDsa512V2;

    fn verify(
        &self,
        public_key: &FalconPublicKey,
        message: &[u8],
        signature: &FalconSignature,
    ) -> bool {
        if public_key[0] != 0x09 || signature[0] != 0x39 {
            return false;
        }
        #[cfg(feature = "falcon")]
        {
            fn_dsa::VerifyingKey512::decode(public_key).is_some_and(|key| {
                key.verify(
                    signature,
                    &fn_dsa::DomainContext(FN_DSA_CONTEXT),
                    &fn_dsa::HASH_ID_RAW,
                    message,
                )
            })
        }
        #[cfg(not(feature = "falcon"))]
        {
            let _ = message;
            false
        }
    }
}

// Adapt the existing, reviewed rand version to fn-dsa's rand_core 0.6 interface.
// Seed once from the OS so entropy failures return SigningError before entering
// the backend's infallible RNG interface. No deterministic fallback is used.
#[cfg(feature = "falcon")]
struct BackendRng(rand::rngs::StdRng);
#[cfg(feature = "falcon")]
impl BackendRng {
    fn new() -> Result<Self, SigningError> {
        use rand::SeedableRng;
        use ring::rand::SecureRandom;
        let mut seed = Zeroizing::new([0u8; 32]);
        ring::rand::SystemRandom::new()
            .fill(seed.as_mut())
            .map_err(|_| SigningError::SigningFailure)?;
        Ok(Self(rand::rngs::StdRng::from_seed(*seed)))
    }
}
#[cfg(feature = "falcon")]
impl fn_dsa::CryptoRng for BackendRng {}
#[cfg(feature = "falcon")]
impl fn_dsa::RngCore for BackendRng {
    fn next_u32(&mut self) -> u32 {
        rand::Rng::next_u32(&mut self.0)
    }
    fn next_u64(&mut self) -> u64 {
        rand::Rng::next_u64(&mut self.0)
    }
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        rand::Rng::fill_bytes(&mut self.0, dest);
    }
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), fn_dsa::RngError> {
        self.fill_bytes(dest);
        Ok(())
    }
}

/// Encoded and expanded secret keys are zeroized on drop, never Debug/Serialize.
/// Cache the expanded key to avoid rebuilding its FFT basis for every signature.
pub struct FalconSigner {
    public_key: FalconPublicKey,
    #[cfg(feature = "falcon")]
    secret_key: Zeroizing<[u8; SECRET_KEY_BYTES]>,
    #[cfg(feature = "falcon")]
    signing_key: std::sync::Mutex<Box<fn_dsa::SigningKey512>>,
}

// Versioned deterministic key-generation stream. Signing still uses fresh OS entropy.
#[cfg(feature = "falcon")]
struct SeedRng(fn_dsa::SHAKE256);
#[cfg(feature = "falcon")]
impl fn_dsa::CryptoRng for SeedRng {}
#[cfg(feature = "falcon")]
impl fn_dsa::RngCore for SeedRng {
    fn next_u32(&mut self) -> u32 {
        let mut bytes = [0; 4];
        self.0.extract(&mut bytes);
        u32::from_le_bytes(bytes)
    }
    fn next_u64(&mut self) -> u64 {
        let mut bytes = [0; 8];
        self.0.extract(&mut bytes);
        u64::from_le_bytes(bytes)
    }
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        self.0.extract(dest);
    }
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), fn_dsa::RngError> {
        self.0.extract(dest);
        Ok(())
    }
}

impl FalconSigner {
    /// Derive a recoverable FN-DSA v2 key from a BIP39 seed and a fixed application path.
    /// The pinned fn-dsa version and domain define this derivation format.
    #[cfg(feature = "falcon")]
    pub fn from_seed(
        seed: &[u8; 64],
        role: u32,
        index: u32,
        hardened: bool,
    ) -> Result<Self, SigningError> {
        let mut stream = fn_dsa::SHAKE256::new();
        stream.inject(b"dg_xch/fn-dsa-512/hd/v1\0");
        stream.inject(seed);
        stream.inject(&role.to_be_bytes());
        stream.inject(&index.to_be_bytes());
        stream.inject(&[u8::from(hardened)]);
        stream.flip();
        let mut rng = SeedRng(stream);
        let mut public_key = [0; 897];
        let mut secret_key = Zeroizing::new([0; SECRET_KEY_BYTES]);
        fn_dsa::KeyPairGenerator512::default().keygen(
            fn_dsa::FN_DSA_LOGN_512,
            &mut rng,
            secret_key.as_mut(),
            &mut public_key,
        );
        Self::from_secret_bytes(public_key, secret_key.as_ref())
    }

    pub fn generate() -> Result<Self, SigningError> {
        #[cfg(feature = "falcon")]
        {
            let mut public_key = [0; 897];
            let mut secret_key = Zeroizing::new([0; SECRET_KEY_BYTES]);
            let mut rng = BackendRng::new()?;
            let mut generator = fn_dsa::KeyPairGenerator512::default();
            generator.keygen(
                fn_dsa::FN_DSA_LOGN_512,
                &mut rng,
                secret_key.as_mut(),
                &mut public_key,
            );
            Self::from_secret_bytes(public_key, secret_key.as_ref())
        }
        #[cfg(not(feature = "falcon"))]
        {
            Err(SigningError::BackendUnavailable)
        }
    }

    #[cfg(feature = "falcon")]
    pub fn from_secret_bytes(
        public_key: FalconPublicKey,
        bytes: &[u8],
    ) -> Result<Self, SigningError> {
        if bytes.len() != SECRET_KEY_BYTES || public_key[0] != 0x09 || bytes[0] != 0x59 {
            return Err(SigningError::SigningFailure);
        }
        let key = fn_dsa::SigningKey512::decode(bytes).ok_or(SigningError::SigningFailure)?;
        let mut derived = [0; 897];
        key.to_verifying_key(&mut derived);
        if derived != public_key {
            return Err(SigningError::SigningFailure);
        }
        let mut secret_key = Zeroizing::new([0; SECRET_KEY_BYTES]);
        secret_key.copy_from_slice(bytes);
        let signer = Self {
            public_key,
            secret_key,
            signing_key: std::sync::Mutex::new(Box::new(key)),
        };
        // The backend assumes the encoded public-key hash is consistent; verify
        // a signature here to reject tampered imported keys, including that hash.
        let message = b"dg_xch/fn-dsa/key-pair-check/v2";
        let signature = signer.sign_message(message)?;
        if !FnDsa512V2.verify(&signer.public_key, message, &signature) {
            return Err(SigningError::SigningFailure);
        }
        Ok(signer)
    }

    #[cfg(feature = "falcon")]
    pub fn secret_bytes(&self) -> &[u8] {
        self.secret_key.as_ref()
    }

    pub fn sign_message(&self, message: &[u8]) -> Result<FalconSignature, SigningError> {
        #[cfg(feature = "falcon")]
        {
            let mut signature = [0; 666];
            let mut rng = BackendRng::new()?;
            self.signing_key
                .lock()
                .map_err(|_| SigningError::SigningFailure)?
                .sign(
                    &mut rng,
                    &fn_dsa::DomainContext(FN_DSA_CONTEXT),
                    &fn_dsa::HASH_ID_RAW,
                    message,
                    &mut signature,
                )
                .ok_or(SigningError::SigningFailure)?;
            Ok(signature)
        }
        #[cfg(not(feature = "falcon"))]
        {
            let _ = message;
            Err(SigningError::BackendUnavailable)
        }
    }
}

impl SignatureSigner for FalconSigner {
    type Scheme = FnDsa512V2;
    fn public_key(&self) -> FalconPublicKey {
        self.public_key
    }
    fn sign(&mut self, message: &[u8]) -> Result<FalconSignature, SigningError> {
        self.sign_message(message)
    }
}

pub struct FalconDirect;
impl AuthorizationVerifier for FalconDirect {
    fn verify(
        &self,
        rules: &AuthorizationRules,
        requirements: &[SignatureRequirement],
        witness: &AuthorizationWitness<'_>,
    ) -> Result<(), AuthorizationError> {
        rules.check_witness(super::AuthorizationMode::FalconDirect, witness)?;
        let AuthorizationWitness::FalconDirect(signatures) = witness else {
            return Err(AuthorizationError::InvalidSignature);
        };
        if requirements.len() != signatures.len() {
            return Err(AuthorizationError::InvalidSignature);
        }
        let mut seen = std::collections::HashSet::new();
        for (requirement, signature) in requirements.iter().zip(*signatures) {
            let SignatureRequirement::Falcon {
                occurrence,
                public_key,
                message,
            } = requirement
            else {
                return Err(AuthorizationError::WrongProfile {
                    expected: SignatureProfile::FnDsa512V2,
                    actual: requirement.profile(),
                });
            };
            if !seen.insert((occurrence.coin_id, occurrence.condition_index))
                || !FnDsa512V2.verify(public_key, message, signature)
            {
                return Err(AuthorizationError::InvalidSignature);
            }
        }
        Ok(())
    }
}

pub struct FalconLabrador;

impl AuthorizationVerifier for FalconLabrador {
    fn verify(
        &self,
        rules: &AuthorizationRules,
        requirements: &[SignatureRequirement],
        witness: &AuthorizationWitness<'_>,
    ) -> Result<(), AuthorizationError> {
        #[cfg(feature = "labrador")]
        {
            rules.check_witness(super::AuthorizationMode::FalconLabrador, witness)?;
            let genesis = rules
                .labrador_genesis()
                .ok_or(AuthorizationError::InvalidSignature)?;
            let AuthorizationWitness::FalconLabrador(proof) = witness else {
                return Err(AuthorizationError::InvalidSignature);
            };
            let profile = rules
                .labrador_profile()
                .ok_or(AuthorizationError::InvalidSignature)?;
            super::labrador::verify_evidence(profile, genesis, requirements, proof)
                .map_err(|_| AuthorizationError::InvalidSignature)
        }
        #[cfg(not(feature = "labrador"))]
        let _ = (rules, requirements, witness);
        #[cfg(not(feature = "labrador"))]
        Err(AuthorizationError::UnsupportedMode(
            super::AuthorizationMode::FalconLabrador,
        ))
    }
}
