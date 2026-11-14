//! Feature bitmasks derived from wqc-core `/gates` (mirrors orchestrator `bid.RequiredFeatures`).

pub const FEATURE_STANDARD_GATES: u32 = 1 << 0;
pub const FEATURE_CUSTOM_UNITARY: u32 = 1 << 1;
/// Network trees that take a CNOT or CZ as `control` and `target` halves.
/// wqc-core lists `NETWORK_GATE_SPLIT` under `/gates`.
pub const FEATURE_NETWORK_GATE_SPLIT: u32 = 1 << 3;
/// This node judges announced `work` against its own speed and timeout
/// (docs `work-budget.md` §3). Set when core reports its rates.
pub const FEATURE_WORK_BUDGET: u32 = 1 << 4;
/// This node proves v2 (sv-stark) leaves up to its bid's `sv_max_rows`. Set when that is
/// nonzero.
pub const FEATURE_SV_STARK_V2: u32 = 1 << 5;
/// This node also proves mid-circuit circuits (RESET, IF, a gate after a MEASURE) as v2
/// leaves, rewritten by `wqc_sv_stark::wire::parse_leaf`. Set with [`FEATURE_SV_STARK_V2`]
/// when core reports `sv_dynamic`.
pub const FEATURE_SV_STARK_DYNAMIC: u32 = 1 << 6;
/// This node also proves `statevector_scalar` and `expectation` sub-tasks as v2 unitary leaves
/// and publishes their final state (`proof.sv_state_b64`). Set with [`FEATURE_SV_STARK_V2`]
/// when core reports `sv_state_outputs`.
pub const FEATURE_SV_STATE_OUTPUTS: u32 = 1 << 7;

/// What this node proves as v2 (sv-stark) leaves. The other two need `proves`.
#[derive(Clone, Copy, Debug, Default)]
pub struct SvCapability {
    /// v2 leaves at all (bit 5).
    pub proves: bool,
    /// Mid-circuit circuits too (bit 6).
    pub dynamic: bool,
    /// `statevector_scalar` and `expectation` with a published state (bit 7).
    pub state_outputs: bool,
}

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
/// it can judge work, and the v2 bits of `sv`: [`FEATURE_SV_STARK_V2`] when it
/// proves v2 leaves, with [`FEATURE_SV_STARK_DYNAMIC`] and
/// [`FEATURE_SV_STATE_OUTPUTS`] for what else those cover.
pub fn node_features(gates: &[String], judges_work: bool, sv: SvCapability) -> u32 {
    let mut features = features_from_gates(gates);
    if judges_work {
        features |= FEATURE_WORK_BUDGET;
    }
    if sv.proves {
        features |= FEATURE_SV_STARK_V2;
        if sv.dynamic {
            features |= FEATURE_SV_STARK_DYNAMIC;
        }
        if sv.state_outputs {
            features |= FEATURE_SV_STATE_OUTPUTS;
        }
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
        assert_eq!(
            node_features(&gates, false, SvCapability::default()),
            FEATURE_STANDARD_GATES
        );
        assert_eq!(
            node_features(&gates, true, SvCapability::default()),
            FEATURE_STANDARD_GATES | FEATURE_WORK_BUDGET
        );
    }

    #[test]
    fn sv_stark_bit_follows_sv_max_rows() {
        let gates = vec!["H".into()];
        assert_eq!(
            node_features(
                &gates,
                false,
                SvCapability {
                    proves: true,
                    dynamic: false,
                    state_outputs: false
                }
            ),
            FEATURE_STANDARD_GATES | FEATURE_SV_STARK_V2
        );
        assert_eq!(FEATURE_SV_STARK_V2, 1 << 5);
    }

    #[test]
    fn sv_dynamic_bit_needs_the_v2_bit() {
        let gates = vec!["H".into()];
        assert_eq!(
            node_features(
                &gates,
                false,
                SvCapability {
                    proves: true,
                    dynamic: true,
                    state_outputs: false
                }
            ),
            FEATURE_STANDARD_GATES | FEATURE_SV_STARK_V2 | FEATURE_SV_STARK_DYNAMIC
        );
        assert_eq!(
            node_features(
                &gates,
                false,
                SvCapability {
                    proves: false,
                    dynamic: true,
                    state_outputs: true
                }
            ),
            FEATURE_STANDARD_GATES
        );
        assert_eq!(FEATURE_SV_STARK_DYNAMIC, 1 << 6);
    }

    #[test]
    fn sv_state_outputs_bit_needs_the_v2_bit() {
        let gates = vec!["H".into()];
        assert_eq!(
            node_features(
                &gates,
                false,
                SvCapability {
                    proves: true,
                    dynamic: false,
                    state_outputs: true
                }
            ),
            FEATURE_STANDARD_GATES | FEATURE_SV_STARK_V2 | FEATURE_SV_STATE_OUTPUTS
        );
        assert_eq!(FEATURE_SV_STATE_OUTPUTS, 1 << 7);
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
