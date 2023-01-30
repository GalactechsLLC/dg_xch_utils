use crate::types::blockchain::signage_point::SignagePoint;
use crate::types::blockchain::subslot_bundle::SubSlotBundle;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SignagePointOrEOS {
    pub signage_point: Option<SignagePoint>,
    pub eos: Option<SubSlotBundle>,
    pub time_received: f64,
    pub reverted: bool,
}
