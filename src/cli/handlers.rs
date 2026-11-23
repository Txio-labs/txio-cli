use crate::chains::factory::ChainFactory;
use crate::chains::traits::ChainAdapter;
use crate::cli::format::format_units_fixed;
use crate::cli::parser::{ChainCommand, Cli, Commands, ConfigAction, DbAction};
use crate::cli::ui;
use crate::utils;
use anyhow::{Result, anyhow};
use colored::*;
use serde_json::Value;
use std::sync::Arc;
use txio_api::dtos::admin_dtos::{AdminLogEntry, AdminStatsResponse, AdminUsersResponse};
use txio_api::dtos::request::LoginRequest;
use txio_api::dtos::response::AuthResponse;

use dialoguer::{Confirm, Input, Password};


/// Parse Ethereum `eth_getBalance` hex quantity into wei.
pub(crate) fn eth_wei_from_rpc_result(result: &Value) -> Option<u128> {
    let hex_str = result.as_str()?;
    let clean = hex_str.trim_start_matches("0x");
    if clean.is_empty() {
        return Some(0);
    }
    u128::from_str_radix(clean, 16).ok()
}

/// Parse Solana `getBalance` JSON `{ "value": <lamports u64> }`.
pub(crate) fn sol_lamports_from_rpc_result(result: &Value) -> Option<u64> {
    result.get("value").and_then(|v| v.as_u64())
}

/// Parse Ethereum `eth_gasPrice` hex quantity into wei-per-gas.
pub(crate) fn eth_gas_price_wei_from_rpc_result(result: &Value) -> Option<u128> {
    eth_wei_from_rpc_result(result)
}

/// One Sui coin balance row used for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SuiCoinBalanceRow {
    pub total_balance: String,
    pub coin_object_count: u64,
    pub coin_type: String,
}

/// Parse Sui `suix_getAllBalances` array payload into rows.
pub(crate) fn sui_coin_rows_from_rpc_result(result: &Value) -> Option<Vec<SuiCoinBalanceRow>> {
    let arr = result.as_array()?;
    let mut rows = Vec::with_capacity(arr.len());
    for item in arr {
        rows.push(SuiCoinBalanceRow {
            total_balance: item
                .get("totalBalance")
                .and_then(|v| v.as_str())
                .unwrap_or("0")
                .to_string(),
            coin_object_count: item
                .get("coinObjectCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            coin_type: item
                .get("coinType")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown")
                .to_string(),
        });
    }
    Some(rows)
}

/// Human display for a Sui coin row (SUI uses 9 decimals).
pub(crate) fn format_sui_coin_balance_display(row: &SuiCoinBalanceRow) -> String {
    if row.coin_type == "0x2::sui::SUI" {
        if let Ok(b) = row.total_balance.parse::<u128>() {
            return format!("{} SUI", format_units_fixed(b, 9, 4));
        }
    }
    row.total_balance.clone()
}

/// Parse an Aptos account-resources array into the AptosCoin balance (octas).
pub(crate) fn aptos_balance_octas(result: &Value) -> Option<u128> {
    let arr = result.as_array()?;
    for resource in arr {
        if resource.get("type").and_then(|t| t.as_str())
            == Some("0x1::coin::CoinStore<0x1::aptos_coin::AptosCoin>")
        {
            return resource
                .get("data")
                .and_then(|d| d.get("coin"))
                .and_then(|c| c.get("value"))
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<u128>().ok());
        }
    }
    None
}

/// Parse a Sui reference gas price (string or number) into MIST.
pub(crate) fn sui_gas_mist(result: &Value) -> Option<u64> {
    result
        .as_str()
        .and_then(|s| s.parse::<u64>().ok())
        .or_else(|| result.as_u64())
}

/// Parse an Ethereum `eth_gasPrice` hex (with or without `0x`) into wei.
pub(crate) fn ethereum_gas_wei(result: &Value) -> Option<u128> {
    let hex = result.as_str()?;
    let clean = hex.trim_start_matches("0x");
    if clean.is_empty() {
        return Some(0);
    }
    u128::from_str_radix(clean, 16).ok()
}

fn api_base_url() -> String {
    std::env::var("API_URL").unwrap_or_else(|_| "http://localhost:8000".to_string())
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

fn is_sui_coin_type(coin_type: &str) -> bool {
    coin_type
        .strip_suffix("::sui::SUI")
        .and_then(|addr| addr.strip_prefix("0x"))
        .is_some_and(|hex| hex.trim_start_matches('0') == "2")
}

fn truncate_utf8_for_display(input: &str, prefix_len: usize, suffix_len: usize) -> String {
    let char_count = input.chars().count();
    if char_count <= prefix_len + suffix_len {
        return input.to_string();
    }

    let prefix: String = input.chars().take(prefix_len).collect();
    let suffix: String = input
        .chars()
        .rev()
        .take(suffix_len)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    format!("{prefix}...{suffix}")
}

pub struct CommandHandler;

/// Decide which network a command runs against: an explicit `--network`
/// flag always wins, then the last choice persisted by `switch --network`,
/// and finally the safe Mainnet default. Kept pure so the precedence is
/// unit-testable without touching the real config directory.
fn resolve_network(flag: Option<crate::cli::parser::Network>, persisted: Option<String>) -> crate::cli::parser::Network {
    use crate::cli::parser::Network;
    flag.or_else(|| {
        persisted
            .as_deref()
            .and_then(Network::from_config_str)
    })
    .unwrap_or_default()
}

impl CommandHandler {
    pub async fn handle(cli: Cli) -> Result<()> {
        let network = resolve_network(cli.network.clone(), utils::get_current_network()?);
        match cli.command {
            Commands::Chains => {
                println!("{}", "Supported Blockchains:".bold().cyan());
                for chain in ChainFactory::list_chains() {
                    println!("  - {}", chain.green());
                }
            }
            Commands::Switch { chain, network } => {
                if chain.is_none() && network.is_none() {
                    ui::print_error("Please specify a chain and/or --network to switch.");
                    return Ok(());
                }

                if let Some(chain_name) = chain {
                    if ChainFactory::list_chains().contains(&chain_name.to_lowercase().as_str()) {
                        utils::save_current_chain(&chain_name.to_lowercase())?;
                        ui::print_success(&format!(
                            "Switched default chain to {}",
                            chain_name.bold().cyan()
                        ));
                    } else {
                        let msg = format!("Unknown chain '{chain_name}'");
                        let suggestion = ChainFactory::suggest_chain(&chain_name);
                        if let Some(s) = suggestion {
                            ui::print_error(&format!("{msg} \n\nDid you mean:\n  {}", s.green()));
                        } else {
                            ui::print_error(&msg);
                        }
                    }
                }

                if let Some(net) = network {
                    utils::save_current_network(&net.to_string())?;
                    ui::print_success(&format!(
                        "Switched default network to {}",
                        net.to_string().bold().yellow()
                    ));
                }
            }
            Commands::Login => {
                Self::handle_login().await?;
            }
            Commands::Logout => {
                utils::remove_token()?;
                ui::print_success("Logged out successfully.");
            }
            Commands::Status => {
                let chain = utils::get_current_chain()?.unwrap_or_else(|| "sui".to_string());
                let logged_in = utils::get_token()?.is_some();
                println!("{}", "─── txio Status ───".bold().cyan());
                println!(
                    "  {} Default chain:  {}",
                    "»".dimmed(),
                    chain.green().bold()
                );
                println!(
                    "  {} Network:        {}",
                    "»".dimmed(),
                    network.to_string().yellow()
                );
                println!(
                    "  {} Authenticated:  {}",
                    "»".dimmed(),
                    if logged_in {
                        "Yes".green().bold()
                    } else {
                        "No".red().bold()
                    }
                );
                if let Ok(adapter) = ChainFactory::get_adapter(
                    &chain,
                    cli.rpc_url.clone(),
                    network.clone(),
                    cli.verbose,
                )
                {
                    let rpc = cli.rpc_url.as_deref().unwrap_or(adapter.default_rpc());
                    let healthy = adapter.get_gas_price().await.is_ok();
                    println!("  {} RPC endpoint:   {}", "»".dimmed(), rpc.dimmed());
                    println!(
                        "  {} RPC health:     {}",
                        "»".dimmed(),
                        if healthy {
                            "✔ OK".green().bold()
                        } else {
                            "✖ Unreachable".red().bold()
                        }
                    );
                }
            }
            Commands::Config { action } => match action {
                ConfigAction::List => {
                    let entries = utils::list_config()?;
                    if entries.is_empty() {
                        println!("{}", "No configuration entries set.".dimmed());
                    } else {
                        println!("{}", "CLI Configuration:".bold().cyan());
                        for (k, v) in entries {
                            println!("  {} = {}", k.yellow(), v.green());
                        }
                    }
                }
                ConfigAction::Get { key } => match utils::get_config(&key)? {
                    Some(v) => println!("{} = {}", key.yellow(), v.green()),
                    None => ui::print_error(&format!("Key '{}' not found.", key)),
                },
                ConfigAction::Set { key, value } => {
                    utils::save_config(&key, &value)?;
                    ui::print_success(&format!("Set {} = {}", key.yellow(), value.green()));
                }
                ConfigAction::Unset { key } => {
                    utils::remove_config(&key)?;
                    ui::print_success(&format!("Removed key '{}'.", key.yellow()));
                }
            },
            Commands::Sui { command } => {
                let adapter = ChainFactory::get_adapter(
                    "sui",
                    cli.rpc_url.clone(),
                    network.clone(),
                    cli.verbose,
                )?;
                Self::handle_chain_command(adapter, command, cli.pretty, cli.verbose).await?;
            }
            Commands::Ethereum { command } => {
                let adapter = ChainFactory::get_adapter(
                    "ethereum",
                    cli.rpc_url.clone(),
                    network.clone(),
                    cli.verbose,
                )?;
                Self::handle_chain_command(adapter, command, cli.pretty, cli.verbose).await?;
            }
            Commands::Solana { command } => {
                let adapter = ChainFactory::get_adapter(
                    "solana",
                    cli.rpc_url.clone(),
                    network.clone(),
                    cli.verbose,
                )?;
                Self::handle_chain_command(adapter, command, cli.pretty, cli.verbose).await?;
            }
            Commands::Aptos { command } => {
                let adapter = ChainFactory::get_adapter(
                    "aptos",
                    cli.rpc_url.clone(),
                    network.clone(),
                    cli.verbose,
                )?;
                Self::handle_chain_command(adapter, command, cli.pretty, cli.verbose).await?;
            }
            Commands::Soroban { command } => {
                let adapter = ChainFactory::get_adapter(
                    "soroban",
                    cli.rpc_url.clone(),
                    network.clone(),
                    cli.verbose,
                )?;
                Self::handle_chain_command(adapter, command, cli.pretty, cli.verbose).await?;
            }
            Commands::Db { action } => {
                Self::handle_db_command(action).await?;
            }
            Commands::Completion { shell } => {
                use clap::CommandFactory;
                let mut cmd = Cli::command();
                clap_complete::generate(shell, &mut cmd, "txio", &mut std::io::stdout());
            }
            _ => {
                println!("{}", "Feature coming soon!".yellow());
            }
        }
        Ok(())
    }

    async fn handle_db_command(action: DbAction) -> Result<()> {
        let token = match utils::get_token()? {
            Some(token) => token,
            None => {
                ui::print_error(&format!(
                    "Not logged in. Run {} first.",
                    "txio login".cyan()
                ));
                return Ok(());
            }
        };

        let client = http_client();
        let api = api_base_url();

        match action {
            DbAction::ListUsers => {
                let response = client
                    .get(format!("{api}/api/v1/admin/users"))
                    .bearer_auth(&token)
                    .send()
                    .await?;

                if let Some(body) = Self::handle_admin_error(response.status()) {
                    ui::print_error(&body);
                    return Ok(());
                }

                let body: AdminUsersResponse = response.json().await?;
                println!("{}", "Registered Users:".bold().cyan());
                if body.emails.is_empty() {
                    println!("  {}", "No users found.".yellow());
                } else {
                    for email in body.emails {
                        println!("  - {}", email.green());
                    }
                }
            }
            DbAction::DeleteUser { email } => {
                let confirmed = Confirm::new()
                    .with_prompt(format!(
                        "Delete user '{}'? This cannot be undone",
                        email.red()
                    ))
                    .default(false)
                    .interact()?;

                if !confirmed {
                    ui::print_error("Aborted.");
                    return Ok(());
                }

                let response = client
                    .post(format!("{api}/api/v1/admin/users/delete"))
                    .bearer_auth(&token)
                    .json(&serde_json::json!({ "email": email }))
                    .send()
                    .await?;

                if let Some(body) = Self::handle_admin_error(response.status()) {
                    ui::print_error(&body);
                    return Ok(());
                }

                ui::print_success(&format!("User '{}' deleted.", email.bold()));
            }
            DbAction::Stats => {
                let response = client
                    .get(format!("{api}/api/v1/admin/stats"))
                    .bearer_auth(&token)
                    .send()
                    .await?;

                if let Some(body) = Self::handle_admin_error(response.status()) {
                    ui::print_error(&body);
                    return Ok(());
                }

                let stats: AdminStatsResponse = response.json().await?;
                println!("{}", "─── Database Stats ───".bold().cyan());
                println!(
                    "  {} Registered users: {}",
                    "»".dimmed(),
                    stats.user_count.to_string().green().bold()
                );
                println!(
                    "  {} Total RPC logs:   {}",
                    "»".dimmed(),
                    stats.rpc_log_count.to_string().yellow().bold()
                );
            }
            DbAction::ListLogs { limit } => {
                let response = client
                    .get(format!("{api}/api/v1/admin/logs"))
                    .query(&[("limit", limit.to_string())])
                    .bearer_auth(&token)
                    .send()
                    .await?;

                if let Some(body) = Self::handle_admin_error(response.status()) {
                    ui::print_error(&body);
                    return Ok(());
                }

                let logs: Vec<AdminLogEntry> = response.json().await?;
                println!("{}", "Recent RPC Logs:".bold().cyan());
                if logs.is_empty() {
                    println!("  {}", "No logs found.".yellow());
                } else {
                    for log in logs {
                        let status = if log.success {
                            "OK".green()
                        } else {
                            "ERR".red()
                        };
                        let err = log.error.unwrap_or_default();
                        println!(
                            "  [{}] {} {}",
                            status,
                            log.method.cyan(),
                            if !err.is_empty() {
                                format!("— {}", err.dimmed())
                            } else {
                                String::new()
                            }
                        );
                    }
                }
            }
        }
        Ok(())
    }

    /// Maps an admin-endpoint HTTP status into a user-facing message, or
    /// `None` when the response was successful and the caller should
    /// proceed to read the body.
    fn handle_admin_error(status: reqwest::StatusCode) -> Option<String> {
        match status {
            reqwest::StatusCode::UNAUTHORIZED => Some(format!(
                "Session expired or invalid. Run {} again.",
                "txio login".cyan()
            )),
            reqwest::StatusCode::FORBIDDEN => Some("Admin access required.".to_string()),
            status if status.is_success() => None,
            status => Some(format!("Request failed ({}).", status)),
        }
    }

    async fn handle_login() -> Result<()> {
        println!("{}", "--- txio Account Login ---".bold().cyan());

        let email: String = Input::new().with_prompt("Email").interact_text()?;
        let password = Password::new().with_prompt("Password").interact()?;

        println!("\n{} Logging in...", "⏳".yellow());

        let login_request = LoginRequest {
            email: email.clone(),
            password,
        };

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        let api_url =
            std::env::var("API_URL").unwrap_or_else(|_| "http://localhost:8000".to_string());

        let response = client
            .post(format!("{api_url}/api/v1/auth/login"))
            .json(&login_request)
            .send()
            .await?;

        if response.status().is_success() {
            let auth_response: AuthResponse = response.json().await?;
            utils::save_token(&auth_response.token)?;
            ui::print_success(&format!(
                "Login successful! Welcome, {}.",
                auth_response.user.email.bold().cyan()
            ));
        } else {
            let status = response.status();
            let error_body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            ui::print_error(&format!("Login failed ({}): {}", status, error_body.red()));
        }

        Ok(())
    }

    async fn handle_chain_command(
        adapter: Arc<dyn ChainAdapter>,
        command: ChainCommand,
        pretty: bool,
        verbose: bool,
    ) -> Result<()> {
        let start_time = std::time::Instant::now();

        if verbose {
            eprintln!(
                "[verbose] target chain: {} | default RPC: {}",
                adapter.name(),
                adapter.default_rpc()
            );
        }

        match command {
            ChainCommand::Call { method, params } => {
                let params_val: Value = if let Some(p) = params {
                    serde_json::from_str(&p).map_err(|e| anyhow!("Invalid JSON params: {e}"))?
                } else {
                    Value::Array(vec![])
                };

                if verbose {
                    eprintln!("[verbose] method: {method} | params: {params_val}");
                }

                // Status goes to stderr so stdout carries only the answer.
                eprintln!(
                    "{} Calling {} on {}...",
                    "🚀".bold(),
                    method.cyan(),
                    adapter.name().green()
                );

                let response = adapter.call_rpc(&method, params_val).await?;
                match Self::balance_lines(adapter.name(), &method, &response) {
                    Some(lines) => lines.iter().for_each(|line| println!("{line}")),
                    None => Self::print_result(&response, pretty)?,
                }
            }
            ChainCommand::Balance { address } => {
                eprintln!(
                    "{} Fetching balance for {} on {}...",
                    "💰".bold(),
                    address.dimmed(),
                    adapter.name().green()
                );

                let resolved_address = if let Some(addr) = adapter.resolve_name(&address).await? {
                    eprintln!("{} Resolved {} to {}", "🔍".blue(), address.yellow(), addr.cyan());
                    addr
                } else {
                    if address.ends_with(".sui") || address.ends_with(".eth") {
                        eprintln!(
                            "{} {} could not be resolved! Proceeding with raw input...",
                            "⚠️".yellow(),
                            address.yellow()
                        );
                    }
                    address
                };

                let result = adapter.get_balance(&resolved_address).await?;
                match Self::balance_lines(adapter.name(), "", &result) {
                    Some(lines) => lines.iter().for_each(|line| println!("{line}")),
                    None => Self::print_result(&result, pretty)?,
                }
            }
            ChainCommand::Tx { hash } => {
                eprintln!(
                    "{} Fetching transaction {} on {}...\n",
                    "🔎".bold(),
                    hash.dimmed(),
                    adapter.name().green()
                );
                let result = adapter.get_transaction(&hash).await?;
                Self::print_result(&result, pretty)?;
            }
            ChainCommand::Object { id } => {
                eprintln!(
                    "{} Inspecting {} on {}...\n",
                    "🔎".bold(),
                    id.dimmed(),
                    adapter.name().green()
                );
                let result = adapter.get_account(&id).await?;
                Self::print_result(&result, pretty)?;
            }
            ChainCommand::History { address, limit } => {
                let chain = adapter.name();
                if chain == "Ethereum" {
                    eprintln!(
                        "{} Fetching up to {} recent ERC-20 transfer logs for {} on {}...\n",
                        "📜".bold(),
                        limit,
                        address.dimmed(),
                        chain.green()
                    );
                } else {
                    eprintln!(
                        "{} Fetching {} recent transactions for {} on {}...\n",
                        "📜".bold(),
                        limit,
                        address.dimmed(),
                        chain.green()
                    );
                }
                let resolved_address = if let Some(addr) = adapter.resolve_name(&address).await? {
                    println!(
                        "{} Resolved {} to {}\n",
                        "🔍".blue(),
                        address.yellow(),
                        addr.cyan()
                    );
                    addr
                } else {
                    if address.ends_with(".sui") || address.ends_with(".eth") {
                        println!(
                            "{} {} could not be resolved! Proceeding with raw input...\n",
                            "⚠️".yellow(),
                            address.yellow()
                        );
                    }
                    address
                };
                let result = adapter.get_history(&resolved_address, limit).await?;
                match (pretty, crate::cli::history::history_lines(adapter.name(), &result)) {
                    (false, Some(lines)) => lines.iter().for_each(|line| println!("{line}")),
                    _ => Self::print_value(&result, pretty)?,
                }
            }
            ChainCommand::Gas => {
                eprintln!(
                    "{} Fetching gas price on {}...\n",
                    "⛽".bold(),
                    adapter.name().green()
                );
                let result = adapter.get_gas_price().await?;
                let chain = adapter.name();

                if chain == "Sui" {
                    let mist = sui_gas_mist(&result).unwrap_or(0);
                    let sui_gas = format_units_fixed(mist as u128, 9, 9);
                    println!(
                        "{} {} MIST  ({} SUI per gas unit)",
                        "Reference Gas Price:".bold().cyan(),
                        mist.to_string().green().bold(),
                        sui_gas
                    );
                } else if chain == "Ethereum" {
                    if let Some(wei) = ethereum_gas_wei(&result) {
                        let gwei = format_units_fixed(wei, 9, 4);
                        println!(
                            "{} {} Gwei  ({} wei)",
                            "Gas Price:".bold().cyan(),
                            gwei.green().bold(),
                            wei.to_string().yellow()
                        );
                    } else {
                        Self::print_result(&result, pretty)?;
                    }
                } else {
                    Self::print_result(&result, pretty)?;
                }
            }
            ChainCommand::Block { number } => {
                match number {
                    Some(n) => eprintln!(
                        "{} Fetching block #{} on {}...\n",
                        "📦".bold(),
                        n,
                        adapter.name().green()
                    ),
                    None => eprintln!(
                        "{} Fetching latest block on {}...\n",
                        "📦".bold(),
                        adapter.name().green()
                    ),
                }
                let result = adapter.get_block(number).await?;
                Self::print_result(&result, pretty)?;
            }
        }

        if verbose {
            eprintln!("[verbose] elapsed: {:.2?}", start_time.elapsed());
        }

        Ok(())
    }

    fn print_value(value: &Value, pretty: bool) -> Result<()> {
        if pretty {
            println!("{}", serde_json::to_string_pretty(value)?);
        } else {
            println!("{}", serde_json::to_string(value)?);
        }
        Ok(())
    }

    /// Prints an RPC result without JSON wrapping for scalars (a string is
    /// printed as-is, not quoted); containers still print as JSON.
    fn print_result(value: &Value, pretty: bool) -> Result<()> {
        match value {
            Value::String(s) => println!("{s}"),
            Value::Null | Value::Bool(_) | Value::Number(_) => println!("{value}"),
            _ => Self::print_value(value, pretty)?,
        }
        Ok(())
    }

    /// Parses a decimal amount string (e.g. "0.9580461") into base units with
    /// `decimals` fractional digits. None if it has more fractional digits than that.
    fn parse_decimal_units(s: &str, decimals: u32) -> Option<u128> {
        let (whole, frac) = s.split_once('.').unwrap_or((s, ""));
        if frac.len() > decimals as usize {
            return None;
        }
        let whole = if whole.is_empty() { "0" } else { whole };
        let padded = format!("{frac:0<width$}", width = decimals as usize);
        format!("{whole}{padded}").parse::<u128>().ok()
    }

    /// The human-readable balance lines for a balance result, or `None` when
    /// the call is not a balance query for this chain or the result is not in

    /// a shape we know. `method` is empty for the `balance` subcommand.
    fn balance_lines(chain: &str, method: &str, value: &Value) -> Option<Vec<String>> {
        let is_balance_call = match chain {
            "Sui" => matches!(method, "" | "suix_getAllBalances" | "suix_getBalance"),
            "Ethereum" => matches!(method, "" | "eth_getBalance"),
            "Solana" => matches!(method, "" | "getBalance"),
            "Soroban" => matches!(method, "" | "getBalance"),
            _ => method.is_empty(),
        };
        if !is_balance_call {
            return None;
        }

        match chain {
            "Sui" => {
                let entries: Vec<&Value> = match value {
                    Value::Array(arr) => arr.iter().collect(),
                    Value::Object(_) => vec![value],
                    _ => return None,
                };
                if entries.is_empty() {
                    return Some(vec!["0 SUI".to_string()]);
                }
                Some(
                    entries
                        .into_iter()
                        .map(|item| {
                            let total = item.get("totalBalance").and_then(|v| v.as_str()).unwrap_or("0");
                            let coin_type = item.get("coinType").and_then(|v| v.as_str()).unwrap_or("Unknown");
                            if is_sui_coin_type(coin_type) {
                                match total.parse::<u128>() {
                                    Ok(b) => format!("{} SUI", format_units_fixed(b, 9, 4)),
                                    Err(_) => format!("{total} SUI"),
                                }
                            } else {
                                format!("{total} {coin_type}")
                            }
                        })
                        .collect(),
                )
            }
            "Ethereum" => {
                let hex = value.as_str()?;
                let wei = u128::from_str_radix(hex.trim_start_matches("0x"), 16).ok()?;
                Some(vec![format!("{} ETH", format_units_fixed(wei, 18, 4))])
            }
            "Solana" => {
                let lamports = value.get("value").and_then(|v| v.as_u64())?;
                Some(vec![format!("{} SOL", format_units_fixed(lamports as u128, 9, 4))])
            }
            "Aptos" => {
                let octas = value.as_array()?.iter().find_map(|resource| {
                    if resource.get("type").and_then(|t| t.as_str())
                        != Some("0x1::coin::CoinStore<0x1::aptos_coin::AptosCoin>")
                    {
                        return None;
                    }
                    resource.get("data")?.get("coin")?.get("value")?.as_str()?.parse::<u128>().ok()
                })?;
                Some(vec![format!("{} APT", format_units_fixed(octas, 8, 4))])
            }
            "Starknet" => {
                let amount = value.get("balance")?.as_str()?;
                let token = value.get("token")?.as_str()?;
                Some(vec![format!("{amount} {token}")])
            }
            "Soroban" => {
                // Horizon account: one line per asset; amounts are 7-decimal strings.
                let lines: Vec<String> = value
                    .get("balances")?
                    .as_array()?
                    .iter()
                    .filter_map(|b| {
                        let base = Self::parse_decimal_units(b.get("balance")?.as_str()?, 7)?;
                        let code = if b.get("asset_type")?.as_str()? == "native" {
                            "XLM".to_string()
                        } else {
                            b.get("asset_code")?.as_str()?.to_string()
                        };
                        Some(format!("{} {code}", format_units_fixed(base, 7, 4)))
                    })
                    .collect();
                if lines.is_empty() { None } else { Some(lines) }
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn balance_lines_shape_per_chain() {
        use super::CommandHandler;
        use serde_json::json;

        // Sui: SUI formatted to 4 places, other coin types left raw.
        let sui = json!([
            {"coinType": "0x2::sui::SUI", "totalBalance": "1234500000"},
            {"coinType": "0xdba3::usdc::USDC", "totalBalance": "42"}
        ]);
        assert_eq!(
            CommandHandler::balance_lines("Sui", "", &sui).unwrap(),
            vec!["1.2345 SUI".to_string(), "42 0xdba3::usdc::USDC".to_string()]
        );

        // Ethereum eth_getBalance hex -> ETH.
        assert_eq!(
            CommandHandler::balance_lines("Ethereum", "eth_getBalance", &json!("0xde0b6b3a7640000")).unwrap(),
            vec!["1.0000 ETH".to_string()]
        );

        // Solana getBalance {value} lamports -> SOL.
        assert_eq!(
            CommandHandler::balance_lines("Solana", "getBalance", &json!({"context": {}, "value": 1500000000u64})).unwrap(),
            vec!["1.5000 SOL".to_string()]
        );

        // Non-balance methods are not reshaped.
        assert!(CommandHandler::balance_lines("Ethereum", "eth_blockNumber", &json!("0x10")).is_none());
        assert!(CommandHandler::balance_lines("Sui", "suix_getCoins", &json!([])).is_none());
    }

    use super::truncate_utf8_for_display;
    use super::resolve_network;
    use super::{
        aptos_balance_octas, eth_wei_from_rpc_result, ethereum_gas_wei,
        format_sui_coin_balance_display, sol_lamports_from_rpc_result,
        sui_coin_rows_from_rpc_result, sui_gas_mist, SuiCoinBalanceRow,
    };
    use crate::cli::parser::Network;
    use serde_json::json;

    #[test]
    fn explicit_flag_beats_persisted_and_default() {
        let resolved = resolve_network(Some(Network::Testnet), Some("mainnet".to_string()));
        assert_eq!(resolved, Network::Testnet);
    }

    #[test]
    fn persisted_network_used_when_no_flag() {
        let resolved = resolve_network(None, Some("devnet".to_string()));
        assert_eq!(resolved, Network::Devnet);
    }

    #[test]
    fn unrecognized_persisted_value_falls_back_to_mainnet() {
        let resolved = resolve_network(None, Some("garbage".to_string()));
        assert_eq!(resolved, Network::Mainnet);
    }

    #[test]
    fn nothing_settled_defaults_to_mainnet() {
        let resolved = resolve_network(None, None);
        assert_eq!(resolved, Network::Mainnet);
    }

    #[test]
    fn truncates_multi_byte_utf8_input_without_panicking() {
        let input = "0x2::🍕::coin::very_long_type_name";
        let output = truncate_utf8_for_display(input, 10, 10);

        assert!(output.contains("..."));
        assert!(output.chars().count() < input.chars().count());
    }

    #[test]
    fn leaves_short_strings_unchanged() {
        let input = "0x2::sui::SUI";
        assert_eq!(truncate_utf8_for_display(input, 10, 10), input);
    }


    #[test]
    fn eth_wei_from_rpc_parses_hex_quantity() {
        assert_eq!(
            eth_wei_from_rpc_result(&json!("0xde0b6b3a7640000")),
            Some(1_000_000_000_000_000_000u128)
        );
        assert_eq!(eth_wei_from_rpc_result(&json!("0x0")), Some(0));
        assert_eq!(eth_wei_from_rpc_result(&json!("0x")), Some(0));
        assert_eq!(eth_wei_from_rpc_result(&json!("not-hex")), None);
        assert_eq!(eth_wei_from_rpc_result(&json!({"value": 1})), None);
    }

    #[test]
    fn sol_lamports_from_rpc_parses_value_field() {
        assert_eq!(
            sol_lamports_from_rpc_result(&json!({"value": 1_500_000_000u64, "context": {}})),
            Some(1_500_000_000)
        );
        assert_eq!(sol_lamports_from_rpc_result(&json!({"value": "nope"})), None);
        assert_eq!(sol_lamports_from_rpc_result(&json!("0x1")), None);
    }

    #[test]
    fn sui_coin_rows_and_sui_display() {
        let payload = json!([
            {
                "coinType": "0x2::sui::SUI",
                "coinObjectCount": 2,
                "totalBalance": "1500000000"
            },
            {
                "coinType": "0xabc::usdc::USDC",
                "coinObjectCount": 1,
                "totalBalance": "42"
            }
        ]);
        let rows = sui_coin_rows_from_rpc_result(&payload).expect("array");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].coin_object_count, 2);
        assert_eq!(format_sui_coin_balance_display(&rows[0]), "1.5000 SUI");
        assert_eq!(format_sui_coin_balance_display(&rows[1]), "42");
    }

    // Edge cases ported from #24 that the coverage above doesn't reach.

    #[test]
    fn sui_coin_rows_empty_array_is_an_empty_wallet_not_missing_data() {
        let rows = sui_coin_rows_from_rpc_result(&json!([])).expect("array body");
        assert!(rows.is_empty());
    }

    #[test]
    fn sui_coin_rows_non_array_body_signals_fallback() {
        // Error-shaped body (issue #2 class): not an array at all.
        let body = json!({ "detail": "Unhandled error in 'suix_getBalance'" });
        assert!(sui_coin_rows_from_rpc_result(&body).is_none());
    }

    #[test]
    fn sui_coin_rows_missing_fields_fall_back_without_panic() {
        let rows = sui_coin_rows_from_rpc_result(&json!([{}])).expect("array body");
        assert_eq!(rows[0].total_balance, "0");
        assert_eq!(rows[0].coin_object_count, 0);
        assert_eq!(rows[0].coin_type, "Unknown");
    }

    #[test]
    fn sui_coin_display_unparseable_native_balance_stays_raw() {
        let payload = json!([
            { "totalBalance": "not-a-number", "coinType": "0x2::sui::SUI" }
        ]);
        let rows = sui_coin_rows_from_rpc_result(&payload).expect("array");
        assert_eq!(format_sui_coin_balance_display(&rows[0]), "not-a-number");
    }

    #[test]
    fn aptos_balance_octas_missing_store_is_none() {
        let body = json!([
            { "type": "0x1::coin::CoinStore<0x1::other::Other>", "data": { "coin": { "value": "999" } } }
        ]);
        assert_eq!(aptos_balance_octas(&body), None);
    }

    #[test]
    fn aptos_balance_octas_error_shaped_body_is_none() {
        // Exact shape documented in issue #2: the REST error body flows into
        // the same code path and must degrade to the raw-JSON fallback.
        let body = json!({ "code": 404, "message": "account not found" });
        assert_eq!(aptos_balance_octas(&body), None);
    }

    #[test]
    fn sui_gas_mist_zero_is_distinct_from_missing() {
        // Zero balance and unknown shape must not be conflated: 0 parses to
        // Some(0), an unrecognized shape parses to None.
        assert_eq!(sui_gas_mist(&json!("0")), Some(0));
        assert_eq!(sui_gas_mist(&json!(null)), None);
    }

    #[test]
    fn eth_balance_display_units_match_prior_precision_fix() {
        // Guards the same path as CLI ETH balance formatting (18 decimals, 4 places).
        let wei = eth_wei_from_rpc_result(&json!("0xde0b6b3a7640000")).unwrap();
        assert_eq!(crate::cli::format::format_units_fixed(wei, 18, 4), "1.0000");
    }

    // Integration-level check that save_current_network/get_current_network
    // (real file I/O under a scratch HOME) feed correctly into resolve_network's
    // precedence, on top of the pure-logic tests above.
    #[test]
    fn resolves_network_with_precedence_end_to_end() {
        use crate::utils;
        use std::sync::Mutex;
        use std::sync::atomic::{AtomicU64, Ordering};

        static ENV_LOCK: Mutex<()> = Mutex::new(());
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        fn unique_dir(tag: &str) -> std::path::PathBuf {
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let mut d = std::env::temp_dir();
            d.push(format!(
                "txio_handler_test_{}_{}_{}",
                tag,
                std::process::id(),
                n
            ));
            std::fs::create_dir_all(&d).unwrap();
            d
        }

        let _g = ENV_LOCK.lock().unwrap();
        let temp_home = unique_dir("res_net");
        let old_home = std::env::var_os("HOME");

        unsafe {
            std::env::set_var("HOME", &temp_home);
        }

        // 1. First run, nothing persisted -> Mainnet
        assert_eq!(
            resolve_network(None, utils::get_current_network().unwrap()),
            Network::Mainnet
        );

        // 2. Persisted network exists -> picks up persisted network
        utils::save_current_network("testnet").unwrap();
        assert_eq!(
            resolve_network(None, utils::get_current_network().unwrap()),
            Network::Testnet
        );

        // 3. Explicit CLI flag -> overrides persisted network
        assert_eq!(
            resolve_network(Some(Network::Devnet), utils::get_current_network().unwrap()),
            Network::Devnet
        );

        match old_home {
            Some(value) => unsafe { std::env::set_var("HOME", value) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }

    #[test]
    fn aptos_balance_octas_parses_coin_store() {
        let value = json!([
            {"type":"0x1::coin::CoinStore<0x1::aptos_coin::AptosCoin>","data":{"coin":{"value":"100000000"}}}
        ]);
        assert_eq!(aptos_balance_octas(&value), Some(100_000_000u128));
    }

    #[test]
    fn aptos_balance_octas_returns_none_when_missing() {
        assert!(aptos_balance_octas(&json!([{"type":"other"}])).is_none());
        assert!(aptos_balance_octas(&json!("not array")).is_none());
    }

    #[test]
    fn aptos_balance_octas_handles_malformed_gracefully() {
        let value = json!([{"type":"0x1::coin::CoinStore<0x1::aptos_coin::AptosCoin>","data":{"coin":{"value":"invalid"}}}]);
        assert_eq!(aptos_balance_octas(&value), None);
    }

    #[test]
    fn sui_gas_mist_parses_string_and_number() {
        assert_eq!(sui_gas_mist(&json!("1000000000")), Some(1_000_000_000u64));
        assert_eq!(
            sui_gas_mist(&json!(1_000_000_000u64)),
            Some(1_000_000_000u64)
        );
        assert_eq!(sui_gas_mist(&json!("0")), Some(0u64));
    }

    #[test]
    fn sui_gas_mist_returns_none_for_invalid() {
        assert!(sui_gas_mist(&json!("not a number")).is_none());
        assert!(sui_gas_mist(&json!({})).is_none());
    }

    #[test]
    fn ethereum_gas_wei_parses_hex() {
        assert_eq!(
            ethereum_gas_wei(&json!("0x3b9aca00")),
            Some(1_000_000_000u128)
        );
        assert_eq!(
            ethereum_gas_wei(&json!("3b9aca00")),
            Some(1_000_000_000u128)
        );
        assert_eq!(ethereum_gas_wei(&json!("0x")), Some(0u128));
    }

    #[test]
    fn ethereum_gas_wei_returns_none_for_invalid() {
        assert!(ethereum_gas_wei(&json!("not hex")).is_none());
        assert!(ethereum_gas_wei(&json!(123u64)).is_none());
    }
}
