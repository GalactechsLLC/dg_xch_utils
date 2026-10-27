//! Compatibility exports for the legacy BLS API. The implementation lives in the
//! signature provider so existing wallets, farmers, pools, and header validators
//! share it without changing their BLS wire types or signing conventions.

pub use crate::signatures::bls::{
    aggregate_signatures, aggregate_verify_signature, sign, sign_prepend, verify_signature,
};
