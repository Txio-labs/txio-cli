//! One-line-per-transaction views of `history` results.
//!
//! Each chain returns its own shape. These formatters read the fields that
//! shape is known to carry and return `None` when the value doesn't match, so
//! the caller can fall back to the raw JSON.

use serde_json::Value;

/// Lines for a history result on `chain`, or `None` when the shape is unknown.
pub fn history_lines(chain: &str, value: &Value) -> Option<Vec<String>> {
    let lines = match chain {
        "Sui" => sui(value)?,
        "Solana" => solana(value)?,
        "Aptos" => aptos(value)?,
        "Soroban" => soroban(value)?,
        "Ethereum" => ethereum(value)?,
        _ => return None,
    };
    Some(if lines.is_empty() {
        vec!["No transactions".to_string()]
    } else {
        lines
    })
}

fn sui(value: &Value) -> Option<Vec<String>> {
    value.get("data")?.as_array()?.iter().map(|tx| {
        let ts = tx.get("timestampMs")?.as_str().map(str::to_string).or_else(|| tx.get("timestampMs")?.as_u64().map(|v| v.to_string()))?;
        let secs = ts.parse::<i64>().ok()? / 1000;
        let status = tx.pointer("/effects/status/status").and_then(Value::as_str).unwrap_or("unknown");
        Some(row(&fmt_unix(secs), status_word(status), tx.get("digest")?.as_str()?))
    }).collect()
}

fn solana(value: &Value) -> Option<Vec<String>> {
    value.as_array()?.iter().map(|tx| {
        let when = match tx.get("blockTime").and_then(Value::as_i64) {
            Some(secs) => fmt_unix(secs),
            None => "—".to_string(),
        };
        let status = if tx.get("err").map_or(true, Value::is_null) { "success" } else { "failed" };
        Some(row(&when, status, tx.get("signature")?.as_str()?))
    }).collect()
}

fn aptos(value: &Value) -> Option<Vec<String>> {
    value.as_array()?.iter().map(|tx| {
        // Aptos timestamps are microseconds, sent as strings.
        let secs = tx.get("timestamp")?.as_str()?.parse::<i64>().ok()? / 1_000_000;
        let status = if tx.get("success").and_then(Value::as_bool).unwrap_or(false) { "success" } else { "failed" };
        Some(row(&fmt_unix(secs), status, tx.get("hash")?.as_str()?))
    }).collect()
}

fn soroban(value: &Value) -> Option<Vec<String>> {
    // Horizon wraps the list as _embedded.records.
    value.pointer("/_embedded/records")?.as_array()?.iter().map(|tx| {
        let created = tx.get("created_at")?.as_str()?;
        // "2026-11-22T12:28:51Z" -> "2026-11-23 10:42"
        let when: String = created.chars().take(16).collect::<String>().replace('T', " ");
        let status = if tx.get("successful").and_then(Value::as_bool).unwrap_or(false) { "success" } else { "failed" };
        Some(row(&when, status, tx.get("hash")?.as_str()?))
    }).collect()
}

fn ethereum(value: &Value) -> Option<Vec<String>> {
    // ERC-20 Transfer logs: topics = [Transfer, from, to], data = amount.
    value.as_array()?.iter().map(|log| {
        let block = u64::from_str_radix(log.get("blockNumber")?.as_str()?.trim_start_matches("0x"), 16).ok()?;
        let topics = log.get("topics")?.as_array()?;
        let from = topic_address(topics.get(1)?.as_str()?)?;
        let to = topic_address(topics.get(2)?.as_str()?)?;
        let amount = log.get("data")?.as_str()?;
        let raw = u128::from_str_radix(amount.trim_start_matches("0x"), 16).ok()?;
        Some(format!(
            "block {block:<10}  {}  {} -> {}  amount {raw} (raw token units)",
            short(log.get("transactionHash")?.as_str()?),
            short(&from),
            short(&to),
        ))
    }).collect()
}

fn status_word(status: &str) -> &'static str {
    match status {
        "success" => "success",
        "failure" | "failed" => "failed",
        _ => "unknown",
    }
}

fn row(when: &str, status: &str, hash: &str) -> String {
    format!("{when}  {status:<7}  {hash}")
}

/// Last 20 bytes of a 32-byte topic, as a 0x address.
fn topic_address(topic: &str) -> Option<String> {
    let hex = topic.trim_start_matches("0x");
    if hex.len() < 40 {
        return None;
    }
    Some(format!("0x{}", &hex[hex.len() - 40..]))
}

fn short(s: &str) -> String {
    if s.len() <= 14 {
        s.to_string()
    } else {
        format!("{}…{}", &s[..8], &s[s.len() - 4..])
    }
}

/// `YYYY-MM-DD HH:MM` in UTC for a unix timestamp in seconds.
pub fn fmt_unix(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}",
        rem / 3600,
        (rem % 3600) / 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn formats_unix_time_in_utc() {
        assert_eq!(fmt_unix(0), "1970-01-01 00:00");
        assert_eq!(fmt_unix(1_795_430_531), "2026-11-23 10:42");
    }

    #[test]
    fn solana_signatures_one_line_each() {
        let v = json!([
            {"signature": "5abcdefghijklmnop", "blockTime": 1795430531, "err": null},
            {"signature": "7xyz", "blockTime": null, "err": {"InstructionError": [0, "Custom"]}}
        ]);
        let lines = history_lines("Solana", &v).unwrap();
        assert_eq!(lines[0], "2026-11-23 10:42  success  5abcdefghijklmnop");
        assert_eq!(lines[1], "—  failed   7xyz");
    }

    #[test]
    fn horizon_records_show_date_status_hash() {
        let v = json!({"_embedded": {"records": [
            {"hash": "2e16", "created_at": "2026-11-22T12:28:51Z", "successful": true}
        ]}});
        assert_eq!(history_lines("Soroban", &v).unwrap(), vec!["2026-11-22 12:28  success  2e16"]);
    }

    #[test]
    fn empty_list_says_so_and_unknown_shape_falls_back() {
        assert_eq!(history_lines("Aptos", &json!([])).unwrap(), vec!["No transactions"]);
        assert!(history_lines("Sui", &json!({"unexpected": 1})).is_none());
        assert!(history_lines("Starknet", &json!([])).is_none());
    }
}
