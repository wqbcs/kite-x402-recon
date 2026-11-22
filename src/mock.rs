//! Fixture-backed chain client. Used by tests and by `--chain-mode mock` demos
//! so every example in the docs runs without network access.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use crate::chain::{BoxFuture, ChainClient, Receipt, Transfer, TransferQuery};
use crate::error::{ReconError, Result};
use crate::model::{parse_atomic, AmountAtomic};

#[derive(Debug, Deserialize)]
struct FixtureFile {
    chain_id: u64,
    tip: u64,
    #[serde(default)]
    token: FixtureToken,
    #[serde(default)]
    receipts: BTreeMap<String, FixtureReceipt>,
    #[serde(default)]
    transfers: Vec<FixtureTransfer>,
}

#[derive(Debug, Default, Deserialize)]
struct FixtureToken {
    #[serde(default = "default_symbol")]
    symbol: String,
    #[serde(default = "default_decimals")]
    decimals: u8,
}

fn default_symbol() -> String {
    "USDT".to_string()
}

fn default_decimals() -> u8 {
    6
}

#[derive(Debug, Deserialize)]
struct FixtureReceipt {
    #[serde(default = "default_true")]
    status: bool,
    block_number: u64,
    #[serde(default)]
    logs: Vec<FixtureTransfer>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
struct FixtureTransfer {
    tx_hash: String,
    block_number: u64,
    from: String,
    to: String,
    amount_atomic: String,
}

impl FixtureTransfer {
    fn into_transfer(self) -> Result<Transfer> {
        Ok(Transfer {
            tx_hash: self.tx_hash,
            block_number: self.block_number,
            from: self.from,
            to: self.to,
            amount_atomic: parse_atomic(&self.amount_atomic)?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct FixtureChain {
    chain_id: u64,
    tip: u64,
    symbol: String,
    decimals: u8,
    receipts: BTreeMap<String, Receipt>,
    transfers: Vec<Transfer>,
}

impl FixtureChain {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)?;
        let file: FixtureFile = serde_json::from_str(&raw)?;
        let mut receipts = BTreeMap::new();
        for (hash, receipt) in file.receipts {
            let logs = receipt
                .logs
                .into_iter()
                .map(FixtureTransfer::into_transfer)
                .collect::<Result<Vec<_>>>()?;
            receipts.insert(
                hash.to_ascii_lowercase(),
                Receipt {
                    status: receipt.status,
                    block_number: receipt.block_number,
                    logs,
                },
            );
        }
        let transfers = file
            .transfers
            .into_iter()
            .map(FixtureTransfer::into_transfer)
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            chain_id: file.chain_id,
            tip: file.tip,
            symbol: file.token.symbol,
            decimals: file.token.decimals,
            receipts,
            transfers,
        })
    }

    fn matching(&self, query: &TransferQuery) -> Vec<Transfer> {
        self.transfers
            .iter()
            .filter(|t| t.block_number >= query.from_block && t.block_number <= query.to_block)
            .filter(|t| t.to.eq_ignore_ascii_case(&query.to))
            .filter(|t| match query.from.as_deref() {
                Some(from) => t.from.eq_ignore_ascii_case(from),
                None => true,
            })
            .take(query.limit)
            .cloned()
            .collect()
    }
}

impl ChainClient for FixtureChain {
    fn chain_id(&self) -> u64 {
        self.chain_id
    }

    fn tip<'a>(&'a self) -> BoxFuture<'a, Result<u64>> {
        Box::pin(async move { Ok(self.tip) })
    }

    fn receipt<'a>(&'a self, tx_hash: &'a str) -> BoxFuture<'a, Result<Option<Receipt>>> {
        Box::pin(async move {
            if !tx_hash.starts_with("0x") || tx_hash.len() != 66 {
                return Err(ReconError::Chain(format!(
                    "transaction hash must be 0x + 64 hex characters: {tx_hash}"
                )));
            }
            Ok(self.receipts.get(&tx_hash.to_ascii_lowercase()).cloned())
        })
    }

    fn find_transfers<'a>(&'a self, query: TransferQuery) -> BoxFuture<'a, Result<Vec<Transfer>>> {
        Box::pin(async move { Ok(self.matching(&query)) })
    }

    fn token_decimals<'a>(&'a self) -> BoxFuture<'a, Result<u8>> {
        Box::pin(async move { Ok(self.decimals) })
    }

    fn token_symbol<'a>(&'a self) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move { Ok(self.symbol.clone()) })
    }
}

/// Helper for tests that want an in-memory chain without touching the disk.
pub fn in_memory(
    chain_id: u64,
    tip: u64,
    receipts: Vec<(String, Receipt)>,
    transfers: Vec<Transfer>,
) -> FixtureChain {
    FixtureChain {
        chain_id,
        tip,
        symbol: "USDT".to_string(),
        decimals: 6,
        receipts: receipts
            .into_iter()
            .map(|(hash, receipt)| (hash.to_ascii_lowercase(), receipt))
            .collect(),
        transfers,
    }
}

/// Build a transfer for tests and fixtures.
pub fn transfer(
    tx_hash: &str,
    block_number: u64,
    from: &str,
    to: &str,
    amount: AmountAtomic,
) -> Transfer {
    Transfer {
        tx_hash: tx_hash.to_string(),
        block_number,
        from: from.to_string(),
        to: to.to_string(),
        amount_atomic: amount,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_filters_by_window_and_receiver() {
        let chain = in_memory(
            2368,
            100,
            vec![],
            vec![
                transfer(
                    "0xaa",
                    90,
                    "0x1111111111111111111111111111111111111111",
                    "0x2222222222222222222222222222222222222222",
                    10,
                ),
                transfer(
                    "0xbb",
                    95,
                    "0x1111111111111111111111111111111111111111",
                    "0x3333333333333333333333333333333333333333",
                    20,
                ),
            ],
        );
        let found = futures_block_on(chain.find_transfers(TransferQuery {
            token: String::new(),
            from: None,
            to: "0x2222222222222222222222222222222222222222".to_string(),
            from_block: 80,
            to_block: 92,
            limit: 10,
        }))
        .unwrap_or_else(|e| panic!("fixture query failed: {e}"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].tx_hash, "0xaa");
    }

    fn futures_block_on<'a, T>(future: BoxFuture<'a, T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map(|rt| rt.block_on(future))
            .unwrap_or_else(|_| panic!("runtime"))
    }
}
