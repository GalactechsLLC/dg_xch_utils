use crate::blockchain::signage_point::SignagePoint;
use crate::blockchain::subslot_bundle::SubSlotBundle;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct SignagePointOrEOS {
    pub signage_point: Option<SignagePoint>,
    pub eos: Option<SubSlotBundle>,
    pub time_received: f64,
    pub reverted: bool,
}
