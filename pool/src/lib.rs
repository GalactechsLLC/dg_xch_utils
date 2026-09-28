#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::todo,
        clippy::unimplemented
    )
)]

pub mod accounting;
pub mod authentication;
pub mod chain;
pub mod config;
pub mod http;
pub mod payout;
pub mod service;
pub mod store;
pub mod verification;
pub mod worker;
