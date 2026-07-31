pub mod discriminant;
pub mod error;
pub mod form;
pub mod proof;
pub mod validation;

pub use crate::discriminant::{create_discriminant, create_discriminant_bytes};
pub use crate::error::{Error, Result};
pub use crate::proof::{prove, verify_n_wesolowski, verify_vdf};
pub use crate::validation::{
    default_classgroup_element, validate_vdf_info, validate_vdf_info_result,
    validate_vdf_with_normalization, validate_vdf_with_normalization_result,
};

fn _version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
fn _pkg_name() -> &'static str {
    env!("CARGO_PKG_NAME")
}

#[must_use]
pub fn version() -> String {
    format!("{}: {}", _pkg_name(), _version())
}

#[test]
fn test_version() {
    println!("{}", version());
}
