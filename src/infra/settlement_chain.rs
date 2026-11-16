//! SettlementV4 on L2: read a task, challenge it.

use alloy::network::EthereumWallet;
use alloy::primitives::{keccak256, Address, B256};
use alloy::providers::{DynProvider, Provider, ProviderBuilder};
use alloy::signers::local::PrivateKeySigner;
use alloy::sol;
use anyhow::Context;

use crate::domain::settlement::OnChainTask;

sol! {
    #[sol(rpc)]
    interface ISettlementV4 {
        function tasks(bytes32 taskId) external view returns (
            uint8 status,
            address client,
            uint256 escrow,
            bytes32 rootHash,
            bytes32 receiptHash,
            bytes32 wrapDigest,
            bytes32 payoutsHash,
            uint256 burnAmount,
            uint256 refundAmount,
            uint256 deadline,
            address challenger,
            uint256 bond
        );
        function challengeBond() external view returns (uint256);
        function challenge(bytes32 taskId) external payable;
    }
}

pub struct SettlementChain {
    provider: DynProvider,
    contract: Address,
    /// Without a key the challenger only reports.
    can_challenge: bool,
}

/// SettlementV4's task key: keccak256 of the task id string.
pub fn task_key(task_id: &str) -> B256 {
    keccak256(task_id.as_bytes())
}

impl SettlementChain {
    pub fn new(
        rpc_url: &str,
        contract: Address,
        private_key: Option<&str>,
    ) -> anyhow::Result<Self> {
        let url = rpc_url.parse().context("WQC_CHALLENGER_RPC_URL")?;
        let (provider, can_challenge) = match private_key {
            Some(key) => {
                let signer: PrivateKeySigner = key
                    .trim()
                    .parse()
                    .context("WQC_CHALLENGER_PRIVATE_KEY is not a secp256k1 key")?;
                let p = ProviderBuilder::new()
                    .wallet(EthereumWallet::from(signer))
                    .connect_http(url);
                (DynProvider::new(p), true)
            }
            None => (
                DynProvider::new(ProviderBuilder::new().connect_http(url)),
                false,
            ),
        };
        Ok(Self {
            provider,
            contract,
            can_challenge,
        })
    }

    pub fn can_challenge(&self) -> bool {
        self.can_challenge
    }

    pub async fn task(&self, task_id: &str) -> anyhow::Result<OnChainTask> {
        let c = ISettlementV4::new(self.contract, &self.provider);
        let t = c.tasks(task_key(task_id)).call().await?;
        Ok(OnChainTask {
            status: t.status,
            root_hash: t.rootHash.0,
            receipt_hash: t.receiptHash.0,
            wrap_digest: t.wrapDigest.0,
            payouts_hash: t.payoutsHash.0,
            burn_amount: t.burnAmount,
            refund_amount: t.refundAmount,
            deadline: u64::try_from(t.deadline).unwrap_or(u64::MAX),
        })
    }

    /// The latest block's timestamp: the clock the challenge window runs on.
    pub async fn chain_time(&self) -> anyhow::Result<u64> {
        let block = self
            .provider
            .get_block_by_number(alloy::eips::BlockNumberOrTag::Latest)
            .await?
            .context("no latest block")?;
        Ok(block.header.timestamp)
    }

    /// Sends `challenge(taskId)` with the contract's bond; returns the transaction hash once
    /// it is mined.
    pub async fn challenge(&self, task_id: &str) -> anyhow::Result<B256> {
        anyhow::ensure!(self.can_challenge, "no L2 key to challenge with");
        let c = ISettlementV4::new(self.contract, &self.provider);
        let bond = c.challengeBond().call().await?;
        let receipt = c
            .challenge(task_key(task_id))
            .value(bond)
            .send()
            .await?
            .get_receipt()
            .await?;
        anyhow::ensure!(receipt.status(), "challenge transaction reverted");
        Ok(receipt.transaction_hash)
    }
}
