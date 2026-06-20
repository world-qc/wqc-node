//! Feature bitmasks derived from wqc-core `/gates` (mirrors orchestrator `bid.RequiredFeatures`).

pub const FEATURE_STANDARD_GATES: u32 = 1 << 0;
pub const FEATURE_CUSTOM_UNITARY: u32 = 1 << 1;

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
        _ => {}
    }
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
    fn empty_gate_list_yields_zero() {
        assert_eq!(features_from_gates(&[]), 0);
    }
}
