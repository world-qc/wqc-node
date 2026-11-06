//! Feature bitmasks derived from wqc-core `/gates` (mirrors orchestrator `bid.RequiredFeatures`).

pub const FEATURE_STANDARD_GATES: u32 = 1 << 0;
pub const FEATURE_CUSTOM_UNITARY: u32 = 1 << 1;
/// Network trees that take a CNOT or CZ as `control` and `target` halves.
/// wqc-core lists `NETWORK_GATE_SPLIT` under `/gates`.
pub const FEATURE_NETWORK_GATE_SPLIT: u32 = 1 << 3;
/// This node judges announced `work` against its own speed and timeout
/// (docs `work-budget.md` §3). Set when core reports its rates.
pub const FEATURE_WORK_BUDGET: u32 = 1 << 4;

/// Maps gate names (from `GET /gates`) to the orchestrator feature bitmask.
pub fn features_from_gates(gates: &[String]) -> u32 {
    let mut features = 0u32;
    for gate in gates {
        apply_gate_feature(&mut features, gate);
    }
    features
}

fn apply_gate_feature(features: &mut u32, gate: &str) {
    match gate {
        "H" | "X" | "Y" | "Z" | "S" | "T" | "CNOT" | "CZ" | "CCNOT" => {
            *features |= FEATURE_STANDARD_GATES;
        }
        "RX" | "RY" | "RZ" => {
            *features |= FEATURE_CUSTOM_UNITARY;
        }
        "NETWORK_GATE_SPLIT" => {
            *features |= FEATURE_NETWORK_GATE_SPLIT;
        }
        _ => {}
    }
}

/// This node's bid mask: the gate features, plus [`FEATURE_WORK_BUDGET`] when
/// it can judge work.
pub fn node_features(gates: &[String], judges_work: bool) -> u32 {
    let mut features = features_from_gates(gates);
    if judges_work {
        features |= FEATURE_WORK_BUDGET;
    }
    features
}

/// Returns true when `supported` covers every bit in `required`.
pub fn supports_required_features(supported: u32, required: u32) -> bool {
    (supported & required) == required
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_gates_only() {
        let gates = vec!["H".into(), "CNOT".into()];
        assert_eq!(features_from_gates(&gates), FEATURE_STANDARD_GATES);
    }

    #[test]
    fn rotation_adds_custom_unitary_bit() {
        let gates = vec!["H".into(), "RX".into()];
        assert_eq!(
            features_from_gates(&gates),
            FEATURE_STANDARD_GATES | FEATURE_CUSTOM_UNITARY
        );
    }

    #[test]
    fn work_budget_bit_follows_core_rates() {
        let gates = vec!["H".into()];
        assert_eq!(node_features(&gates, false), FEATURE_STANDARD_GATES);
        assert_eq!(
            node_features(&gates, true),
            FEATURE_STANDARD_GATES | FEATURE_WORK_BUDGET
        );
    }

    #[test]
    fn empty_gate_list_yields_zero() {
        assert_eq!(features_from_gates(&[]), 0);
    }

    #[test]
    fn core_split_capability_sets_its_bit() {
        let gates = vec!["CNOT".into(), "NETWORK_GATE_SPLIT".into()];
        let features = features_from_gates(&gates);
        assert_eq!(
            features,
            FEATURE_STANDARD_GATES | FEATURE_NETWORK_GATE_SPLIT
        );
        assert!(!supports_required_features(
            FEATURE_STANDARD_GATES,
            FEATURE_STANDARD_GATES | FEATURE_NETWORK_GATE_SPLIT
        ));
        assert!(supports_required_features(
            features,
            FEATURE_STANDARD_GATES | FEATURE_NETWORK_GATE_SPLIT
        ));
    }
}
