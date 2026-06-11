use crate::domain::models::{ComplexResult, Proof};

pub const PROTOCOL_RESULT: &str = "/wqc/tensor-result/1.0.0";

/// ResultMessage mirrors the orchestrator P2P result stream payload.
pub struct ResultMessage {
    pub sub_task_id: String,
    pub node_id: String,
    pub complex_result: ComplexResult,
    pub proof: Proof,
    pub error: Option<String>,
}

impl ResultMessage {
    pub fn to_json_bytes(&self) -> anyhow::Result<Vec<u8>> {
        let complex_json = format_go_complex_result_json(&self.complex_result);
        let proof_json = serde_json::to_string(&self.proof)?;
        let error_json = match &self.error {
            Some(err) => serde_json::to_string(err)?,
            None => "null".to_string(),
        };

        let body = format!(
            r#"{{"sub_task_id":{},"node_id":{},"complex_result":{},"proof":{},"error":{}}}"#,
            serde_json::to_string(&self.sub_task_id)?,
            serde_json::to_string(&self.node_id)?,
            complex_json,
            proof_json,
            error_json,
        );
        Ok(body.into_bytes())
    }
}

/// Matches orchestrator `ComplexResult.MarshalJSON` for hash verification.
pub fn format_go_complex_result_json(value: &ComplexResult) -> String {
    format!(
        r#"{{"real":{},"imag":{}}}"#,
        format_go_float(value.real),
        format_go_float(value.imag),
    )
}

fn format_go_float(val: f64) -> String {
    if val == (val as i64) as f64 {
        format!("{:.1}", val)
    } else {
        format!("{val}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complex_result_json_matches_go_integer_style() {
        let json = format_go_complex_result_json(&ComplexResult {
            real: 0.0,
            imag: 1.0,
        });
        assert_eq!(json, r#"{"real":0.0,"imag":1.0}"#);
    }
}
