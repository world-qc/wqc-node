use crate::domain::models::{ComplexResult, ExpectationResult, Proof, SampleResult, WorkReport};

pub const PROTOCOL_RESULT: &str = "/wqc/tensor-result/1.0.0";

/// ResultMessage mirrors the orchestrator P2P result stream payload.
pub struct ResultMessage {
    pub sub_task_id: String,
    pub node_id: String,
    pub result_type: String,
    pub complex_result: ComplexResult,
    pub sample_result: Option<SampleResult>,
    pub expectation_result: Option<ExpectationResult>,
    pub proof: Proof,
    pub work_report: Option<WorkReport>,
    pub error: Option<String>,
}

impl ResultMessage {
    pub fn failure_json_bytes(
        sub_task_id: &str,
        node_id: &str,
        error: &str,
    ) -> anyhow::Result<Vec<u8>> {
        let body = format!(
            r#"{{"sub_task_id":{},"node_id":{},"error":{}}}"#,
            serde_json::to_string(sub_task_id)?,
            serde_json::to_string(node_id)?,
            serde_json::to_string(error)?,
        );
        Ok(body.into_bytes())
    }

    pub fn to_json_bytes(&self) -> anyhow::Result<Vec<u8>> {
        let complex_json = format_go_complex_result_json(&self.complex_result);
        let sample_json = match &self.sample_result {
            Some(sample) => format_go_sample_result_json(sample),
            None => "null".to_string(),
        };
        let expectation_json = match &self.expectation_result {
            Some(expectation) => format_go_expectation_result_json(expectation),
            None => "null".to_string(),
        };
        let proof_json = serde_json::to_string(&self.proof)?;
        let work_report_json = match &self.work_report {
            Some(report) => serde_json::to_string(report)?,
            None => "null".to_string(),
        };
        let error_json = match &self.error {
            Some(err) => serde_json::to_string(err)?,
            None => "null".to_string(),
        };
        let result_type_json = serde_json::to_string(&self.result_type)?;

        let body = format!(
            r#"{{"sub_task_id":{},"node_id":{},"result_type":{},"complex_result":{},"sample_result":{},"expectation_result":{},"proof":{},"work_report":{},"error":{}}}"#,
            serde_json::to_string(&self.sub_task_id)?,
            serde_json::to_string(&self.node_id)?,
            result_type_json,
            complex_json,
            sample_json,
            expectation_json,
            proof_json,
            work_report_json,
            error_json,
        );
        Ok(body.into_bytes())
    }
}

/// Matches orchestrator canonical JSON for expectation-result hashing.
pub fn format_go_expectation_result_json(value: &ExpectationResult) -> String {
    let mut pairs = String::new();
    for (id, result) in &value.values {
        if !pairs.is_empty() {
            pairs.push(',');
        }
        pairs.push_str(&format!(
            r#""{id}":{}"#,
            format_go_complex_result_json(result)
        ));
    }
    format!(r#"{{"values":{{{pairs}}}}}"#)
}

/// Matches orchestrator canonical JSON for sample-result hashing.
pub fn format_go_sample_result_json(value: &SampleResult) -> String {
    let mut counts_pairs = String::new();
    for (key, count) in &value.counts {
        if !counts_pairs.is_empty() {
            counts_pairs.push(',');
        }
        counts_pairs.push_str(&format!(r#""{key}":{count}"#));
    }
    format!(
        r#"{{"counts":{{{counts_pairs}}},"shots":{}}}"#,
        value.shots
    )
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

    #[test]
    fn expectation_result_json_matches_orchestrator_canonical() {
        let mut values = std::collections::BTreeMap::new();
        values.insert(
            "ZZ".to_string(),
            ComplexResult {
                real: 1.0,
                imag: 0.0,
            },
        );
        let json = format_go_expectation_result_json(&ExpectationResult { values });
        assert_eq!(json, r#"{"values":{"ZZ":{"real":1.0,"imag":0.0}}}"#);
    }

    #[test]
    fn failure_json_omits_proof_and_result() {
        let body = ResultMessage::failure_json_bytes("sub-1", "node-1", "timeout").unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["sub_task_id"], "sub-1");
        assert_eq!(json["node_id"], "node-1");
        assert_eq!(json["error"], "timeout");
        assert!(json.get("proof").is_none());
        assert!(json.get("complex_result").is_none());
    }
}
