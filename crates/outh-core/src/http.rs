//! HTTP client factory and retry policy — port of gotohp `core/httpclient.go`.
//!
//! Go built a `net/http` client with connection pooling (100 idle / 10
//! per-host, 90 s idle timeout) and, when a proxy was configured, silently
//! disabled TLS verification (`InsecureSkipVerify = true`). That last part is
//! a deliberate **deviation**: Outh never disables TLS verification, with or
//! without a proxy (CONTRACT.md decision 2). ureq manages its own connection
//! pool internally, which is the pooling equivalent available here; gzip is a
//! ureq default feature and stays enabled, as in Go.
//!
//! The auth agent additionally mirrors `newGoogleAuthHTTPClient`
//! (googleauth.go): redirects are never followed on token-bearing requests
//! and the whole exchange is capped at 30 s.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::{Error, Result};

/// Retry configuration — port of Go `RetryConfig` / `DefaultRetryConfig`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryConfig {
    /// Number of retries *after* the first attempt (Go `MaxRetries`).
    pub max_retries: u32,
    /// Delay before the first retry (Go `InitialDelay`).
    pub initial_delay: Duration,
    /// Cap for the exponential delay (Go `MaxDelay`).
    pub max_delay: Duration,
}

impl Default for RetryConfig {
    /// Go `DefaultRetryConfig`: 3 retries, 1 s → 30 s.
    fn default() -> Self {
        RetryConfig {
            max_retries: 3,
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(30),
        }
    }
}

/// Port of Go `ShouldRetry` for the response side: retry on any transport
/// (network) failure, on a missing response, and on 5xx / 429 statuses.
///
/// Pass `status = None` when no response was received and
/// `transport_failed = true` when the request never completed.
pub fn should_retry(status: Option<u16>, transport_failed: bool) -> bool {
    if transport_failed {
        return true; // Go: network errors should be retried
    }
    match status {
        None => true, // Go: nil response should be retried
        Some(code) => code >= 500 || code == 429,
    }
}

/// Port of Go `CalculateBackoff`: `initial * 2^attempt`, capped at
/// `max_delay`, plus up to 10 % jitter.
pub fn calculate_backoff(attempt: u32, config: &RetryConfig) -> Duration {
    let mut delay = config.initial_delay;
    for _ in 0..attempt {
        delay = delay.saturating_mul(2);
        if delay >= config.max_delay {
            delay = config.max_delay;
            break;
        }
    }
    if delay > config.max_delay {
        delay = config.max_delay;
    }
    let tenth = delay / 10;
    if tenth.is_zero() {
        return delay;
    }
    let jitter_nanos = next_random_u64() % (tenth.as_nanos() as u64).max(1);
    delay + Duration::from_nanos(jitter_nanos)
}

/// Whether an [`Error`] is worth retrying under the Go policy: transport and
/// HTTP failures yes; auth/protocol/config errors no (retrying those cannot
/// help and, for commits, could duplicate media — commit call sites layer
/// their own `Error::CommitAmbiguous` handling on top of this).
fn is_retryable(err: &Error) -> bool {
    matches!(err, Error::Http(_) | Error::Io(_))
}

/// Run `f` until it succeeds, retrying retryable failures with
/// [`RetryConfig::default`] backoff — the placement Go uses for its upload
/// and album retry loops.
pub fn retry<T, F>(f: F) -> Result<T>
where
    F: FnMut() -> Result<T>,
{
    retry_with(&RetryConfig::default(), f)
}

/// [`retry`] with an explicit configuration (also used by tests to keep
/// delays tiny).
pub fn retry_with<T, F>(config: &RetryConfig, mut f: F) -> Result<T>
where
    F: FnMut() -> Result<T>,
{
    let mut attempt: u32 = 0;
    loop {
        match f() {
            Ok(value) => return Ok(value),
            Err(err) => {
                if attempt >= config.max_retries || !is_retryable(&err) {
                    return Err(err);
                }
                std::thread::sleep(calculate_backoff(attempt, config));
                attempt += 1;
            }
        }
    }
}

/// Port of Go `CheckResponse`: 2xx is success, anything else becomes an
/// error carrying the status and body.
pub fn check_response(status: u16, body: &[u8]) -> Result<()> {
    if (200..300).contains(&status) {
        return Ok(());
    }
    Err(Error::Http(format!(
        "request failed with status {}: {}",
        status,
        String::from_utf8_lossy(body)
    )))
}

/// General-purpose agent — port of Go `NewHTTPClientWithProxy`: no overall
/// timeout (cancellation is the caller's job, exactly like Go), gzip on,
/// HTTP status codes are *not* turned into errors so call sites can apply
/// [`should_retry`] / [`check_response`] themselves.
///
/// TLS verification is always on; a proxy changes routing only.
pub fn build_agent(proxy: &str) -> Result<ureq::Agent> {
    build_agent_inner(proxy, false)
}

/// Auth agent — port of Go `newGoogleAuthHTTPClient` (googleauth.go):
/// 30 s overall timeout and redirects are never followed
/// (`rejectGoogleAuthRedirect`): a redirect response is returned to the
/// caller, which reports it as an HTTP error, instead of forwarding a
/// token-bearing request to an untrusted target.
pub fn build_auth_agent(proxy: &str) -> Result<ureq::Agent> {
    build_agent_inner(proxy, true)
}

fn build_agent_inner(proxy: &str, auth: bool) -> Result<ureq::Agent> {
    // http_status_as_error(false): Go callers inspect status codes
    // themselves (ShouldRetry / CheckResponse / the auth exchange's own
    // status check), so ureq must hand responses back untouched.
    let mut builder = ureq::Agent::config_builder().http_status_as_error(false);
    if auth {
        builder = builder
            .timeout_global(Some(Duration::from_secs(30)))
            .max_redirects(0)
            .max_redirects_will_error(false);
    }
    let trimmed = proxy.trim();
    if !trimmed.is_empty() {
        // NOTE (deviation from Go): Go set InsecureSkipVerify=true whenever
        // a proxy was configured. Outh does not — see module docs.
        let parsed = ureq::Proxy::new(trimmed)
            .map_err(|e| Error::Http(format!("invalid proxy URL: {e}")))?;
        builder = builder.proxy(Some(parsed));
    }
    let config = builder.build();
    Ok(config.into())
}

/// Small self-contained PRNG for backoff jitter and Android-ID fallback
/// entropy (the workspace dependency list has no rand crate). Seeded from
/// the wall clock, the process id and a monotonic counter; xorshift64*.
pub(crate) fn next_random_u64() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0x9e37_79b9_7f4a_7c15);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x2545_f491_4f6c_dd1d);
    let counter = COUNTER.fetch_add(0x9e37_79b9_7f4a_7c15, Ordering::Relaxed);
    let mut x = nanos ^ counter ^ (std::process::id() as u64).rotate_left(32);
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    x
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn should_retry_matches_go_table() {
        // Go ShouldRetry: err != nil -> true; resp == nil -> true;
        // 5xx or 429 -> true; anything else -> false.
        assert!(should_retry(None, true));
        assert!(should_retry(None, false));
        assert!(should_retry(Some(500), false));
        assert!(should_retry(Some(503), false));
        assert!(should_retry(Some(429), false));
        assert!(!should_retry(Some(200), false));
        assert!(!should_retry(Some(400), false));
        assert!(!should_retry(Some(404), false));
        // A transport failure dominates even a seen status.
        assert!(should_retry(Some(200), true));
    }

    #[test]
    fn backoff_grows_caps_and_jitters_below_ten_percent() {
        let config = RetryConfig::default();
        let first = calculate_backoff(0, &config);
        assert!(first >= Duration::from_secs(1));
        assert!(first < Duration::from_millis(1100));
        let capped = calculate_backoff(20, &config);
        assert!(capped >= Duration::from_secs(30));
        assert!(capped < Duration::from_secs(33));
    }

    #[test]
    fn retry_succeeds_after_transient_failures() {
        let config = RetryConfig {
            max_retries: 3,
            initial_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(2),
        };
        let calls = Cell::new(0u32);
        let out = retry_with(&config, || {
            calls.set(calls.get() + 1);
            if calls.get() < 3 {
                Err(Error::Http("request failed with status 503".into()))
            } else {
                Ok(42)
            }
        });
        assert_eq!(out.unwrap(), 42);
        assert_eq!(calls.get(), 3);
    }

    #[test]
    fn retry_exhausts_and_returns_last_error() {
        let config = RetryConfig {
            max_retries: 2,
            initial_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(2),
        };
        let calls = Cell::new(0u32);
        let out: Result<()> = retry_with(&config, || {
            calls.set(calls.get() + 1);
            Err(Error::Http("boom".into()))
        });
        assert!(out.is_err());
        // 1 initial attempt + 2 retries, like Go's MaxRetries semantics.
        assert_eq!(calls.get(), 3);
    }

    #[test]
    fn retry_does_not_retry_auth_errors() {
        let calls = Cell::new(0u32);
        let out: Result<()> = retry(|| {
            calls.set(calls.get() + 1);
            Err(Error::BadAuthentication)
        });
        assert!(matches!(out, Err(Error::BadAuthentication)));
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn check_response_accepts_2xx_only() {
        assert!(check_response(200, b"").is_ok());
        assert!(check_response(204, b"").is_ok());
        let err = check_response(404, b"nope").unwrap_err();
        assert!(err.to_string().contains("404"));
    }

    #[test]
    fn agents_build_without_network() {
        // Construction only — no network. A valid proxy parses; agent
        // construction itself never touches the network.
        assert!(build_agent("").is_ok());
        assert!(build_auth_agent("").is_ok());
        assert!(build_agent("http://proxy.example:8080").is_ok());
    }
}
