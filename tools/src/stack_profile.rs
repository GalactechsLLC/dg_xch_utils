use clap::ValueEnum;
use std::io::{Error, ErrorKind};

/// Select the complete chain used by the development stack, including farming.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum StackProfile {
    #[default]
    Bls,
    Falcon,
    Labrador,
    LabradorZk,
}

impl StackProfile {
    pub fn matches(
        self,
        constants: &dg_xch_core::consensus::constants::ConsensusConstants,
    ) -> bool {
        let profile = constants.signature_profile;
        matches!(
            (self, profile),
            (
                Self::Bls,
                dg_xch_core::signatures::SignatureProfile::Bls12381Aug
            ) | (
                Self::Falcon | Self::Labrador | Self::LabradorZk,
                dg_xch_core::signatures::SignatureProfile::FnDsa512V2
            )
        ) && constants.falcon_aggregation
            == match self {
                Self::Labrador => {
                    Some(dg_xch_core::signatures::labrador::LabradorProfile::Packed11V2)
                }
                Self::LabradorZk => {
                    Some(dg_xch_core::signatures::labrador::LabradorProfile::Packed11ZkV2)
                }
                _ => None,
            }
    }

    /// Check before reading or creating identities, plots, or acceptance evidence.
    pub fn ensure_supported(self) -> Result<(), Error> {
        match self {
            Self::Bls => Ok(()),
            Self::Falcon if cfg!(feature = "falcon") => Ok(()),
            Self::Falcon => Err(Error::new(
                ErrorKind::Unsupported,
                "rebuild stack tools with --features falcon",
            )),
            Self::Labrador | Self::LabradorZk if cfg!(feature = "labrador") => Ok(()),
            Self::Labrador | Self::LabradorZk => Err(Error::new(
                ErrorKind::Unsupported,
                "rebuild stack tools with --features labrador",
            )),
        }
    }
}
