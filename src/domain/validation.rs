use crate::domain::models::Gate;
use serde_json::Value;

pub fn normalize_gate_params(circuit: &mut [Gate]) {
    for gate in circuit.iter_mut() {
        normalize_gate_value(&mut gate.params);
    }
}

fn normalize_gate_value(params: &mut Value) {
    if let Some(arr) = params.as_array() {
        if arr.len() == 1 {
            *params = arr[0].clone();
            return;
        }
    }

    if let Some(obj) = params.as_object_mut() {
        if let Some(nested) = obj.get_mut("gate") {
            if let Some(nested_obj) = nested.as_object_mut() {
                if let Some(nested_params) = nested_obj.get_mut("params") {
                    normalize_gate_value(nested_params);
                }
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
        validate_gate(gate, idx, qubit_count, supported_gates)?;
    }
    Ok(())
}

fn validate_gate(
    gate: &Gate,
    idx: usize,
    qubit_count: usize,
    supported_gates: &[String],
) -> Result<(), String> {
    if !supported_gates.contains(&gate.r#type) {
        return Err(format!(
            "Gate '{}' at index {} not supported",
            gate.r#type, idx
        ));
    }

    if gate.r#type == "IF" {
        let Some(obj) = gate.params.as_object() else {
            return Err(format!("IF gate at index {} requires object params", idx));
        };
        let Some(nested) = obj.get("gate") else {
            return Err(format!("IF gate at index {} missing nested gate", idx));
        };
        let nested_gate: Gate = serde_json::from_value(nested.clone())
            .map_err(|e| format!("IF gate at index {} has invalid nested gate: {e}", idx))?;
        return validate_gate(&nested_gate, idx, qubit_count, supported_gates);
    }

    if let Some(params) = gate.params.as_array() {
        for p in params {
            if let Some(q_idx) = p.as_u64() {
                if q_idx as usize >= qubit_count {
                    return Err(format!(
                        "Qubit index {} out of range at gate {}",
                        q_idx, idx
                    ));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalize_flattens_if_nested_gate_params() {
        let mut circuit = vec![Gate {
            r#type: "IF".to_string(),
            params: json!({
                "cbit": 0,
                "value": 1,
                "gate": {"type": "X", "params": [1]}
            }),
        }];
        normalize_gate_params(&mut circuit);
        assert_eq!(circuit[0].params["gate"]["params"], json!(1));
    }
}
