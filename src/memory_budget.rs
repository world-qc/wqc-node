//! Dense statevector memory envelope: `2^n × 16` bytes (matches wqc-core / orchestrator Gas).

pub const DENSE_AMPLITUDE_BYTES: u64 = 16;
const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
const GIB_U64: u64 = 1024 * 1024 * 1024;

pub const HOST_MEMORY_RESERVE_THRESHOLD_GIB: u64 = 16;
pub const HOST_MEMORY_RESERVE_SMALL_GIB: u64 = 1;
pub const HOST_MEMORY_RESERVE_LARGE_GIB: u64 = 2;

/// Headroom subtracted from adapter VRAM budget before intersecting with RAM.
pub const VRAM_MEMORY_RESERVE_GIB: u64 = 1;

/// Which envelope limited the effective dense budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryCapSource {
    Ram,
    Vram,
}

impl MemoryCapSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ram => "ram",
            Self::Vram => "vram",
        }
    }
}

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
    let gib = total_bytes / GIB_U64;
    max_wqc_memory_gib_from_total(gib).saturating_mul(GIB_U64)
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
/// `None` request = use full host−reserve (unset `WQC_MAX_MEMORY_GB`).
pub fn effective_memory_bytes(requested_gib: Option<f64>, total_physical_bytes: u64) -> u64 {
    let cap = max_wqc_memory_bytes_from_total(total_physical_bytes);
    match requested_gib {
        None => cap,
        Some(g) => ((g.max(0.0) * GIB) as u64).min(cap),
    }
}

/// Resolves `(max_qubits, effective_gib)` from optional `WQC_MAX_MEMORY_GB` and host RAM.
pub fn resolve_max_qubits_from_memory_gb(
    requested_gib: Option<f64>,
    total_physical_bytes: u64,
) -> (usize, f64) {
    let bytes = effective_memory_bytes(requested_gib, total_physical_bytes);
    let effective_gib = bytes as f64 / GIB;
    (max_qubits_from_dense_memory_budget(bytes), effective_gib)
}

/// Intersect RAM dense budget with adapter VRAM budget (WebGPU).
/// Returns `(max_qubits, effective_gib, which cap won)`.
pub fn resolve_max_qubits_with_optional_vram(
    ram_budget_bytes: u64,
    vram_budget_bytes: Option<u64>,
) -> (usize, f64, MemoryCapSource) {
    let (bytes, source) = match vram_budget_bytes.filter(|&b| b > 0) {
        Some(vram) => {
            let vram_usable = vram
                .saturating_sub(VRAM_MEMORY_RESERVE_GIB.saturating_mul(GIB_U64))
                .max(DENSE_AMPLITUDE_BYTES);
            if vram_usable < ram_budget_bytes {
                (vram_usable, MemoryCapSource::Vram)
            } else {
                (ram_budget_bytes, MemoryCapSource::Ram)
            }
        }
        None => (ram_budget_bytes, MemoryCapSource::Ram),
    };
    let effective_gib = bytes as f64 / GIB;
    (
        max_qubits_from_dense_memory_budget(bytes),
        effective_gib,
        source,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_gib_yields_26_qubits() {
        let gib = GIB_U64;
        assert_eq!(max_qubits_from_dense_memory_budget(gib), 26);
    }

    #[test]
    fn sixteen_gib_yields_30_qubits() {
        let gib = 16 * GIB_U64;
        assert_eq!(max_qubits_from_dense_memory_budget(gib), 30);
    }

    #[test]
    fn caps_at_total_minus_one_gib_below_sixteen() {
        let total = 10 * GIB_U64;
        let bytes = effective_memory_bytes(Some(9.0), total);
        assert_eq!(bytes, 9 * GIB_U64);
    }

    #[test]
    fn caps_at_total_minus_two_gib_at_sixteen() {
        let total = 16 * GIB_U64;
        let bytes = effective_memory_bytes(Some(16.0), total);
        assert_eq!(bytes, 14 * GIB_U64);
    }

    #[test]
    fn unset_request_uses_host_minus_reserve() {
        let total = 32 * GIB_U64;
        let bytes = effective_memory_bytes(None, total);
        assert_eq!(bytes, 30 * GIB_U64);
        let total_small = 8 * GIB_U64;
        assert_eq!(effective_memory_bytes(None, total_small), 7 * GIB_U64);
    }

    #[test]
    fn resolve_applies_cap_before_qubit_conversion() {
        let total = 2 * GIB_U64;
        let (qubits, effective_gib) = resolve_max_qubits_from_memory_gb(Some(16.0), total);
        assert!((effective_gib - 1.0).abs() < 0.01);
        assert_eq!(qubits, 26);
    }

    #[test]
    fn max_memory_examples() {
        assert_eq!(max_wqc_memory_gib_from_total(8), 7);
        assert_eq!(max_wqc_memory_gib_from_total(16), 14);
        assert_eq!(max_wqc_memory_gib_from_total(32), 30);
    }

    #[test]
    fn vram_smaller_than_ram_wins() {
        let ram = 16 * GIB_U64;
        let vram = 4 * GIB_U64; // usable 3 GiB after 1 GiB reserve
        let (qubits, gib, src) = resolve_max_qubits_with_optional_vram(ram, Some(vram));
        assert_eq!(src, MemoryCapSource::Vram);
        assert!((gib - 3.0).abs() < 0.01);
        assert_eq!(qubits, max_qubits_from_dense_memory_budget(3 * GIB_U64));
    }

    #[test]
    fn ram_smaller_than_vram_wins() {
        let ram = 2 * GIB_U64;
        let vram = 24 * GIB_U64;
        let (_, gib, src) = resolve_max_qubits_with_optional_vram(ram, Some(vram));
        assert_eq!(src, MemoryCapSource::Ram);
        assert!((gib - 2.0).abs() < 0.01);
    }

    #[test]
    fn no_vram_keeps_ram() {
        let ram = 8 * GIB_U64;
        let (_, _, src) = resolve_max_qubits_with_optional_vram(ram, None);
        assert_eq!(src, MemoryCapSource::Ram);
    }
}
