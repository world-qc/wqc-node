use crate::domain::models::Gate;

pub fn normalize_gate_params(circuit: &mut [Gate]) {
    for gate in circuit.iter_mut() {
        if let Some(arr) = gate.params.as_array() {
            if arr.len() == 1 {
                gate.params = arr[0].clone();
            }
        }
    }
}

pub fn validate_circuit_logic(
    circuit: &[Gate],
    qubit_count: usize,
    supported_gates: &[String],
) -> Result<(), String> {
    for (idx, gate) in circuit.iter().enumerate() {
        if !supported_gates.contains(&gate.r#type) {
            return Err(format!("Gate '{}' at index {} not supported", gate.r#type, idx));
        }

        if let Some(params) = gate.params.as_array() {
            for p in params {
                if let Some(q_idx) = p.as_u64() {
                    if q_idx as usize >= qubit_count {
                        return Err(format!("Qubit index {} out of range at gate {}", q_idx, idx));
                    }
                }
            }
        }
    }
    Ok(())
}
