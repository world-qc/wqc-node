//! Dense statevector memory envelope: `2^n × 16` bytes (matches wqc-core / orchestrator Gas).

pub const DENSE_AMPLITUDE_BYTES: u64 = 16;
pub const PHYSICAL_MEMORY_CAP_FRACTION: f64 = 0.8;
const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

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

/// Applies the 80% physical-RAM cap to the operator-requested WQC memory budget.
pub fn effective_memory_bytes(requested_gib: f64, total_physical_bytes: u64) -> u64 {
    let requested = (requested_gib.max(0.0) * GIB) as u64;
    let cap = (total_physical_bytes as f64 * PHYSICAL_MEMORY_CAP_FRACTION) as u64;
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
    fn caps_requested_memory_at_eighty_percent_of_physical() {
        let total = 10 * 1024u64 * 1024 * 1024;
        let bytes = effective_memory_bytes(9.0, total);
        assert_eq!(bytes, (10.0 * 0.8 * GIB) as u64);
    }

    #[test]
    fn resolve_applies_cap_before_qubit_conversion() {
        let total = 2 * 1024u64 * 1024 * 1024;
        let (qubits, effective_gib) = resolve_max_qubits_from_memory_gb(16.0, total);
        assert!((effective_gib - 1.6).abs() < 0.01);
        assert_eq!(qubits, 26);
    }
}
