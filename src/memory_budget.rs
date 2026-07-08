//! Dense statevector memory envelope: `2^n × 16` bytes (matches wqc-core / orchestrator Gas).

pub const DENSE_AMPLITUDE_BYTES: u64 = 16;
const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

pub const HOST_MEMORY_RESERVE_THRESHOLD_GIB: u64 = 16;
pub const HOST_MEMORY_RESERVE_SMALL_GIB: u64 = 1;
pub const HOST_MEMORY_RESERVE_LARGE_GIB: u64 = 2;

/// Headroom to leave on the host (GiB).
pub fn host_memory_reserve_gib(total_gib: u64) -> u64 {
    if total_gib >= HOST_MEMORY_RESERVE_THRESHOLD_GIB {
        HOST_MEMORY_RESERVE_LARGE_GIB
    } else {
        HOST_MEMORY_RESERVE_SMALL_GIB
    }
}

/// Maximum WQC memory budget (GiB) from total physical RAM (GiB).
pub fn max_wqc_memory_gib_from_total(total_gib: u64) -> u64 {
    if total_gib == 0 {
        return 1;
    }
    total_gib
        .saturating_sub(host_memory_reserve_gib(total_gib))
        .max(1)
}

/// Maximum WQC memory budget (bytes) from total physical RAM (bytes).
pub fn max_wqc_memory_bytes_from_total(total_bytes: u64) -> u64 {
    let gib = total_bytes / (1024 * 1024 * 1024);
    max_wqc_memory_gib_from_total(gib).saturating_mul(1024 * 1024 * 1024)
}

/// Maximum compact qubit width storable in `budget_bytes` at dense amplitude resolution.
pub fn max_qubits_from_dense_memory_budget(budget_bytes: u64) -> usize {
    if budget_bytes < DENSE_AMPLITUDE_BYTES {
        return 0;
    }
    let mut n = 0usize;
    while n < 63 && DENSE_AMPLITUDE_BYTES.saturating_mul(1u64 << n) <= budget_bytes {
        n += 1;
    }
    n.saturating_sub(1)
}

/// Caps operator-requested WQC memory budget to host total minus reserve.
pub fn effective_memory_bytes(requested_gib: f64, total_physical_bytes: u64) -> u64 {
    let requested = (requested_gib.max(0.0) * GIB) as u64;
    let cap = max_wqc_memory_bytes_from_total(total_physical_bytes);
    requested.min(cap)
}

/// Resolves `(max_qubits, effective_gib)` from `WQC_MAX_MEMORY_GB` and host RAM.
pub fn resolve_max_qubits_from_memory_gb(
    requested_gib: f64,
    total_physical_bytes: u64,
) -> (usize, f64) {
    let bytes = effective_memory_bytes(requested_gib, total_physical_bytes);
    let effective_gib = bytes as f64 / GIB;
    (
        max_qubits_from_dense_memory_budget(bytes),
        effective_gib,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_gib_yields_26_qubits() {
        let gib = 1024u64 * 1024 * 1024;
        assert_eq!(max_qubits_from_dense_memory_budget(gib), 26);
    }

    #[test]
    fn sixteen_gib_yields_30_qubits() {
        let gib = 16 * 1024u64 * 1024 * 1024;
        assert_eq!(max_qubits_from_dense_memory_budget(gib), 30);
    }

    #[test]
    fn caps_at_total_minus_one_gib_below_sixteen() {
        let total = 10 * 1024u64 * 1024 * 1024;
        let bytes = effective_memory_bytes(9.0, total);
        assert_eq!(bytes, 9 * 1024u64 * 1024 * 1024);
    }

    #[test]
    fn caps_at_total_minus_two_gib_at_sixteen() {
        let total = 16 * 1024u64 * 1024 * 1024;
        let bytes = effective_memory_bytes(16.0, total);
        assert_eq!(bytes, 14 * 1024u64 * 1024 * 1024);
    }

    #[test]
    fn resolve_applies_cap_before_qubit_conversion() {
        let total = 2 * 1024u64 * 1024 * 1024;
        let (qubits, effective_gib) = resolve_max_qubits_from_memory_gb(16.0, total);
        assert!((effective_gib - 1.0).abs() < 0.01);
        assert_eq!(qubits, 26);
    }

    #[test]
    fn max_memory_examples() {
        assert_eq!(max_wqc_memory_gib_from_total(8), 7);
        assert_eq!(max_wqc_memory_gib_from_total(16), 14);
        assert_eq!(max_wqc_memory_gib_from_total(32), 30);
    }
}
