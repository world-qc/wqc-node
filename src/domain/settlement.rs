//! Checks a task settled on SettlementV4 against the evidence the orchestrator publishes
//! (wqc-docs `spec/zk-SNARK.md` §11.6). Pure: the caller fetches the evidence and the
//! contract's stored task, and recomputes the wrap digest.

use std::collections::BTreeMap;

use alloy::primitives::{keccak256, Address, U256};
use alloy::sol;
use alloy::sol_types::SolValue;
use base64::Engine as _;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use sha3::Sha3_256;

sol! {
    /// SettlementV4's payout row; `payoutsHash = keccak256(abi.encode(payouts))`.
    struct Payout {
        address to;
        uint256 amount;
    }
}

/// SettlementV4 task status.
pub const STATUS_SETTLED_COMMITTED: u8 = 2;

/// What `SettlementV4.tasks(taskId)` stores for a task (the fields the challenger checks).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnChainTask {
    pub status: u8,
    pub root_hash: [u8; 32],
    pub receipt_hash: [u8; 32],
    pub wrap_digest: [u8; 32],
    pub payouts_hash: [u8; 32],
    pub burn_amount: U256,
    pub refund_amount: U256,
    pub deadline: u64,
}

/// A payout binding as `GET /api/v1/economy/payout/{operator_id}` returns it.
#[derive(Debug, Clone, Deserialize)]
pub struct Binding {
    pub operator_id: String,
    pub chain_id: u64,
    pub payout_address: String,
    pub nonce: u64,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub operator_pubkey_b64: String,
    #[serde(default)]
    pub signature_b64: String,
}

/// A leaf this node proved, as it reported it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnLeaf {
    pub sub_task_id: String,
    pub node_id: String,
    pub sv_digest: [u64; 4],
}

/// The published evidence of one task.
pub struct Evidence<'a> {
    /// The receipt exactly as published (`receipt_hash` empty).
    pub receipt: &'a [u8],
    /// Each payee's binding, by operator id.
    pub bindings: &'a BTreeMap<String, Binding>,
    pub root: &'a [u8],
    /// The leaves JSON the root and the wrap chain were verified over.
    pub leaves_json: &'a [u8],
}

#[derive(Debug, Deserialize)]
struct Receipt {
    burn_settled_pwqc: String,
    escrow_refunded_pwqc: String,
    #[serde(default)]
    payees: Vec<Payee>,
}

#[derive(Debug, Deserialize)]
struct Payee {
    operator_id: String,
    amount_pwqc: String,
    /// The payout binding fixed at finalize; absent in receipts from before.
    #[serde(default)]
    payout_address: String,
    #[serde(default)]
    chain_id: u64,
    #[serde(default)]
    nonce: u64,
    #[serde(default)]
    operator_pubkey_b64: String,
    #[serde(default)]
    signature_b64: String,
}

impl Payee {
    fn binding(&self) -> Option<Binding> {
        (!self.payout_address.is_empty()).then(|| Binding {
            operator_id: self.operator_id.clone(),
            chain_id: self.chain_id,
            payout_address: self.payout_address.clone(),
            nonce: self.nonce,
            status: String::new(),
            operator_pubkey_b64: self.operator_pubkey_b64.clone(),
            signature_b64: self.signature_b64.clone(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct Leaves {
    leaves: Vec<LeafRef>,
}

#[derive(Debug, Deserialize)]
struct LeafRef {
    sub_task_id: String,
    node_id: String,
    digest: [u64; 4],
}

/// The payees whose binding the receipt does not carry (receipts from before bindings were
/// fixed at finalize), so the caller can fetch theirs from the registry.
pub fn payees_without_binding(receipt: &[u8]) -> Result<Vec<String>, String> {
    let r: Receipt = serde_json::from_slice(receipt).map_err(|e| format!("receipt JSON: {e}"))?;
    Ok(r.payees
        .into_iter()
        .filter(|p| p.payout_address.is_empty())
        .map(|p| p.operator_id)
        .collect())
}

/// Checks the settled task against the evidence: `wrap_digest` is the digest recomputed from
/// the evidence's leaves, root and wrap chain, or `None` when this build cannot recompute it.
/// Returns every failed check; an empty list means the settlement matches the evidence.
pub fn check(
    task: &OnChainTask,
    ev: &Evidence<'_>,
    chain_id: u64,
    own_operator: Option<&str>,
    own_leaves: &[OwnLeaf],
    wrap_digest: Option<[u64; 4]>,
) -> Vec<String> {
    let mut failed = Vec::new();

    // 1. receiptHash over the published bytes.
    let receipt_hash: [u8; 32] = Sha3_256::digest(ev.receipt).into();
    if receipt_hash != task.receipt_hash {
        failed.push(format!(
            "receiptHash 0x{} is not the receipt's SHA3-256 0x{}",
            hex::encode(task.receipt_hash),
            hex::encode(receipt_hash)
        ));
    }
    let receipt: Receipt = match serde_json::from_slice(ev.receipt) {
        Ok(r) => r,
        Err(e) => {
            failed.push(format!("receipt JSON: {e}"));
            return failed;
        }
    };

    // 2. payouts from the payees' signed bindings, burn and refund. A receipt from before
    // payees has none: its payouts came from the orchestrator's ledger and are not rebuilt.
    if !receipt.payees.is_empty() {
        match payouts_hash(&receipt.payees, ev.bindings, chain_id) {
            Ok(h) if h != task.payouts_hash => failed.push(format!(
                "payoutsHash 0x{} is not the receipt's payees at their bindings 0x{}",
                hex::encode(task.payouts_hash),
                hex::encode(h)
            )),
            Ok(_) => {}
            Err(e) => failed.push(e),
        }
    }
    for (name, field, on_chain) in [
        ("burnAmount", &receipt.burn_settled_pwqc, task.burn_amount),
        (
            "refundAmount",
            &receipt.escrow_refunded_pwqc,
            task.refund_amount,
        ),
    ] {
        match U256::from_str_radix(field, 10) {
            Ok(v) if v == on_chain => {}
            Ok(v) => failed.push(format!("{name} {on_chain} is not the receipt's {v}")),
            Err(e) => failed.push(format!("receipt {name} {field:?}: {e}")),
        }
    }

    // 3. rootHash and the wrap digest.
    let root_hash = keccak256(ev.root);
    if root_hash.0 != task.root_hash {
        failed.push(format!(
            "rootHash 0x{} is not the root's keccak256 {root_hash}",
            hex::encode(task.root_hash)
        ));
    }
    if let Some(d) = wrap_digest {
        let encoded = encode_digest(d);
        if encoded != task.wrap_digest {
            failed.push(format!(
                "wrapDigest 0x{} is not the digest recomputed from the evidence 0x{}",
                hex::encode(task.wrap_digest),
                hex::encode(encoded)
            ));
        }
    }

    // 4. this node's own leaves and operator.
    let leaves: Leaves = match serde_json::from_slice(ev.leaves_json) {
        Ok(l) => l,
        Err(e) => {
            failed.push(format!("leaves JSON: {e}"));
            return failed;
        }
    };
    let mut own_in_root = false;
    for own in own_leaves {
        let found = leaves.leaves.iter().any(|l| {
            l.sub_task_id == own.sub_task_id
                && l.node_id == own.node_id
                && l.digest == own.sv_digest
        });
        if found {
            own_in_root = true;
        } else if leaves
            .leaves
            .iter()
            .any(|l| l.sub_task_id == own.sub_task_id && l.node_id == own.node_id)
        {
            // This node's leaf, with another result: its work was replaced.
            failed.push(format!(
                "leaf of sub-task {} carries this node's id with another digest",
                own.sub_task_id
            ));
        } else if leaves
            .leaves
            .iter()
            .any(|l| l.sub_task_id == own.sub_task_id)
        {
            // Another node's leaf for the same sub-task (a quorum peer) is not a mismatch.
            continue;
        } else {
            failed.push(format!(
                "leaf of sub-task {} is missing from the evidence",
                own.sub_task_id
            ));
        }
    }
    if own_in_root && !receipt.payees.is_empty() {
        if let Some(op) = own_operator {
            if !receipt.payees.iter().any(|p| p.operator_id == op) {
                failed.push(format!(
                    "operator {op} proved a leaf of the root but is not a payee"
                ));
            }
        }
    }
    failed
}

/// SettlementV4's wrap digest encoding: element i is the big-endian word at bytes 8i..8i+8.
pub fn encode_digest(d: [u64; 4]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, w) in d.iter().enumerate() {
        out[8 * i..8 * i + 8].copy_from_slice(&w.to_be_bytes());
    }
    out
}

/// keccak256(abi.encode(payouts)) for the payees at their verified bindings: amounts summed
/// per address, ordered by lowercase hex address, as the orchestrator builds settle.
fn payouts_hash(
    payees: &[Payee],
    bindings: &BTreeMap<String, Binding>,
    chain_id: u64,
) -> Result<[u8; 32], String> {
    let mut by_address: BTreeMap<String, U256> = BTreeMap::new();
    for p in payees {
        let carried = p.binding();
        let b = match carried.as_ref() {
            Some(b) => b,
            None => bindings
                .get(&p.operator_id)
                .ok_or_else(|| format!("payee {}: no payout binding", p.operator_id))?,
        };
        verify_binding(b, &p.operator_id, chain_id)?;
        let amount = U256::from_str_radix(&p.amount_pwqc, 10)
            .map_err(|e| format!("payee {} amount {:?}: {e}", p.operator_id, p.amount_pwqc))?;
        *by_address
            .entry(b.payout_address.to_ascii_lowercase())
            .or_default() += amount;
    }
    let mut payouts = Vec::with_capacity(by_address.len());
    for (addr, amount) in by_address {
        let to: Address = addr
            .parse()
            .map_err(|e| format!("payout address {addr}: {e}"))?;
        payouts.push(Payout { to, amount });
    }
    Ok(keccak256(payouts.abi_encode()).0)
}

/// The binding is the operator's: its key hashes to the operator id, and it signed the
/// wqc-payout-v1 register message for this address, chain and nonce.
fn verify_binding(b: &Binding, operator_id: &str, chain_id: u64) -> Result<(), String> {
    let bad = |why: &str| format!("payee {operator_id}: payout binding {why}");
    if b.operator_id != operator_id || b.chain_id != chain_id {
        return Err(bad("is for another operator or chain"));
    }
    if !b.status.is_empty() && b.status != "active" {
        return Err(bad("is not active"));
    }
    let b64 = base64::engine::general_purpose::STANDARD;
    let key: [u8; 32] = b64
        .decode(&b.operator_pubkey_b64)
        .ok()
        .and_then(|k| k.try_into().ok())
        .ok_or_else(|| bad("has no public key"))?;
    if hex::encode(Sha256::digest(key)) != operator_id {
        return Err(bad("key does not hash to the operator id"));
    }
    let sig: [u8; 64] = b64
        .decode(&b.signature_b64)
        .ok()
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| bad("has no signature"))?;
    let message = format!(
        "wqc-payout-v1\0register\0{operator_id}\0{}\0{chain_id}\0{}",
        b.payout_address.to_ascii_lowercase(),
        b.nonce
    );
    VerifyingKey::from_bytes(&key)
        .map_err(|_| bad("key is not an Ed25519 point"))?
        .verify(message.as_bytes(), &Signature::from_bytes(&sig))
        .map_err(|_| bad("signature does not verify"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    const CHAIN: u64 = 31337;

    fn operator(seed: u8) -> (String, SigningKey) {
        let key = SigningKey::from_bytes(&[seed; 32]);
        let id = hex::encode(Sha256::digest(key.verifying_key().to_bytes()));
        (id, key)
    }

    fn binding(id: &str, key: &SigningKey, address: &str) -> Binding {
        let message = format!("wqc-payout-v1\0register\0{id}\0{address}\0{CHAIN}\07");
        let b64 = base64::engine::general_purpose::STANDARD;
        Binding {
            operator_id: id.to_string(),
            chain_id: CHAIN,
            payout_address: address.to_string(),
            nonce: 7,
            status: "active".into(),
            operator_pubkey_b64: b64.encode(key.verifying_key().to_bytes()),
            signature_b64: b64.encode(key.sign(message.as_bytes()).to_bytes()),
        }
    }

    struct Fixture {
        receipt: Vec<u8>,
        bindings: BTreeMap<String, Binding>,
        root: Vec<u8>,
        leaves: Vec<u8>,
        task: OnChainTask,
        me: String,
        own: Vec<OwnLeaf>,
    }

    /// Two operators: mine (paid 70 at 0x…01) and another (paid 30 at 0x…aa).
    fn fixture() -> Fixture {
        let (me, my_key) = operator(1);
        let (other, other_key) = operator(2);
        let receipt = format!(
            r#"{{"task_id":"t","burn_settled_pwqc":"20","escrow_refunded_pwqc":"5","payees":[{{"operator_id":"{me}","amount_pwqc":"70"}},{{"operator_id":"{other}","amount_pwqc":"30"}}],"receipt_hash":""}}"#
        )
        .into_bytes();
        let mut bindings = BTreeMap::new();
        bindings.insert(
            me.clone(),
            binding(&me, &my_key, "0x0000000000000000000000000000000000000001"),
        );
        bindings.insert(
            other.clone(),
            binding(
                &other,
                &other_key,
                "0x00000000000000000000000000000000000000aa",
            ),
        );
        let root = b"root".to_vec();
        let leaves =
            br#"{"leaves":[{"sub_task_id":"s1","node_id":"me","digest":[1,2,3,4],"proof":"AA=="}]}"#
                .to_vec();
        let mut payouts_hash = [0u8; 32];
        // cast keccak $(cast abi-encode "f((address,uint256)[])" "[(0x…01,70),(0x…aa,30)]")
        hex::decode_to_slice(
            "7a49d4ad69c11dfe69db2d6cd397072e13a635609239439aa2ddc03e59eddcf5",
            &mut payouts_hash,
        )
        .unwrap();
        let task = OnChainTask {
            status: STATUS_SETTLED_COMMITTED,
            root_hash: keccak256(&root).0,
            receipt_hash: Sha3_256::digest(&receipt).into(),
            wrap_digest: encode_digest([9, 9, 9, 9]),
            payouts_hash,
            burn_amount: U256::from(20),
            refund_amount: U256::from(5),
            deadline: 100,
        };
        Fixture {
            receipt,
            bindings,
            root,
            leaves,
            task,
            me,
            own: vec![OwnLeaf {
                sub_task_id: "s1".into(),
                node_id: "me".into(),
                sv_digest: [1, 2, 3, 4],
            }],
        }
    }

    fn run(f: &Fixture, digest: Option<[u64; 4]>) -> Vec<String> {
        let ev = Evidence {
            receipt: &f.receipt,
            bindings: &f.bindings,
            root: &f.root,
            leaves_json: &f.leaves,
        };
        check(&f.task, &ev, CHAIN, Some(&f.me), &f.own, digest)
    }

    #[test]
    fn an_honest_settlement_passes() {
        let f = fixture();
        assert_eq!(run(&f, Some([9, 9, 9, 9])), Vec::<String>::new());
        assert_eq!(run(&f, None), Vec::<String>::new());
    }

    #[test]
    fn each_tampered_claim_is_caught() {
        type Tamper = Box<dyn Fn(&mut Fixture)>;
        let cases: Vec<(&str, Tamper)> = vec![
            ("payoutsHash", Box::new(|f| f.task.payouts_hash[0] ^= 1)),
            ("receiptHash", Box::new(|f| f.task.receipt_hash[0] ^= 1)),
            ("rootHash", Box::new(|f| f.task.root_hash[0] ^= 1)),
            (
                "burnAmount",
                Box::new(|f| f.task.burn_amount = U256::from(21)),
            ),
            (
                "refundAmount",
                Box::new(|f| f.task.refund_amount = U256::from(4)),
            ),
            ("wrapDigest", Box::new(|f| f.task.wrap_digest[31] ^= 1)),
            (
                "binding",
                Box::new(|f| {
                    let me = f.me.clone();
                    f.bindings.get_mut(&me).unwrap().payout_address =
                        "0x00000000000000000000000000000000000000bb".into();
                }),
            ),
            ("missing", Box::new(|f| f.own[0].sub_task_id = "s9".into())),
            ("replaced", Box::new(|f| f.own[0].sv_digest = [1, 2, 3, 5])),
            ("not a payee", Box::new(|f| f.me = operator(3).0)),
        ];
        for (name, tamper) in cases {
            let mut f = fixture();
            tamper(&mut f);
            assert!(!run(&f, Some([9, 9, 9, 9])).is_empty(), "{name}: passed");
        }
    }

    #[test]
    fn a_quorum_peers_leaf_is_not_a_mismatch() {
        let mut f = fixture();
        f.own[0].node_id = "peer".into();
        assert_eq!(run(&f, None), Vec::<String>::new());
    }

    #[test]
    fn a_receipt_without_payees_skips_the_payouts_check() {
        let mut f = fixture();
        f.receipt = br#"{"task_id":"t","burn_settled_pwqc":"20","escrow_refunded_pwqc":"5","receipt_hash":""}"#.to_vec();
        f.task.receipt_hash = Sha3_256::digest(&f.receipt).into();
        assert_eq!(run(&f, None), Vec::<String>::new());
    }

    #[test]
    fn payees_without_binding_lists_the_operators_to_fetch() {
        let f = fixture();
        let payees = payees_without_binding(&f.receipt).unwrap();
        assert_eq!(payees.len(), 2);
        assert!(payees.contains(&f.me));
        let carried = with_carried_bindings(&f);
        assert!(payees_without_binding(&carried).unwrap().is_empty());
    }

    /// The fixture's receipt with each payee's binding carried in it (as fixed at finalize).
    fn with_carried_bindings(f: &Fixture) -> Vec<u8> {
        let mut r: serde_json::Value = serde_json::from_slice(&f.receipt).unwrap();
        for p in r["payees"].as_array_mut().unwrap() {
            let b = &f.bindings[p["operator_id"].as_str().unwrap()];
            p["payout_address"] = b.payout_address.clone().into();
            p["chain_id"] = b.chain_id.into();
            p["nonce"] = b.nonce.into();
            p["operator_pubkey_b64"] = b.operator_pubkey_b64.clone().into();
            p["signature_b64"] = b.signature_b64.clone().into();
        }
        serde_json::to_vec(&r).unwrap()
    }

    /// Bindings carried in the receipt are checked as they are; the registry's current ones
    /// (a later rotation) play no part.
    #[test]
    fn carried_bindings_ignore_the_registry() {
        let mut f = fixture();
        f.receipt = with_carried_bindings(&f);
        f.task.receipt_hash = Sha3_256::digest(&f.receipt).into();
        // The registry now points elsewhere (the operator rotated after finalize).
        let me = f.me.clone();
        f.bindings.get_mut(&me).unwrap().payout_address =
            "0x00000000000000000000000000000000000000bb".into();
        assert_eq!(run(&f, None), Vec::<String>::new());
        f.bindings.clear();
        assert_eq!(run(&f, None), Vec::<String>::new());

        // A carried binding that was tampered with (address not what the operator signed) fails.
        let mut r: serde_json::Value = serde_json::from_slice(&f.receipt).unwrap();
        r["payees"][0]["payout_address"] = "0x00000000000000000000000000000000000000cc".into();
        f.receipt = serde_json::to_vec(&r).unwrap();
        f.task.receipt_hash = Sha3_256::digest(&f.receipt).into();
        assert!(!run(&f, None).is_empty());
    }
}
