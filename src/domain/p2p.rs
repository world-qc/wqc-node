use base64::{engine::general_purpose::STANDARD, Engine as _};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::Deserialize;
use serde_json;

use crate::domain::features::FEATURE_WORK_BUDGET;
use crate::domain::models::{Gate, ObservableSpec, SliceAssignment};

pub const ANNOUNCEMENT_TOPIC: &str = "wqc-global-announcements";
pub const PROTOCOL_ANNOUNCE: &str = "/wqc/task-announce/1.0.0";
pub const PROTOCOL_DISPATCH: &str = "/wqc/tensor-dispatch/1.0.0";

/// TaskAnnouncement mirrors the orchestrator signed gossip payload.
#[derive(Debug, Clone, Deserialize)]
pub struct TaskAnnouncement {
    pub task_id: String,
    pub global_qubit_count: u32,
    pub required_features: u32,
    pub bid_difficulty: u32,
    pub required_votes: u32,
    pub nonce: u64,
    /// Sub-task deadline the orchestrator enforces, from dispatch. Present
    /// with `work` only.
    #[serde(default)]
    pub deadline_secs: Option<u32>,
    /// Planner counts for one sub-task (docs `work-budget.md` §1).
    #[serde(default)]
    pub work: Option<Work>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
pub struct Work {
    pub traffic_bytes: u64,
    pub flops: u64,
}

#[derive(Debug, Deserialize)]
pub struct TaskAnnouncementMessage {
    pub announcement: TaskAnnouncement,
    pub signature: String,
    /// Covers the v1 layout plus `deadline_secs` and `work`.
    #[serde(default)]
    pub signature_v2: Option<String>,
}

/// SubTask mirrors the orchestrator dispatch payload.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct SubTask {
    pub task_id: String,
    pub parent_task_id: String,
    pub circuit_id: String,
    pub qubit_count: u32,
    pub original_qubit_count: u32,
    pub slice_id: String,
    #[serde(default)]
    pub slice_assignments: Vec<SliceAssignment>,
    #[serde(default)]
    pub circuit: Vec<Gate>,
    pub required_votes: u32,
    #[serde(default)]
    pub mps_max_bond_dim: u32,
    #[serde(default)]
    pub mps_site_order: Vec<u32>,
    /// Gate index of a scalar time split. 0 means one MPS sweep.
    #[serde(default)]
    pub mps_time_split: u32,
    /// Exact tree from the orchestrator. Absent on an MPS dispatch.
    #[serde(default)]
    pub contraction_tree: Option<crate::domain::models::ContractionTreeWire>,
    #[serde(default)]
    pub output_mode: String,
    #[serde(default)]
    pub classical_bit_count: u32,
    #[serde(default)]
    pub shots: u64,
    #[serde(default)]
    pub sample_seed: u64,
    #[serde(default)]
    pub observables: Vec<ObservableSpec>,
    #[serde(default)]
    pub noise_model: Option<crate::domain::models::NoiseModel>,
    #[serde(default)]
    pub security_level: String,
}

/// Mirrors orchestrator `bid.SerializeAnnouncementPayload` byte layout exactly.
pub fn serialize_announcement_payload(announcement: &TaskAnnouncement) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(announcement.task_id.as_bytes());
    payload.extend_from_slice(&announcement.global_qubit_count.to_be_bytes());
    payload.extend_from_slice(&announcement.required_features.to_be_bytes());
    payload.extend_from_slice(&announcement.bid_difficulty.to_be_bytes());
    payload.extend_from_slice(&announcement.required_votes.to_be_bytes());
    payload.extend_from_slice(&announcement.nonce.to_be_bytes());
    payload
}

/// The v1 layout followed by `deadline_secs`, `traffic_bytes`, and `flops`
/// (docs `work-budget.md` §3). `None` without `work` or `deadline_secs`.
pub fn serialize_announcement_payload_v2(announcement: &TaskAnnouncement) -> Option<Vec<u8>> {
    let work = announcement.work?;
    let mut payload = serialize_announcement_payload(announcement);
    payload.extend_from_slice(&announcement.deadline_secs?.to_be_bytes());
    payload.extend_from_slice(&work.traffic_bytes.to_be_bytes());
    payload.extend_from_slice(&work.flops.to_be_bytes());
    Some(payload)
}

pub fn parse_signed_announcement(
    payload: &[u8],
    orchestrator_public_key_b64: &str,
) -> Result<TaskAnnouncement, String> {
    let message: TaskAnnouncementMessage =
        serde_json::from_slice(payload).map_err(|e| format!("invalid announcement JSON: {e}"))?;
    verify_announcement_message(&message, orchestrator_public_key_b64)?;
    Ok(message.announcement)
}

pub fn verify_announcement_message(
    message: &TaskAnnouncementMessage,
    orchestrator_public_key_b64: &str,
) -> Result<(), String> {
    // Bit 4 means the task has work. The v1 signature does not cover `work`,
    // so a relay could strip it; such an announcement is dropped.
    if message.announcement.required_features & FEATURE_WORK_BUDGET != 0
        && (message.announcement.work.is_none() || message.announcement.deadline_secs.is_none())
    {
        return Err("announcement asks for the work budget without work".to_string());
    }
    // `work` is trusted only under the v2 signature, which also covers every
    // v1 field.
    if message.announcement.work.is_some() {
        let signed_payload = serialize_announcement_payload_v2(&message.announcement)
            .ok_or_else(|| "announcement work has no deadline".to_string())?;
        let signature = message
            .signature_v2
            .as_deref()
            .ok_or_else(|| "announcement work is not signed".to_string())?;
        return verify_orchestrator_signature(
            &signed_payload,
            signature,
            orchestrator_public_key_b64,
            "announcement v2",
        );
    }
    let signed_payload = serialize_announcement_payload(&message.announcement);
    verify_orchestrator_signature(
        &signed_payload,
        &message.signature,
        orchestrator_public_key_b64,
        "announcement",
    )
}

#[derive(Debug, Deserialize)]
pub struct TaskDispatchMessage {
    pub sub_task: SubTask,
    pub signature: String,
}

/// Mirrors orchestrator `task.SerializeDispatchPayload` byte layout exactly.
pub fn serialize_dispatch_payload(sub_task: &SubTask) -> Result<Vec<u8>, String> {
    let mut payload = Vec::new();
    payload.extend_from_slice(sub_task.task_id.as_bytes());
    payload.extend_from_slice(sub_task.parent_task_id.as_bytes());
    payload.extend_from_slice(sub_task.circuit_id.as_bytes());
    payload.extend_from_slice(sub_task.slice_id.as_bytes());
    payload.extend_from_slice(&sub_task.qubit_count.to_be_bytes());
    payload.extend_from_slice(&sub_task.original_qubit_count.to_be_bytes());
    payload.extend_from_slice(&sub_task.required_votes.to_be_bytes());

    let assignments = &sub_task.slice_assignments;
    payload.extend_from_slice(&(assignments.len() as u32).to_be_bytes());
    for assignment in assignments {
        payload.extend_from_slice(assignment.edge_id.as_bytes());
        payload.push(assignment.value);
    }

    let circuit_json = circuit_signing_json(&sub_task.circuit)?;
    payload.extend_from_slice(&(circuit_json.len() as u32).to_be_bytes());
    payload.extend_from_slice(&circuit_json);
    payload.extend_from_slice(&sub_task.mps_max_bond_dim.to_be_bytes());

    let output_mode = sub_task.output_mode.as_bytes();
    payload.extend_from_slice(&(output_mode.len() as u32).to_be_bytes());
    payload.extend_from_slice(output_mode);
    payload.extend_from_slice(&sub_task.shots.to_be_bytes());
    payload.extend_from_slice(&sub_task.classical_bit_count.to_be_bytes());
    payload.extend_from_slice(&sub_task.sample_seed.to_be_bytes());

    let observables_json = format_observables_wire_json(&sub_task.observables);
    payload.extend_from_slice(&(observables_json.len() as u32).to_be_bytes());
    payload.extend_from_slice(observables_json.as_bytes());

    let site_order = &sub_task.mps_site_order;
    payload.extend_from_slice(&(site_order.len() as u32).to_be_bytes());
    for q in site_order {
        payload.extend_from_slice(&q.to_be_bytes());
    }

    let security_level = sub_task.security_level.as_bytes();
    payload.extend_from_slice(&(security_level.len() as u32).to_be_bytes());
    payload.extend_from_slice(security_level);
    // Match orchestrator: only present when a time split is set, so ordinary
    // dispatches stay byte-identical to the pre-time-split wire.
    if sub_task.mps_time_split != 0 {
        payload.extend_from_slice(&sub_task.mps_time_split.to_be_bytes());
    }
    // Present only when the orchestrator attached a tree, so an MPS dispatch
    // stays byte-identical and an old node rejects the tree signature.
    if let Some(tree) = &sub_task.contraction_tree {
        let tree_json = serde_json::to_vec(tree)
            .map_err(|e| format!("marshal contraction tree for dispatch signing: {e}"))?;
        payload.extend_from_slice(&(tree_json.len() as u32).to_be_bytes());
        payload.extend_from_slice(&tree_json);
    }

    Ok(payload)
}

/// Circuit JSON as the orchestrator signs it: `[{"type":…,"params":…}]` with
/// numbers and strings written the way Go `encoding/json` writes them.
fn circuit_signing_json(circuit: &[Gate]) -> Result<Vec<u8>, String> {
    let mut out = String::from("[");
    for (i, gate) in circuit.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(r#"{"type":"#);
        write_go_json_string(&mut out, &gate.r#type);
        out.push_str(r#","params":"#);
        write_go_json_value(&mut out, &gate.params)?;
        out.push('}');
    }
    out.push(']');
    Ok(out.into_bytes())
}

/// Go decodes params into `any`, so every number is a float64 and object keys
/// come out sorted. serde_json's default map is sorted the same way.
fn write_go_json_value(out: &mut String, value: &serde_json::Value) -> Result<(), String> {
    use serde_json::Value;
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => match n.as_f64() {
            Some(f) => out.push_str(&go_json_float(f)),
            None => return Err(format!("circuit number {n} is not a float64")),
        },
        Value::String(text) => write_go_json_string(out, text),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_go_json_value(out, item)?;
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, (key, item)) in map.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_go_json_string(out, key);
                out.push(':');
                write_go_json_value(out, item)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

/// Go `encoding/json` string with HTML escaping: `<`, `>`, `&`, U+2028 and
/// U+2029 become `\u` escapes; `\b \f \n \r \t` keep their short form.
fn write_go_json_string(out: &mut String, text: &str) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '<' | '>' | '&' | '\u{2028}' | '\u{2029}' => {
                out.push_str(&format!("\\u{:04x}", ch as u32));
            }
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Go `encoding/json` float64. Ryu picks the shortest digits with the same
/// tie rule as Go (Rust's Display rounds ties away from zero, so
/// 1533979136307259.25 would end in 3 instead of Go's 2). The layout follows
/// Go: plain decimal unless |f| < 1e-6 or |f| >= 1e21, then `1e-7` / `1e+21`.
fn go_json_float(f: f64) -> String {
    if f == 0.0 {
        return if f.is_sign_negative() { "-0" } else { "0" }.to_string();
    }
    let mut buffer = ryu::Buffer::new();
    let (sign, digits, exp) = decimal_digits(buffer.format_finite(f));
    let abs = f.abs();
    let body = if (1e-6..1e21).contains(&abs) {
        plain_decimal(&digits, exp)
    } else {
        let mantissa = match digits.len() {
            1 => digits.clone(),
            _ => format!("{}.{}", &digits[..1], &digits[1..]),
        };
        if exp < 0 {
            format!("{mantissa}e-{}", -exp)
        } else {
            format!("{mantissa}e+{exp:02}")
        }
    };
    format!("{sign}{body}")
}

/// Splits ryu output (`1.15`, `1e20`, `2.4e-6`) into sign, significant
/// digits without leading or trailing zeros, and the exponent of the first
/// digit, so the value is `d.ddd × 10^exp`.
fn decimal_digits(text: &str) -> (&'static str, String, i32) {
    let (sign, text) = match text.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", text),
    };
    let (mantissa, exp_part) = text.split_once('e').unwrap_or((text, "0"));
    let exp_part: i32 = exp_part.parse().unwrap_or(0);
    let point = mantissa.find('.').unwrap_or(mantissa.len()) as i32;
    let all: String = mantissa.chars().filter(|c| *c != '.').collect();
    let leading = all.len() - all.trim_start_matches('0').len();
    let digits = all
        .trim_start_matches('0')
        .trim_end_matches('0')
        .to_string();
    (sign, digits, point - 1 - leading as i32 + exp_part)
}

/// `d.ddd × 10^exp` written without an exponent.
fn plain_decimal(digits: &str, exp: i32) -> String {
    if exp < 0 {
        let zeros = "0".repeat((-exp - 1) as usize);
        return format!("0.{zeros}{digits}");
    }
    let int_len = exp as usize + 1;
    if digits.len() <= int_len {
        format!("{digits}{}", "0".repeat(int_len - digits.len()))
    } else {
        format!("{}.{}", &digits[..int_len], &digits[int_len..])
    }
}

/// Canonical observables JSON for dispatch signing (matches orchestrator `FormatObservablesWireJSON`).
fn format_observables_wire_json(observables: &[ObservableSpec]) -> String {
    if observables.is_empty() {
        return "[]".to_string();
    }
    let inner = format_observable_spec_json(observables);
    // Strip `{"observables":` prefix and trailing `}`.
    inner
        .strip_prefix(r#"{"observables":"#)
        .and_then(|s| s.strip_suffix('}'))
        .unwrap_or(&inner)
        .to_string()
}

fn format_observable_spec_json(observables: &[ObservableSpec]) -> String {
    let mut sorted: Vec<&ObservableSpec> = observables.iter().collect();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));

    let mut obs_parts = Vec::new();
    for obs in sorted {
        let mut terms = obs.terms.clone();
        terms.sort_by(|a, b| a.label.cmp(&b.label));
        let term_parts: Vec<String> = terms
            .iter()
            .map(|t| {
                format!(
                    r#"{{"coeff":{},"label":"{}"}}"#,
                    format_complex_coeff_json(&t.coeff),
                    t.label
                )
            })
            .collect();
        obs_parts.push(format!(
            r#"{{"id":"{}","terms":[{}]}}"#,
            obs.id,
            term_parts.join(",")
        ));
    }
    format!(r#"{{"observables":[{}]}}"#, obs_parts.join(","))
}

fn format_complex_coeff_json(coeff: &crate::domain::models::ComplexCoeff) -> String {
    format!(
        r#"{{"imag":{},"real":{}}}"#,
        format_go_float(coeff.imag),
        format_go_float(coeff.real)
    )
}

fn format_go_float(val: f64) -> String {
    if val == (val as i64) as f64 {
        format!("{:.1}", val)
    } else {
        format!("{val}")
    }
}

pub fn verify_dispatch_signature(
    sub_task: &SubTask,
    signature_b64: &str,
    orchestrator_public_key_b64: &str,
) -> Result<(), String> {
    let payload = serialize_dispatch_payload(sub_task)?;
    verify_orchestrator_signature(
        &payload,
        signature_b64,
        orchestrator_public_key_b64,
        "dispatch",
    )
}

pub(crate) fn verify_orchestrator_signature(
    payload: &[u8],
    signature_b64: &str,
    orchestrator_public_key_b64: &str,
    label: &str,
) -> Result<(), String> {
    let signature_bytes = STANDARD
        .decode(signature_b64.trim())
        .map_err(|e| format!("invalid {label} signature base64: {e}"))?;
    let signature = Signature::from_slice(&signature_bytes)
        .map_err(|e| format!("invalid {label} signature bytes: {e}"))?;

    let public_key_bytes = STANDARD
        .decode(orchestrator_public_key_b64.trim())
        .map_err(|e| format!("invalid orchestrator public key base64: {e}"))?;
    let verifying_key = VerifyingKey::from_bytes(
        public_key_bytes
            .as_slice()
            .try_into()
            .map_err(|_| "orchestrator public key must be 32 bytes".to_string())?,
    )
    .map_err(|e| format!("invalid orchestrator public key: {e}"))?;

    verifying_key
        .verify(payload, &signature)
        .map_err(|_| format!("{label} signature verification failed"))
}

impl SubTask {
    pub fn into_compute_request(self, peer_id: &str) -> crate::domain::models::ComputeRequest {
        crate::domain::models::ComputeRequest {
            task_id: self.task_id,
            parent_task_id: Some(self.parent_task_id),
            circuit_id: self.circuit_id,
            node_id: Some(peer_id.to_string()),
            qubit_count: self.qubit_count as usize,
            original_qubit_count: self.original_qubit_count as usize,
            slice_id: self.slice_id,
            slice_assignments: self.slice_assignments,
            circuit: self.circuit,
            required_votes: Some(self.required_votes),
            mps_max_bond_dim: if self.mps_max_bond_dim > 0 {
                Some(self.mps_max_bond_dim as usize)
            } else {
                None
            },
            mps_site_order: if self.mps_site_order.is_empty() {
                None
            } else {
                Some(self.mps_site_order.iter().map(|&q| q as usize).collect())
            },
            mps_time_split: if self.mps_time_split > 0 {
                Some(self.mps_time_split as usize)
            } else {
                None
            },
            contraction_tree: self.contraction_tree,
            output_mode: self.output_mode.clone(),
            classical_bit_count: if self.classical_bit_count > 0 {
                Some(self.classical_bit_count)
            } else {
                None
            },
            shots: if self.shots > 0 {
                Some(self.shots)
            } else {
                None
            },
            sample_seed: if self.sample_seed > 0 {
                Some(self.sample_seed)
            } else {
                None
            },
            observables: self.observables,
            noise_model: self.noise_model,
            security_level: self.security_level,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::models::Gate;
    use crate::domain::models::{ContractNodeWire, ContractionTreeWire, TensorRefWire};

    #[test]
    fn serialize_announcement_payload_layout_matches_orchestrator() {
        let announcement = TaskAnnouncement {
            task_id: "task-parent-1".to_string(),
            global_qubit_count: 30,
            required_features: 3,
            bid_difficulty: 2,
            required_votes: 2,
            nonce: 42,
            deadline_secs: None,
            work: None,
        };

        let payload = serialize_announcement_payload(&announcement);
        let mut expected = Vec::new();
        expected.extend_from_slice(b"task-parent-1");
        expected.extend_from_slice(&30u32.to_be_bytes());
        expected.extend_from_slice(&3u32.to_be_bytes());
        expected.extend_from_slice(&2u32.to_be_bytes());
        expected.extend_from_slice(&2u32.to_be_bytes());
        expected.extend_from_slice(&42u64.to_be_bytes());
        assert_eq!(payload, expected);
    }

    const GOLDEN_V2: &str = "74000000640000001b000000010000000200000000000000090000012c000001000000000000000008d8f9fc00";

    #[test]
    fn announcement_work_needs_the_v2_signature() {
        use ed25519_dalek::{Signer, SigningKey};
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let public = STANDARD.encode(key.verifying_key().to_bytes());
        let sign = |bytes: &[u8]| STANDARD.encode(key.sign(bytes).to_bytes());
        let announcement = TaskAnnouncement {
            task_id: "t".to_string(),
            global_qubit_count: 100,
            required_features: 27,
            bid_difficulty: 1,
            required_votes: 2,
            nonce: 9,
            deadline_secs: Some(300),
            work: Some(Work {
                traffic_bytes: 1 << 40,
                flops: 38_000_000_000,
            }),
        };
        let v1 = sign(&serialize_announcement_payload(&announcement));
        let v2 = sign(&serialize_announcement_payload_v2(&announcement).unwrap());
        let message =
            |announcement: TaskAnnouncement, v2: Option<String>| TaskAnnouncementMessage {
                announcement,
                signature: v1.clone(),
                signature_v2: v2,
            };

        // The v2 layout appends deadline, traffic, and flops, big-endian. The
        // orchestrator's TestSerializeAnnouncementPayloadV2AppendsWork checks
        // the same bytes.
        let expected: Vec<u8> = (0..GOLDEN_V2.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&GOLDEN_V2[i..i + 2], 16).unwrap())
            .collect();
        assert_eq!(
            serialize_announcement_payload_v2(&announcement).unwrap(),
            expected
        );

        assert!(verify_announcement_message(
            &message(announcement.clone(), Some(v2.clone())),
            &public
        )
        .is_ok());
        // v1 alone does not vouch for work.
        let err =
            verify_announcement_message(&message(announcement.clone(), None), &public).unwrap_err();
        assert!(err.contains("not signed"), "{err}");
        // A changed work count fails.
        let mut lighter = announcement.clone();
        lighter.work = Some(Work {
            traffic_bytes: 1,
            flops: 1,
        });
        assert!(verify_announcement_message(&message(lighter, Some(v2)), &public).is_err());
        // Stripping work and signature_v2 leaves a valid v1 signature, but
        // bit 4 says work belongs there.
        let mut stripped = announcement.clone();
        stripped.work = None;
        stripped.deadline_secs = None;
        let err = verify_announcement_message(&message(stripped, None), &public).unwrap_err();
        assert!(err.contains("without work"), "{err}");
        // Without work or bit 4, v1 is checked as before.
        let mut plain = announcement;
        plain.work = None;
        plain.deadline_secs = None;
        plain.required_features &= !FEATURE_WORK_BUDGET;
        let v1_plain = sign(&serialize_announcement_payload(&plain));
        let ok = TaskAnnouncementMessage {
            announcement: plain,
            signature: v1_plain,
            signature_v2: None,
        };
        assert!(verify_announcement_message(&ok, &public).is_ok());
    }

    #[test]
    fn serialize_dispatch_payload_layout_matches_orchestrator() {
        let sub_task = SubTask {
            task_id: "sub-task-1".to_string(),
            parent_task_id: "parent-uuid".to_string(),
            circuit_id: "abc123".to_string(),
            qubit_count: 2,
            original_qubit_count: 3,
            slice_id: "01".to_string(),
            slice_assignments: vec![SliceAssignment {
                edge_id: "e_0".to_string(),
                value: 1,
            }],
            circuit: vec![Gate {
                r#type: "H".to_string(),
                params: serde_json::json!([0]),
            }],
            required_votes: 2,
            mps_max_bond_dim: 128,
            ..Default::default()
        };

        let payload = serialize_dispatch_payload(&sub_task).expect("serialize dispatch payload");
        let mut expected = Vec::new();
        expected.extend_from_slice(b"sub-task-1");
        expected.extend_from_slice(b"parent-uuid");
        expected.extend_from_slice(b"abc123");
        expected.extend_from_slice(b"01");
        expected.extend_from_slice(&2u32.to_be_bytes());
        expected.extend_from_slice(&3u32.to_be_bytes());
        expected.extend_from_slice(&2u32.to_be_bytes());
        expected.extend_from_slice(&1u32.to_be_bytes());
        expected.extend_from_slice(b"e_0");
        expected.push(1);
        let circuit_json = br#"[{"type":"H","params":[0]}]"#;
        expected.extend_from_slice(&(circuit_json.len() as u32).to_be_bytes());
        expected.extend_from_slice(circuit_json);
        expected.extend_from_slice(&128u32.to_be_bytes());
        expected.extend_from_slice(&0u32.to_be_bytes()); // output_mode len
        expected.extend_from_slice(&0u64.to_be_bytes()); // shots
        expected.extend_from_slice(&0u32.to_be_bytes()); // classical_bit_count
        expected.extend_from_slice(&0u64.to_be_bytes()); // sample_seed
        expected.extend_from_slice(&2u32.to_be_bytes()); // observables_json_len
        expected.extend_from_slice(b"[]"); // observables_json
        expected.extend_from_slice(&0u32.to_be_bytes()); // mps_site_order_len
        expected.extend_from_slice(&0u32.to_be_bytes()); // security_level_len
        assert_eq!(payload, expected);
    }

    #[test]
    fn go_json_float_matches_go_encoding_json() {
        // Expected strings are Go `json.Marshal(float64)` output.
        let cases: &[(f64, &str)] = &[
            (1.1500000000000001, "1.1500000000000001"),
            (-0.41000000000000003, "-0.41000000000000003"),
            (2.4082865570993347e-6, "0.0000024082865570993347"),
            (1e-6, "0.000001"),
            (1e-7, "1e-7"),
            (5e-324, "5e-324"),
            (1e20, "100000000000000000000"),
            (1e21, "1e+21"),
            (1.5e300, "1.5e+300"),
            (-0.0, "-0"),
            (std::f64::consts::FRAC_PI_8, "0.39269908169872414"),
            (3.0, "3"),
            // 1533979136307259.25 is exact and halfway between two shortest
            // forms; Go keeps the even digit. Built from parts because the
            // literal trips clippy::excessive_precision.
            (1_533_979_136_307_259.0 + 0.25, "1533979136307259.2"),
            (-(1_533_979_136_307_259.0 + 0.25), "-1533979136307259.2"),
            (123456789012345680000.0, "123456789012345680000"),
            (1.5e-300, "1.5e-300"),
        ];
        for (value, expected) in cases {
            assert_eq!(go_json_float(*value), *expected, "value {value:e}");
        }
    }

    #[test]
    fn circuit_signing_json_keeps_the_orchestrator_bytes() {
        // Without float_roundtrip, 1.1500000000000001 parsed as 1.15 and the
        // signature over this circuit failed on every node.
        let signed = concat!(
            r#"[{"type":"RY","params":[17,1.1500000000000001]},"#,
            r#"{"type":"RZ","params":[0,0.0000024082865570993347]},"#,
            r#"{"type":"MEASURE","params":{"cbit":0,"qubit":1}},"#,
            r#"{"type":"IF","params":{"cbit":2,"gate":{"params":[0],"type":"X"},"value":1}},"#,
            r#"{"type":"H","params":[3]}]"#,
        );
        let circuit: Vec<Gate> = serde_json::from_str(signed).expect("parse circuit");
        let bytes = circuit_signing_json(&circuit).expect("signing json");
        assert_eq!(String::from_utf8(bytes).unwrap(), signed);
        // The canonical form the orchestrator dispatches: one operand as a bare value.
        let canonical = concat!(
            r#"[{"type":"H","params":0},{"type":"X","params":2},"#,
            r#"{"type":"IF","params":{"cbit":0,"gate":{"params":1,"type":"X"},"value":1}}]"#,
        );
        let circuit: Vec<Gate> = serde_json::from_str(canonical).expect("parse circuit");
        let bytes = circuit_signing_json(&circuit).expect("signing json");
        assert_eq!(String::from_utf8(bytes).unwrap(), canonical);
    }

    #[test]
    fn go_json_string_matches_go_encoding_json() {
        // Expected string is Go 1.26 `json.Marshal` output.
        let mut out = String::new();
        write_go_json_string(
            &mut out,
            "a<b>&c\u{8}\u{c}\n\r\t\u{1}\u{1f}\"\\ \u{2028}\u{2029} 日本",
        );
        assert_eq!(
            out,
            r#""a\u003cb\u003e\u0026c\b\f\n\r\t\u0001\u001f\"\\ \u2028\u2029 日本""#
        );
    }

    #[test]
    fn serialize_dispatch_payload_includes_observables() {
        let sub_task = SubTask {
            task_id: "sub-task-1".to_string(),
            parent_task_id: "parent-uuid".to_string(),
            circuit_id: "abc123".to_string(),
            qubit_count: 2,
            original_qubit_count: 2,
            slice_id: "0".to_string(),
            slice_assignments: vec![],
            circuit: vec![Gate {
                r#type: "H".to_string(),
                params: serde_json::json!([0]),
            }],
            required_votes: 2,
            output_mode: "expectation".to_string(),
            observables: vec![crate::domain::models::ObservableSpec {
                id: "ZZ".to_string(),
                terms: vec![crate::domain::models::PauliTerm {
                    label: "ZZ".to_string(),
                    coeff: crate::domain::models::ComplexCoeff {
                        real: 1.0,
                        imag: 0.0,
                    },
                }],
            }],
            ..Default::default()
        };

        let payload = serialize_dispatch_payload(&sub_task).expect("serialize");
        let obs_json = br#"[{"id":"ZZ","terms":[{"coeff":{"imag":0.0,"real":1.0},"label":"ZZ"}]}]"#;
        let mut tail = Vec::new();
        tail.extend_from_slice(&(obs_json.len() as u32).to_be_bytes());
        tail.extend_from_slice(obs_json);
        tail.extend_from_slice(&0u32.to_be_bytes()); // mps_site_order_len
        tail.extend_from_slice(&0u32.to_be_bytes()); // security_level_len
        assert!(payload.ends_with(&tail));
    }

    #[test]
    fn serialize_dispatch_payload_includes_site_order() {
        let sub_task = SubTask {
            task_id: "s".to_string(),
            parent_task_id: "p".to_string(),
            circuit_id: "c".to_string(),
            qubit_count: 2,
            original_qubit_count: 2,
            slice_id: "0".to_string(),
            mps_site_order: vec![1, 0],
            ..Default::default()
        };
        let payload = serialize_dispatch_payload(&sub_task).expect("serialize");
        let mut want_suffix = Vec::new();
        want_suffix.extend_from_slice(&2u32.to_be_bytes());
        want_suffix.extend_from_slice(b"[]");
        want_suffix.extend_from_slice(&2u32.to_be_bytes());
        want_suffix.extend_from_slice(&1u32.to_be_bytes());
        want_suffix.extend_from_slice(&0u32.to_be_bytes());
        want_suffix.extend_from_slice(&0u32.to_be_bytes()); // security_level_len
        assert!(payload.ends_with(&want_suffix));
    }

    #[test]
    fn serialize_dispatch_payload_includes_security_level() {
        let sub_task = SubTask {
            task_id: "s".to_string(),
            parent_task_id: "p".to_string(),
            circuit_id: "c".to_string(),
            qubit_count: 2,
            original_qubit_count: 2,
            slice_id: "0".to_string(),
            security_level: "low".to_string(),
            ..Default::default()
        };
        let payload = serialize_dispatch_payload(&sub_task).expect("serialize");
        let mut want_suffix = Vec::new();
        want_suffix.extend_from_slice(&3u32.to_be_bytes());
        want_suffix.extend_from_slice(b"low");
        assert!(payload.ends_with(&want_suffix));
    }

    #[test]
    fn serialize_dispatch_payload_includes_time_split_when_set() {
        let sub_task = SubTask {
            task_id: "s".to_string(),
            parent_task_id: "p".to_string(),
            circuit_id: "c".to_string(),
            qubit_count: 2,
            original_qubit_count: 2,
            slice_id: "0".to_string(),
            mps_time_split: 196,
            ..Default::default()
        };
        let payload = serialize_dispatch_payload(&sub_task).expect("serialize");
        let mut want_suffix = Vec::new();
        want_suffix.extend_from_slice(&0u32.to_be_bytes()); // security_level_len
        want_suffix.extend_from_slice(&196u32.to_be_bytes());
        assert!(payload.ends_with(&want_suffix));
    }

    #[test]
    fn into_compute_request_maps_time_split_zero_to_none() {
        let req = SubTask {
            task_id: "s".into(),
            parent_task_id: "p".into(),
            circuit_id: "c".into(),
            qubit_count: 2,
            original_qubit_count: 2,
            slice_id: "0".into(),
            mps_time_split: 0,
            ..Default::default()
        }
        .into_compute_request("peer");
        assert_eq!(req.mps_time_split, None);
    }

    #[test]
    fn into_compute_request_maps_time_split_to_some() {
        let req = SubTask {
            task_id: "s".into(),
            parent_task_id: "p".into(),
            circuit_id: "c".into(),
            qubit_count: 2,
            original_qubit_count: 2,
            slice_id: "0".into(),
            mps_time_split: 196,
            ..Default::default()
        }
        .into_compute_request("peer");
        assert_eq!(req.mps_time_split, Some(196));
    }

    #[test]
    fn sub_task_json_reads_with_and_without_time_split() {
        let bare: SubTask = serde_json::from_str(
            r#"{"task_id":"s","parent_task_id":"p","circuit_id":"c","qubit_count":2,"original_qubit_count":2,"slice_id":"0","required_votes":1}"#,
        )
        .expect("bare");
        assert_eq!(bare.mps_time_split, 0);

        let split: SubTask = serde_json::from_str(
            r#"{"task_id":"s","parent_task_id":"p","circuit_id":"c","qubit_count":2,"original_qubit_count":2,"slice_id":"0","required_votes":1,"mps_time_split":196}"#,
        )
        .expect("split");
        assert_eq!(split.mps_time_split, 196);
    }

    #[test]
    fn serialize_dispatch_payload_appends_tree_and_leaves_mps_bytes_unchanged() {
        let bare = SubTask {
            task_id: "s".into(),
            parent_task_id: "p".into(),
            circuit_id: "c".into(),
            qubit_count: 1,
            original_qubit_count: 1,
            slice_id: "0".into(),
            ..Default::default()
        };
        let plain = serialize_dispatch_payload(&bare).expect("plain");
        let tree_json =
            r#"{"nodes":[{"op":"contract","left":{"ket0":0},"right":{"gate":0}}],"peak_bytes":32}"#;
        let with_tree = SubTask {
            contraction_tree: Some(ContractionTreeWire {
                kind: None,
                nodes: vec![ContractNodeWire {
                    op: "contract".into(),
                    left: TensorRefWire {
                        ket0: Some(0),
                        ..Default::default()
                    },
                    right: TensorRefWire {
                        gate: Some(0),
                        ..Default::default()
                    },
                }],
                peak_bytes: 32,
            }),
            ..bare
        };
        let signed = serialize_dispatch_payload(&with_tree).expect("tree");
        assert!(signed.starts_with(&plain));
        assert_ne!(signed, plain);
        let json = serde_json::to_vec(with_tree.contraction_tree.as_ref().unwrap()).unwrap();
        assert_eq!(json, tree_json.as_bytes());
        let mut suffix = (json.len() as u32).to_be_bytes().to_vec();
        suffix.extend_from_slice(&json);
        assert!(signed.ends_with(&suffix));
        let forwarded = with_tree.into_compute_request("peer");
        assert_eq!(
            forwarded.contraction_tree.unwrap().nodes.len(),
            1,
            "the node forwards the orchestrator's tree"
        );
    }

    /// The orchestrator's network tree for one `H`
    /// (`TestSerializeDispatchPayloadSignsTheNetworkTreeJSON`). The node must
    /// re-encode it byte for byte, or the signature over it fails.
    #[test]
    fn network_tree_round_trips_byte_for_byte() {
        let tree_json = r#"{"kind":"network","nodes":[{"op":"contract","left":{"ket0":0},"right":{"gate":0}},{"op":"contract","left":{"bra0":0},"right":{"node":0}}],"peak_bytes":128}"#;
        let tree: ContractionTreeWire = serde_json::from_str(tree_json).expect("parse");
        assert_eq!(tree.kind.as_deref(), Some("network"));
        assert_eq!(tree.nodes[1].left.bra0, Some(0));
        assert_eq!(serde_json::to_string(&tree).unwrap(), tree_json);

        let sub = SubTask {
            task_id: "s".into(),
            parent_task_id: "p".into(),
            circuit_id: "c".into(),
            qubit_count: 1,
            original_qubit_count: 1,
            slice_id: "0".into(),
            contraction_tree: Some(tree),
            ..Default::default()
        };
        let signed = serialize_dispatch_payload(&sub).expect("tree");
        let mut suffix = (tree_json.len() as u32).to_be_bytes().to_vec();
        suffix.extend_from_slice(tree_json.as_bytes());
        assert!(signed.ends_with(&suffix));
        let forwarded = sub.into_compute_request("peer");
        let body = serde_json::to_value(&forwarded).expect("request");
        assert_eq!(body["contraction_tree"]["kind"], "network");
        assert_eq!(body["contraction_tree"]["nodes"][1]["left"]["bra0"], 0);
    }

    #[test]
    fn gate_halves_round_trip_byte_for_byte() {
        // The orchestrator's TestSerializeDispatchPayloadSignsGateHalves text.
        let tree_json = r#"{"kind":"network","nodes":[{"op":"contract","left":{"control":0},"right":{"target":0}},{"op":"contract","left":{"node":0},"right":{"node":0}}],"peak_bytes":384}"#;
        let tree: ContractionTreeWire = serde_json::from_str(tree_json).expect("parse");
        assert_eq!(tree.nodes[0].left.control, Some(0));
        assert_eq!(tree.nodes[0].right.target, Some(0));
        assert_eq!(serde_json::to_string(&tree).unwrap(), tree_json);
        let sub = SubTask {
            task_id: "s".into(),
            parent_task_id: "p".into(),
            circuit_id: "c".into(),
            qubit_count: 2,
            original_qubit_count: 2,
            slice_id: "0".into(),
            contraction_tree: Some(tree),
            ..Default::default()
        };
        let signed = serialize_dispatch_payload(&sub).expect("tree");
        let mut suffix = (tree_json.len() as u32).to_be_bytes().to_vec();
        suffix.extend_from_slice(tree_json.as_bytes());
        assert!(signed.ends_with(&suffix));
        let body = serde_json::to_value(sub.into_compute_request("peer")).expect("request");
        assert_eq!(body["contraction_tree"]["nodes"][0]["left"]["control"], 0);
        assert_eq!(body["contraction_tree"]["nodes"][0]["right"]["target"], 0);
    }
}
