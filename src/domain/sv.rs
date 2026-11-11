//! v2 (sv-stark) leaves (docs `p2p-protocols.md` §5, §6.1): the largest leaf, in gate rows,
//! this node advertises as `sv_max_rows` and accepts as `sv_leaf_rows`.

/// Protocol limit on a v2 leaf's gate rows.
pub const SV_MAX_ROWS: u64 = 1 << 22;

/// The orchestrator's timeout of a v2 leaf of `W` rows: 90 s + 50 µs × W, at most
/// [`MAX_TTL_SECS`] (its default `MaxSubTaskTTL`).
const TTL_BASE_SECS: f64 = 90.0;
const TTL_SECS_PER_ROW: f64 = 50e-6;
const MAX_TTL_SECS: f64 = 330.0;

/// Share of the memory budget a leaf may take.
const MEMORY_SHARE: f64 = 0.8;
/// A leaf's estimated time times this must fit the timeout.
const TIME_MARGIN: f64 = 1.5;

/// Bounds a measured rate must fall in: 10 ns to 1 s per row.
const SECONDS_PER_ROW: (f64, f64) = (1e-8, 1.0);

/// What core reports in `/sysinfo` about proving v2 leaves.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SvRates {
    pub seconds_per_row: f64,
    pub bytes_per_row: u64,
    pub base_bytes: u64,
}

impl SvRates {
    /// `None` when core does not prove v2 leaves (an older core, `WQC_SV_STARK_V2=0`, or a
    /// failed measurement) or reports something out of bounds.
    pub fn from_sysinfo(
        seconds_per_row: Option<f64>,
        bytes_per_row: Option<u64>,
        base_bytes: Option<u64>,
    ) -> Option<Self> {
        let (lo, hi) = SECONDS_PER_ROW;
        match (seconds_per_row, bytes_per_row, base_bytes) {
            (Some(s), Some(b), Some(base)) if s.is_finite() && s >= lo && s <= hi && b > 0 => {
                Some(Self {
                    seconds_per_row: s,
                    bytes_per_row: b,
                    base_bytes: base,
                })
            }
            _ => None,
        }
    }
}

/// The largest leaf this node can prove: the smaller of the rows whose memory fits
/// [`MEMORY_SHARE`] of `memory_bytes` and the rows whose time, with [`TIME_MARGIN`], fits both
/// the orchestrator's timeout and `compute_timeout_secs`; at most [`SV_MAX_ROWS`]. 0 means
/// none: the node then neither advertises nor accepts v2 leaves.
///
/// Advertising more than it can prove gets a node timed out or killed and slashed.
pub fn sv_max_rows(rates: Option<SvRates>, memory_bytes: u64, compute_timeout_secs: u64) -> u64 {
    let Some(r) = rates else {
        return 0;
    };
    let memory = (memory_bytes as f64 * MEMORY_SHARE - r.base_bytes as f64).max(0.0)
        / r.bytes_per_row as f64;

    // t·m·W ≤ 90 + 50e-6·W, and t·m·W ≤ min(330, node timeout).
    let per_row = r.seconds_per_row * TIME_MARGIN;
    let ttl = if per_row > TTL_SECS_PER_ROW {
        TTL_BASE_SECS / (per_row - TTL_SECS_PER_ROW)
    } else {
        f64::INFINITY
    };
    let cap = MAX_TTL_SECS.min(compute_timeout_secs as f64) / per_row;

    let rows = memory.min(ttl).min(cap).floor();
    if rows >= SV_MAX_ROWS as f64 {
        SV_MAX_ROWS
    } else {
        rows as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    fn rates(seconds_per_row: f64) -> Option<SvRates> {
        SvRates::from_sysinfo(Some(seconds_per_row), Some(4_600), Some(300_000_000))
    }

    #[test]
    fn rejects_missing_or_wild_rates() {
        assert!(SvRates::from_sysinfo(None, Some(4_600), Some(0)).is_none());
        assert!(SvRates::from_sysinfo(Some(1e-5), None, Some(0)).is_none());
        assert!(SvRates::from_sysinfo(Some(f64::NAN), Some(4_600), Some(0)).is_none());
        assert!(SvRates::from_sysinfo(Some(2.0), Some(4_600), Some(0)).is_none());
        assert!(SvRates::from_sysinfo(Some(1e-5), Some(0), Some(0)).is_none());
        assert_eq!(sv_max_rows(None, 64 * GIB, 300), 0);
    }

    #[test]
    fn a_large_fast_node_reaches_the_protocol_limit() {
        // e2-standard-8 class: 24 µs per row, 32 GB.
        assert_eq!(sv_max_rows(rates(24e-6), 32 * GIB, 300), SV_MAX_ROWS);
    }

    #[test]
    fn memory_bounds_a_small_node() {
        // 2.9 GB: (0.8 × 2.9 GB − 0.3 GB) / 4.6 KB ≈ 2^19.
        let rows = sv_max_rows(rates(1e-6), 2_900_000_000, 300);
        assert_eq!(rows, (2_900_000_000.0 * 0.8 - 3e8) as u64 / 4_600);
        assert_eq!(sv_max_rows(rates(1e-6), 300_000_000, 300), 0);
    }

    #[test]
    fn time_bounds_a_slow_node() {
        // e2-medium class: 313 µs per row. 1.5 t W ≤ 90 + 50e-6 W → W ≤ 90 / 419.5e-6.
        let rows = sv_max_rows(rates(313e-6), 64 * GIB, 300);
        assert_eq!(rows, (90.0 / (1.5 * 313e-6 - 50e-6)) as u64);
        let w = rows as f64;
        assert!(1.5 * 313e-6 * w <= 90.0 + 50e-6 * w);
        // A shorter node timeout bounds it further.
        assert_eq!(
            sv_max_rows(rates(313e-6), 64 * GIB, 60),
            (60.0 / (1.5 * 313e-6)) as u64
        );
    }
}
