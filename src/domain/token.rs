//! WP §4 token units — mirrors orchestrator `ParseWQCToPlanck`.

use anyhow::{bail, Context, Result};
use num_bigint::BigInt;

/// 1 WQC = 10^18 Planck (pWQC).
pub const PLANCK_PER_WQC: u128 = 1_000_000_000_000_000_000;

/// Parses a human WQC amount (e.g. `"100"`, `"0.05"`) into Planck integers.
pub fn parse_wqc_to_planck(raw: &str) -> Result<BigInt> {
    parse_fixed_point_wqc(raw.trim(), 18, PLANCK_PER_WQC)
}

fn parse_fixed_point_wqc(raw: &str, max_frac_digits: usize, scale: u128) -> Result<BigInt> {
    if raw.is_empty() {
        bail!("empty WQC amount");
    }

    let parts: Vec<&str> = raw.split('.').collect();
    if parts.len() > 2 {
        bail!("invalid WQC amount {raw:?}");
    }

    let whole_str = if parts[0].is_empty() { "0" } else { parts[0] };
    let whole: BigInt = whole_str
        .parse()
        .with_context(|| format!("invalid WQC whole part in {raw:?}"))?;
    if whole < BigInt::from(0) {
        bail!("invalid WQC whole part in {raw:?}");
    }

    let frac_str = if parts.len() == 2 {
        let frac = parts[1];
        if frac.len() > max_frac_digits {
            bail!("WQC amount {raw:?} exceeds {max_frac_digits} fractional digits");
        }
        if !frac.chars().all(|c| c.is_ascii_digit()) {
            bail!("invalid WQC amount {raw:?}");
        }
        format!("{frac}{}", "0".repeat(max_frac_digits - frac.len()))
    } else {
        "0".repeat(max_frac_digits)
    };

    let frac: BigInt = frac_str
        .parse()
        .with_context(|| format!("invalid WQC fractional part in {raw:?}"))?;

    let scale = BigInt::from(scale);
    Ok(whole * &scale + frac)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_wqc_to_planck_whole_and_fraction() {
        let got = parse_wqc_to_planck("100").unwrap();
        let want: BigInt = "100000000000000000000".parse().unwrap();
        assert_eq!(got, want);

        let got = parse_wqc_to_planck("0.05").unwrap();
        let want: BigInt = "50000000000000000".parse().unwrap();
        assert_eq!(got, want);
    }

    #[test]
    fn parse_wqc_to_planck_rejects_too_many_digits() {
        assert!(parse_wqc_to_planck("0.0000000000000000001").is_err());
    }
}
