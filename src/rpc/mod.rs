use anyhow::{Context, Result, anyhow};
use reqwest::{Client, Response};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::future::Future;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    pub params: Value,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: u64,
    pub result: Option<Value>,
    pub error: Option<Value>,
}

// ── RPC retry policy ────────────────────────────────────────────────────────
//
// Every default endpoint is a shared, unauthenticated public node that
// routinely rate-limits (HTTP 429) or drops connections. Without retries, a
// single transient failure aborts the whole command, which undermines the
// CLI's promise of predictable, scriptable output.
//
// Policy:
//   * Retry on connection errors, timeouts, truncated bodies, and HTTP
//     429/502/503/504.
//   * Up to `MAX_ATTEMPTS` total attempts (one initial + two retries).
//   * Exponential backoff (500ms → 1s, capped at 2s) with full jitter.
//   * Honor an integer-seconds `Retry-After` header, capped at
//     `MAX_RETRY_AFTER` so a misbehaving endpoint can't stall a command.
//   * Everything else — other 4xx/5xx statuses and JSON-RPC application
//     errors (which arrive as HTTP 200 with an `error` field) — fails
//     immediately, unchanged.
//
// Worst-case added latency is 2 × `MAX_RETRY_AFTER` = 8s, on top of the
// existing 30s single-request timeout.

/// Total attempts per RPC call (initial request plus retries).
pub const MAX_ATTEMPTS: u32 = 3;

/// Base backoff for the first retry.
const BASE_BACKOFF: Duration = Duration::from_millis(500);

/// Ceiling for the computed exponential backoff (before jitter is applied).
const MAX_BACKOFF: Duration = Duration::from_secs(2);

/// Upper bound on how long a `Retry-After` header is honored.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(4);

/// HTTP statuses that indicate a transient, retryable condition.
fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 429 | 502 | 503 | 504)
}

/// Transport-level failures that are transient and safe to retry for read-only
/// RPC calls: connection establishment failures, timeouts, and truncated body
/// reads (connection resets mid-stream).
fn is_retryable_transport_error(err: &reqwest::Error) -> bool {
    err.is_connect() || err.is_timeout() || err.is_body()
}

/// Send a JSON-RPC POST and return the parsed JSON body, retrying transient
/// failures per the module policy above.
pub async fn post_json(
    client: &Client,
    url: &str,
    payload: &Value,
    verbose: bool,
) -> Result<Value> {
    run_with_retry(verbose, || async move {
        client.post(url).json(payload).send().await
    })
    .await
}

/// Send a GET and return the parsed JSON body, with the same retry policy.
pub async fn get_json(client: &Client, url: &str, verbose: bool) -> Result<Value> {
    run_with_retry(verbose, || async move { client.get(url).send().await }).await
}

async fn run_with_retry<F, Fut>(verbose: bool, mut send: F) -> Result<Value>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Response, reqwest::Error>>,
{
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;

        let response = match send().await {
            Ok(response) => response,
            Err(err) => {
                if attempt >= MAX_ATTEMPTS || !is_retryable_transport_error(&err) {
                    return Err(err).context("RPC request failed");
                }
                let delay = backoff_delay(attempt);
                log_retry(verbose, attempt, &format!("transport error: {err}"), delay);
                tokio::time::sleep(delay).await;
                continue;
            }
        };

        let status = response.status();
        if status.is_success() {
            match response.json::<Value>().await {
                Ok(body) => return Ok(body),
                Err(err) => {
                    // A malformed body is fatal; a truncated body (connection
                    // reset) is transient and worth retrying.
                    if attempt >= MAX_ATTEMPTS || !err.is_body() {
                        return Err(err).context("failed to decode RPC response");
                    }
                    let delay = backoff_delay(attempt);
                    log_retry(verbose, attempt, &format!("body read error: {err}"), delay);
                    tokio::time::sleep(delay).await;
                    continue;
                }
            }
        }

        if !is_retryable_status(status) {
            return Err(anyhow!(
                "RPC request failed with HTTP {} {}",
                status.as_u16(),
                status.canonical_reason().unwrap_or("")
            ));
        }

        if attempt >= MAX_ATTEMPTS {
            return Err(anyhow!(
                "RPC request failed with HTTP {} after {attempt} attempts",
                status.as_u16()
            ));
        }

        let delay = retry_after_or_backoff(response.headers(), attempt);
        log_retry(verbose, attempt, &format!("HTTP {}", status.as_u16()), delay);
        tokio::time::sleep(delay).await;
    }
}

fn retry_after_or_backoff(headers: &reqwest::header::HeaderMap, attempt: u32) -> Duration {
    if let Some(raw) = headers.get(reqwest::header::RETRY_AFTER) {
        if let Ok(text) = raw.to_str() {
            // The common rate-limit form is an integer number of seconds.
            if let Ok(seconds) = text.trim().parse::<u64>() {
                return Duration::from_secs(seconds).min(MAX_RETRY_AFTER);
            }
            // An HTTP-date Retry-After is not parsed here; fall back to the
            // computed exponential backoff instead.
        }
    }
    backoff_delay(attempt)
}

fn backoff_delay(attempt: u32) -> Duration {
    let exponent = attempt.saturating_sub(1).min(6);
    let base_ms = BASE_BACKOFF.as_millis() as u64;
    let delay_ms = base_ms
        .saturating_mul(1u64 << exponent)
        .min(MAX_BACKOFF.as_millis() as u64);
    jitter(Duration::from_millis(delay_ms))
}

/// Full jitter: sleep for a random duration in `[0, upper)`. Uses a tiny
/// xorshift PRNG seeded from the clock so we avoid pulling in a `rand`
/// dependency for a couple of sleep durations.
fn jitter(upper: Duration) -> Duration {
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let mut x = seed ^ 0x9E37_79B9_7F4A_7C15;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    let rand = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
    let fraction = (rand as f64) / (u64::MAX as f64);
    upper.mul_f64(fraction)
}

fn log_retry(verbose: bool, attempt: u32, reason: &str, delay: Duration) {
    if verbose {
        eprintln!(
            "[verbose] RPC attempt {attempt} failed ({reason}); retrying in {}ms",
            delay.as_millis()
        );
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// A canned HTTP response to serve from a mock RPC server.
    pub(crate) struct MockResponse {
        pub(crate) status: u16,
        pub(crate) body: String,
        pub(crate) retry_after_seconds: Option<u64>,
    }

    impl MockResponse {
        pub(crate) fn json(status: u16, body: &str) -> Self {
            Self {
                status,
                body: body.to_string(),
                retry_after_seconds: None,
            }
        }
    }

    /// Spawn a mock HTTP server that serves `responses` in order (one per
    /// accepted connection) and then exits. Returns the bound address.
    pub(crate) async fn serve(responses: Vec<MockResponse>) -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for response in responses {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = [0u8; 8192];
                let _ = socket.read(&mut buf).await;

                let mut headers = String::from("content-type: application/json\r\n");
                headers.push_str(&format!("content-length: {}\r\n", response.body.len()));
                if let Some(seconds) = response.retry_after_seconds {
                    headers.push_str(&format!("retry-after: {seconds}\r\n"));
                }
                headers.push_str("connection: close\r\n");

                let raw = format!(
                    "HTTP/1.1 {} {}\r\n{}\r\n{}",
                    response.status,
                    status_reason(response.status),
                    headers,
                    response.body
                );
                let _ = socket.write_all(raw.as_bytes()).await;
            }
        });
        addr
    }

    fn status_reason(code: u16) -> &'static str {
        match code {
            200 => "OK",
            400 => "Bad Request",
            404 => "Not Found",
            429 => "Too Many Requests",
            500 => "Internal Server Error",
            502 => "Bad Gateway",
            503 => "Service Unavailable",
            504 => "Gateway Timeout",
            _ => "Error",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::test_support::{self, MockResponse};
    use serde_json::json;

    #[test]
    fn only_transient_http_statuses_are_retryable() {
        use reqwest::StatusCode;
        assert!(is_retryable_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(is_retryable_status(StatusCode::BAD_GATEWAY));
        assert!(is_retryable_status(StatusCode::SERVICE_UNAVAILABLE));
        assert!(is_retryable_status(StatusCode::GATEWAY_TIMEOUT));
        assert!(!is_retryable_status(StatusCode::BAD_REQUEST));
        assert!(!is_retryable_status(StatusCode::NOT_FOUND));
        assert!(!is_retryable_status(StatusCode::INTERNAL_SERVER_ERROR));
        assert!(!is_retryable_status(StatusCode::OK));
    }

    #[test]
    fn backoff_stays_within_bounds() {
        for attempt in 1..=MAX_ATTEMPTS + 5 {
            let delay = backoff_delay(attempt);
            assert!(delay >= Duration::ZERO);
            assert!(delay <= MAX_BACKOFF);
        }
    }

    #[test]
    fn retry_after_seconds_are_respected_and_capped() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::RETRY_AFTER,
            reqwest::header::HeaderValue::from_static("2"),
        );
        assert_eq!(retry_after_or_backoff(&headers, 1), Duration::from_secs(2));

        headers.insert(
            reqwest::header::RETRY_AFTER,
            reqwest::header::HeaderValue::from_static("9999"),
        );
        assert_eq!(retry_after_or_backoff(&headers, 1), MAX_RETRY_AFTER);
    }

    #[tokio::test]
    async fn retries_transient_429_then_succeeds() {
        let addr = test_support::serve(vec![
            MockResponse::json(429, r#"{"jsonrpc":"2.0","id":1,"result":null}"#),
            MockResponse::json(200, r#"{"jsonrpc":"2.0","id":1,"result":"ok"}"#),
        ])
        .await;

        let client = Client::new();
        let body = post_json(&client, &format!("http://{addr}"), &json!({}), false)
            .await
            .unwrap();
        assert_eq!(body, json!("ok"));
    }

    #[tokio::test]
    async fn exhausts_retries_on_persistent_429() {
        let addr = test_support::serve(vec![
            MockResponse::json(429, "{}"),
            MockResponse::json(429, "{}"),
            MockResponse::json(429, "{}"),
        ])
        .await;

        let client = Client::new();
        let err = post_json(&client, &format!("http://{addr}"), &json!({}), false)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("429"),
            "exhausted retries should surface the HTTP status, got: {err}"
        );
    }

    #[tokio::test]
    async fn non_retryable_status_fails_fast() {
        // A single 404: it must not be retried, so only one response is needed.
        let addr = test_support::serve(vec![MockResponse::json(404, r#"{"error":"not found"}"#)])
            .await;

        let client = Client::new();
        let err = post_json(&client, &format!("http://{addr}"), &json!({}), false)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("404"), "got: {err}");
    }

    #[tokio::test]
    async fn honors_zero_retry_after_for_immediate_retry() {
        let mut first = MockResponse::json(429, "{}");
        first.retry_after_seconds = Some(0);
        let addr = test_support::serve(vec![
            first,
            MockResponse::json(200, r#"{"jsonrpc":"2.0","id":1,"result":"ok"}"#),
        ])
        .await;

        let client = Client::new();
        let body = post_json(&client, &format!("http://{addr}"), &json!({}), false)
            .await
            .unwrap();
        assert_eq!(body, json!("ok"));
    }
}
