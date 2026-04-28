use crate::models::Gate;

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

/// Calculates the target number of leading zero bits (Difficulty).
/// This MUST return a small integer (e.g., 5 to 32), not a huge score.
pub fn calculate_difficulty(circuit: &[crate::models::Gate], qubit_count: usize) -> u32 {
    // Base difficulty for any task
    let mut difficulty = 10u32;

    // Add difficulty based on qubit count (Logarithmic or small linear increase)
    // +1 difficulty for every 4 qubits
    difficulty += (qubit_count as u32) / 4;

    // Add difficulty based on gate count
    // +1 difficulty for every 50 gates
    difficulty += (circuit.len() as u32) / 50;

    // Cap the difficulty to prevent infinite loops (Self-protection)
    // 32 bits of zero means 4.2 billion hashes on average.
    // Adjust this based on your CPU power.
    difficulty.min(32)
}
