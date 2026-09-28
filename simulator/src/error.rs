use dg_xch_core::errors::ChiaError;
use dg_xch_node::NodeError;
use dg_xch_vdf::Error as VdfError;
use std::error::Error;
use std::fmt;

/// Which stage of config validation rejected a field: types, then per-field ranges, then
/// cross-field invariants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ValidationTier {
    Type,
    Range,
    CrossField,
}

impl ValidationTier {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ValidationTier::Type => "type",
            ValidationTier::Range => "range",
            ValidationTier::CrossField => "cross-field",
        }
    }
}

impl fmt::Display for ValidationTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A rejected config field. Cross-field failures name both sides of the invariant in `field`, e.g.
/// `"epoch_blocks/sub_epoch_blocks"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub field: String,
    pub tier: ValidationTier,
    pub detail: String,
}

impl ConfigError {
    pub fn new(field: impl Into<String>, tier: ValidationTier, detail: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            tier,
            detail: detail.into(),
        }
    }

    pub fn typed(field: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::new(field, ValidationTier::Type, detail)
    }

    pub fn range(field: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::new(field, ValidationTier::Range, detail)
    }

    pub fn cross_field(field: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::new(field, ValidationTier::CrossField, detail)
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} validation rejected `{}`: {}",
            self.tier, self.field, self.detail
        )
    }
}

impl Error for ConfigError {}

#[derive(Debug)]
pub enum SimError {
    Config(ConfigError),
    Consensus(NodeError),
    Producer(ChiaError),
    Vdf(VdfError),
    Io(std::io::Error),
    /// A quantity shared by both tiers disagreed between them. Distinct from `Consensus`: a block
    /// the engine rejects is a modelled outcome, a broken invariant means the run is not usable.
    Invariant(String),
    /// The current sub-slot's signage points are used up: no non-overflow position yields an
    /// in-range successor. The caller crosses into a new sub-slot to keep farming.
    SubSlotExhausted,
}

impl fmt::Display for SimError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SimError::Config(e) => write!(f, "invalid config: {e}"),
            SimError::Consensus(e) => write!(f, "consensus error: {e}"),
            SimError::Producer(e) => write!(f, "producer error: {e:?}"),
            SimError::Vdf(e) => write!(f, "vdf error: {e}"),
            SimError::Io(e) => write!(f, "io error: {e}"),
            SimError::Invariant(s) => write!(f, "shared invariant violated: {s}"),
            SimError::SubSlotExhausted => {
                write!(
                    f,
                    "sub-slot signage points exhausted; cross into a new sub-slot"
                )
            }
        }
    }
}

impl Error for SimError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            SimError::Config(e) => Some(e),
            SimError::Consensus(e) => Some(e),
            SimError::Producer(_) => None,
            SimError::Vdf(e) => Some(e),
            SimError::Io(e) => Some(e),
            SimError::Invariant(_) | SimError::SubSlotExhausted => None,
        }
    }
}

impl From<ConfigError> for SimError {
    fn from(e: ConfigError) -> Self {
        SimError::Config(e)
    }
}

impl From<NodeError> for SimError {
    fn from(e: NodeError) -> Self {
        SimError::Consensus(e)
    }
}

impl From<VdfError> for SimError {
    fn from(e: VdfError) -> Self {
        SimError::Vdf(e)
    }
}

impl From<std::io::Error> for SimError {
    fn from(e: std::io::Error) -> Self {
        SimError::Io(e)
    }
}

#[cfg(test)]
#[path = "../tests/unit/error/tests.rs"]
mod tests;
