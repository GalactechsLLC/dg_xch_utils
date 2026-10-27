//! Shared presentation defaults. Chain labels describe configured consensus rules,
//! never whichever optional cryptography happened to be compiled into the binary.
use crate::consensus::constants::ConsensusConstants;
use crate::signatures::{SignatureProfile, labrador::LabradorProfile};

/// Override at build time with DGX_POOL_NAME, or per deployment in PoolConfig.name.
pub const DEFAULT_POOL_NAME: &str = match option_env!("DGX_POOL_NAME") {
    Some(name) => name,
    None => "DGX pool",
};

pub const fn chain_profile_label(
    signature: SignatureProfile,
    aggregation: Option<LabradorProfile>,
) -> &'static str {
    match (signature, aggregation) {
        (SignatureProfile::Bls12381Aug, _) => "BLS",
        (SignatureProfile::FnDsa512V2, None) => "Falcon",
        (SignatureProfile::FnDsa512V2, Some(LabradorProfile::Packed11V2)) => "Falcon / LaBRADOR",
        (SignatureProfile::FnDsa512V2, Some(LabradorProfile::Packed11ZkV2)) => {
            "Falcon / LaBRADOR ZK"
        }
    }
}

pub fn consensus_label(constants: &ConsensusConstants) -> &'static str {
    chain_profile_label(constants.signature_profile, constants.falcon_aggregation)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_follow_the_chain_even_in_a_multi_profile_build() {
        assert_eq!(
            chain_profile_label(SignatureProfile::Bls12381Aug, None),
            "BLS"
        );
        assert_eq!(
            chain_profile_label(SignatureProfile::FnDsa512V2, None),
            "Falcon"
        );
        assert_eq!(
            chain_profile_label(
                SignatureProfile::FnDsa512V2,
                Some(LabradorProfile::Packed11V2)
            ),
            "Falcon / LaBRADOR"
        );
        assert_eq!(
            chain_profile_label(
                SignatureProfile::FnDsa512V2,
                Some(LabradorProfile::Packed11ZkV2)
            ),
            "Falcon / LaBRADOR ZK"
        );
    }
}
