use crate::chains::traits::ChainAdapter;
use crate::chains::validation::{build_url, build_url_with_query, validate_aptos_address};
use crate::cli::parser::Network;
use anyhow::Result;
use async_trait::async_trait;
use reqwest::Client;
use serde_json::Value;

pub struct AptosAdapter {
    client: Client,
    rpc_url: String,
    verbose: bool,
}

impl AptosAdapter {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self::with_rpc(None, Network::Mainnet)
    }

    pub fn with_rpc(rpc_url: Option<String>, network: Network) -> Self {
        let url = rpc_url.unwrap_or_else(|| match network {
            Network::Mainnet => "https://fullnode.mainnet.aptoslabs.com/v1".to_string(),
            Network::Testnet => "https://fullnode.testnet.aptoslabs.com/v1".to_string(),
            Network::Devnet => "https://fullnode.devnet.aptoslabs.com/v1".to_string(),
            Network::Localnet => "http://127.0.0.1:8080/v1".to_string(),
        });

        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_else(|_| Client::new()),
            rpc_url: url,
            verbose: false,
        }
    }

    /// Enable verbose retry logging for this adapter's RPC calls.
    pub fn with_verbose(mut self, verbose: bool) -> Self {
        self.verbose = verbose;
        self
    }
}

#[async_trait]
impl ChainAdapter for AptosAdapter {
    fn name(&self) -> &'static str {
        "Aptos"
    }

    fn default_rpc(&self) -> &'static str {
        "https://fullnode.mainnet.aptoslabs.com/v1"
    }

    async fn call_rpc(&self, method: &str, params: Value) -> Result<Value> {
        if params != Value::Null
            && params != Value::Array(vec![])
            && params != Value::Object(serde_json::Map::new())
        {
            return Err(anyhow::anyhow!(
                "Aptos REST API does not support params in raw RPC calls. Use chain-specific subcommands instead."
            ));
        }
        let url = format!("{}/{}", self.rpc_url, method);
        crate::rpc::get_json(&self.client, &url, self.verbose).await
    }

    async fn get_balance(&self, address: &str) -> Result<Value> {
        let address = validate_aptos_address(address)?;
        let url = build_url(&self.rpc_url, &["accounts", &address, "resources"])?;
        crate::rpc::get_json(&self.client, url.as_str(), self.verbose).await
    }

    async fn get_transaction(&self, hash: &str) -> Result<Value> {
        let url = build_url(&self.rpc_url, &["transactions", "by_hash", hash])?;
        crate::rpc::get_json(&self.client, url.as_str(), self.verbose).await
    }

    async fn get_block(&self, block: Option<u64>) -> Result<Value> {
        match block {
            Some(height) => {
                let url = build_url(&self.rpc_url, &["blocks", "by_height", &height.to_string()])?;
                crate::rpc::get_json(&self.client, url.as_str(), self.verbose).await
            }
            None => {
                let url = build_url(&self.rpc_url, &[""])?;
                crate::rpc::get_json(&self.client, url.as_str(), self.verbose).await
            }
        }
    }

    async fn get_gas_price(&self) -> Result<Value> {
        let url = build_url(&self.rpc_url, &["estimate_gas_price"])?;
        crate::rpc::get_json(&self.client, url.as_str(), self.verbose).await
    }

    async fn get_account(&self, address: &str) -> Result<Value> {
        let address = validate_aptos_address(address)?;
        let url = build_url(&self.rpc_url, &["accounts", &address])?;
        crate::rpc::get_json(&self.client, url.as_str(), self.verbose).await
    }

    async fn get_history(&self, address: &str, limit: u32) -> Result<Value> {
        let address = validate_aptos_address(address)?;
        let url = build_url_with_query(
            &self.rpc_url,
            &["accounts", &address, "transactions"],
            &[("limit", limit.to_string())],
        )?;
        crate::rpc::get_json(&self.client, url.as_str(), self.verbose).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn retries_transient_429_before_succeeding() {
        let addr = crate::rpc::test_support::serve(vec![
            crate::rpc::test_support::MockResponse::json(429, "{}"),
            crate::rpc::test_support::MockResponse::json(200, r#"{"height": 1}"#),
        ])
        .await;

        let adapter =
            AptosAdapter::with_rpc(Some(format!("http://{addr}")), Network::Localnet);
        let result = adapter.call_rpc("blocks", Value::Null).await.unwrap();
        assert_eq!(result, serde_json::json!({"height": 1}));
    }
}
