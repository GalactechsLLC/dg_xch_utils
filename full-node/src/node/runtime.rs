//! Runtime operations over the shared node state.

use super::*;

mod bulk;
mod follow;
pub(super) mod maintenance;
pub(super) use maintenance::IndexMaintenance;
mod peers;
mod proof;
mod resume;

impl<S> FullNode<S>
where
    S: BlockStore + CoinStore + Send + Sync + 'static,
{
    #[must_use]
    pub fn constants(&self) -> &ConsensusConstants {
        &self.constants
    }
}
