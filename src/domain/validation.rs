use crate::domain::models::Gate;
use serde_json::Value;

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
    // The orchestrator dispatches one operand as a bare value, the form wqc-core reads; a
    // one-element array would fail in core, so it is refused here.
    if gate.params.as_array().is_some_and(|a| a.len() == 1) {
        return Err(format!(
            "gate {idx} has a one-element params array (expected a bare value)"
        ));
    }
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

    validate_qubit_params(gate, idx, qubit_count)
}

/// Qubit index carried by a JSON number. Floats that are not integers are not indices.
/// Negative integers are an error.
fn qubit_index(value: &Value) -> Result<Option<u64>, String> {
    if let Some(q) = value.as_i64() {
        if q < 0 {
            return Err(format!("negative qubit index {q}"));
        }
        return Ok(Some(q as u64));
    }
    if let Some(q) = value.as_u64() {
        return Ok(Some(q));
    }
    Ok(None)
}

fn ensure_in_range(q: u64, qubit_count: usize, idx: usize) -> Result<(), String> {
    if q as usize >= qubit_count {
        return Err(format!("Qubit index {q} out of range at gate {idx}"));
    }
    Ok(())
}

fn require_qubit(value: &Value, qubit_count: usize, idx: usize) -> Result<(), String> {
    match qubit_index(value) {
        Ok(Some(q)) => ensure_in_range(q, qubit_count, idx),
        Ok(None) => Err(format!("gate {idx} has a non-integer qubit index")),
        Err(msg) => Err(format!("{msg} at gate {idx}")),
    }
}

/// Rotation angle is the second parameter and is not a qubit index.
fn validate_rotation_qubit(gate: &Gate, idx: usize, qubit_count: usize) -> Result<(), String> {
    let qubit = if let Some(arr) = gate.params.as_array() {
        arr.first()
    } else {
        Some(&gate.params)
    };
    let Some(qubit) = qubit else {
        return Err(format!("rotation gate at index {idx} is missing a qubit"));
    };
    require_qubit(qubit, qubit_count, idx)
}

fn check_qubit_values(value: &Value, idx: usize, qubit_count: usize) -> Result<(), String> {
    match value {
        Value::Array(items) => {
            for item in items {
                check_qubit_values(item, idx, qubit_count)?;
            }
            Ok(())
        }
        Value::Number(_) => match qubit_index(value)? {
            Some(q) => ensure_in_range(q, qubit_count, idx),
            None => Err(format!("gate {idx} has a non-integer qubit index")),
        },
        Value::Object(map) => {
            if let Some(q) = map.get("qubit") {
                require_qubit(q, qubit_count, idx)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn validate_qubit_params(gate: &Gate, idx: usize, qubit_count: usize) -> Result<(), String> {
    if matches!(gate.r#type.as_str(), "RX" | "RY" | "RZ") {
        return validate_rotation_qubit(gate, idx, qubit_count);
    }
    check_qubit_values(&gate.params, idx, qubit_count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rejects_one_element_params_arrays() {
        let err = validate_circuit_logic(&[gate("H", json!([0]))], 2, &supported()).unwrap_err();
        assert!(err.contains("one-element"), "{err}");
        let nested = json!({"cbit": 0, "value": 1, "gate": {"type": "X", "params": [1]}});
        let err = validate_circuit_logic(&[gate("IF", nested)], 2, &supported()).unwrap_err();
        assert!(err.contains("one-element"), "{err}");
        let nested = json!({"cbit": 0, "value": 1, "gate": {"type": "X", "params": 1}});
        validate_circuit_logic(&[gate("H", json!(0)), gate("IF", nested)], 2, &supported())
            .unwrap();
    }

    fn gate(ty: &str, params: Value) -> Gate {
        Gate {
            r#type: ty.to_string(),
            params,
        }
    }

    fn supported() -> Vec<String> {
        ["H", "X", "CNOT", "RY", "MEASURE", "IF", "RESET"]
            .into_iter()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn rejects_scalar_qubit_index_out_of_range() {
        let err = validate_circuit_logic(&[gate("X", json!(5))], 2, &supported()).unwrap_err();
        assert!(err.contains("out of range"), "{err}");
    }

    #[test]
    fn rejects_negative_and_non_integer_qubit_index() {
        let err = validate_circuit_logic(&[gate("H", json!(-1))], 2, &supported()).unwrap_err();
        assert!(err.contains("negative"), "{err}");
        let err = validate_circuit_logic(&[gate("H", json!(1.5))], 2, &supported()).unwrap_err();
        assert!(err.contains("non-integer"), "{err}");
        let err =
            validate_circuit_logic(&[gate("CNOT", json!([0, 5]))], 2, &supported()).unwrap_err();
        assert!(err.contains("out of range"), "{err}");
    }

    #[test]
    fn rotation_angle_is_not_a_qubit_index() {
        validate_circuit_logic(&[gate("RY", json!([0, 3]))], 2, &supported()).unwrap();
        let err =
            validate_circuit_logic(&[gate("RY", json!([5, 0.3]))], 2, &supported()).unwrap_err();
        assert!(err.contains("out of range"), "{err}");
    }

    #[test]
    fn rejects_unsupported_gate_and_measure_qubit() {
        let err =
            validate_circuit_logic(&[gate("SWAP", json!([0, 1]))], 2, &supported()).unwrap_err();
        assert!(err.contains("not supported"), "{err}");

        let err = validate_circuit_logic(
            &[gate("MEASURE", json!({"qubit": 4, "cbit": 0}))],
            2,
            &supported(),
        )
        .unwrap_err();
        assert!(err.contains("out of range"), "{err}");
    }

    #[test]
    fn if_checks_nested_qubit_and_requires_a_gate() {
        let err = validate_circuit_logic(
            &[gate(
                "IF",
                json!({"cbit": 0, "value": 1, "gate": {"type": "X", "params": 9}}),
            )],
            2,
            &supported(),
        )
        .unwrap_err();
        assert!(err.contains("out of range"), "{err}");

        let err = validate_circuit_logic(
            &[gate("IF", json!({"cbit": 0, "value": 1}))],
            2,
            &supported(),
        )
        .unwrap_err();
        assert!(err.contains("missing nested gate"), "{err}");
    }
}
