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
pub mod app;
pub mod backend;
pub mod config;
pub mod runner;
pub mod theme;

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

pub fn parse_mojos(input: &str) -> Result<u64, String> {
    let input = input.trim();
    let mut parts = input.split('.');
    let whole = parts.next().unwrap_or_default();
    let fraction = parts.next().unwrap_or_default();
    if parts.next().is_some()
        || whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || fraction.len() > 12
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err("Use a positive decimal with at most 12 fractional digits".into());
    }
    let whole: u64 = whole.parse().map_err(|_| "Amount is too large")?;
    let fraction: u64 = if fraction.is_empty() {
        0
    } else {
        fraction.parse().map_err(|_| "Invalid fraction")?
    };
    whole
        .checked_mul(1_000_000_000_000)
        .and_then(|whole| {
            whole.checked_add(
                fraction * 10u64.pow(12 - input.split('.').nth(1).unwrap_or_default().len() as u32),
            )
        })
        .ok_or_else(|| "Amount is too large".into())
}

pub fn format_mojos(amount: u128) -> String {
    format!(
        "{}.{:012}",
        amount / 1_000_000_000_000,
        amount % 1_000_000_000_000
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts_never_use_floating_point() {
        assert_eq!(parse_mojos("0.000000000001").unwrap(), 1);
        assert_eq!(parse_mojos("1.5").unwrap(), 1_500_000_000_000);
        assert_eq!(parse_mojos("18446744.073709551615").unwrap(), u64::MAX);
        for invalid in [
            "-1",
            "+1",
            "NaN",
            "1e3",
            "1.0000000000001",
            "1.2.3",
            "18446744.073709551616",
        ] {
            assert!(parse_mojos(invalid).is_err(), "{invalid}");
        }
    }
}
