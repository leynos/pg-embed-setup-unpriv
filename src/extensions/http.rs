//! HTTP transport for extension downloads: which URLs may be fetched, how they
//! are rendered without their secrets, and the bounded, retrying GET.
//!
//! Kept apart from acquisition so the policy about URLs is readable on its own
//! and the cache logic is not interleaved with the client.

use std::{
    io::{self, Read},
    time::{Duration, Instant},
};

use color_eyre::eyre::{Report, eyre};
use reqwest::{StatusCode, Url, redirect};
use tracing::{debug, warn};

use super::LOG_TARGET;

/// Overall timeout for one HTTP request.
const HTTP_TIMEOUT: Duration = Duration::from_secs(120);
/// Attempts made for connection failures and 5xx responses.
const HTTP_ATTEMPTS: u32 = 3;
/// Delay before the second attempt; doubles for each later attempt.
const HTTP_BACKOFF: Duration = Duration::from_millis(200);

/// Returns `true` when `url` may be fetched: `https://`, or plain `http://`
/// to a loopback host only.
///
/// # Examples
///
/// ```
/// use pg_embedded_setup_unpriv::extensions::is_permitted_url;
///
/// assert!(is_permitted_url(
///     "https://github.com/leynos/df12-pg-extensions/releases/x.tar.gz"
/// ));
/// assert!(is_permitted_url("http://127.0.0.1:8080/x.tar.gz"));
/// assert!(!is_permitted_url("http://example.com/x.tar.gz"));
/// assert!(!is_permitted_url("ftp://example.com/x.tar.gz"));
/// ```
#[must_use]
pub fn is_permitted_url(url: &str) -> bool {
    Url::parse(url).is_ok_and(|parsed| is_permitted(&parsed))
}

/// Applies the scheme and loopback rules to a parsed URL.
fn is_permitted(url: &Url) -> bool {
    match url.scheme() {
        "https" => true,
        "http" => url.host_str().is_some_and(is_loopback_host),
        _ => false,
    }
}

/// Recognises `localhost` and loopback IP literals (with or without brackets).
fn is_loopback_host(host: &str) -> bool {
    host == "localhost"
        || host
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Renders a URL for a log field or an error message without its secrets.
///
/// A configured URL is consumer-supplied and can carry credentials: userinfo
/// before the host, a signed query parameter, or a token in the fragment. None
/// of that belongs in a log line or an error a caller may print, so only the
/// scheme, host, port and path survive. An unparsable string is reported as
/// `<unparsable url>` rather than echoed, because the parse failure is no
/// guarantee that it holds no secret.
///
/// # Examples
///
/// ```
/// use pg_embedded_setup_unpriv::extensions::redact_url;
///
/// assert_eq!(
///     redact_url("https://user:pw@example.com/a/b.tar.gz?sig=deadbeef#tok"),
///     "https://example.com/a/b.tar.gz"
/// );
/// assert_eq!(redact_url("not a url"), "<unparsable url>");
/// ```
#[must_use]
pub fn redact_url(url: &str) -> String {
    let Ok(parsed) = Url::parse(url) else {
        return "<unparsable url>".to_owned();
    };
    let scheme = parsed.scheme();
    let host = parsed.host_str().unwrap_or("<no host>");
    let path = parsed.path();
    parsed.port().map_or_else(
        || format!("{scheme}://{host}{path}"),
        |port| format!("{scheme}://{host}:{port}{path}"),
    )
}

/// Builds the HTTP client: bounded timeout, redirects only to permitted URLs.
fn build_client() -> Result<reqwest::blocking::Client, Report> {
    let policy = redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= 10 {
            attempt.error("too many redirects")
        } else if is_permitted(attempt.url()) {
            attempt.follow()
        } else {
            attempt.error("redirect to a non-HTTPS URL is not permitted")
        }
    });
    reqwest::blocking::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .redirect(policy)
        .build()
        .map_err(|err| eyre!("cannot build HTTP client: {err}"))
}

/// Performs a bounded HTTPS GET, streaming the body into `writer`.
///
/// Reads at most `cap + 1` bytes so a caller can detect an oversized body by
/// comparing what it received against `cap`. Connection failures and 5xx
/// responses are retried with backoff; 4xx responses are not.
pub(super) fn http_get(url: &str, cap: u64, writer: &mut dyn io::Write) -> Result<u64, Report> {
    // Parsed once here and threaded onwards, so the retry loop and the request
    // work with a validated URL rather than re-parsing a string each time.
    let Some(parsed) = Url::parse(url).ok().filter(is_permitted) else {
        return Err(eyre!(
            "{} is not an https:// URL (loopback http is the only exception)",
            redact_url(url)
        ));
    };
    let client = build_client()?;
    let started = Instant::now();
    let (response, attempts) = fetch_with_retry(&client, &parsed)?;
    let received = io::copy(&mut response.take(cap + 1), writer)
        .map_err(|err| eyre!("body read failed: {err}"))?;
    debug!(
        target: LOG_TARGET,
        url = %redact_url(url),
        bytes = received,
        attempts,
        elapsed_ms = millis(started.elapsed()),
        "http get complete"
    );
    Ok(received)
}

/// Sends the request up to [`HTTP_ATTEMPTS`] times, backing off between
/// transient failures; returns the response and the attempt count.
fn fetch_with_retry(
    client: &reqwest::blocking::Client,
    url: &Url,
) -> Result<(reqwest::blocking::Response, u32), Report> {
    let mut attempt = 1;
    loop {
        match send(client, url) {
            Ok(response) => return Ok((response, attempt)),
            Err(Failure::Permanent(err)) => return Err(err),
            Err(Failure::Transient(err)) if attempt >= HTTP_ATTEMPTS => {
                return Err(err.wrap_err(format!("giving up after {attempt} attempts")));
            }
            Err(Failure::Transient(err)) => {
                let delay = HTTP_BACKOFF * 2_u32.pow(attempt - 1);
                warn!(
                    target: LOG_TARGET,
                    url = %redact_url(url.as_str()),
                    attempt,
                    error = %err,
                    delay_ms = millis(delay),
                    "transient http failure; retrying"
                );
                std::thread::sleep(delay);
                attempt += 1;
            }
        }
    }
}

/// Milliseconds for log fields, saturating rather than truncating.
pub(super) fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Why one attempt failed, and whether another is worth making.
enum Failure {
    Transient(Report),
    Permanent(Report),
}

/// Sends one GET and classifies the result as transient or permanent.
fn send(
    client: &reqwest::blocking::Client,
    url: &Url,
) -> Result<reqwest::blocking::Response, Failure> {
    let response = client.get(url.clone()).send().map_err(|err| {
        if err.is_redirect() {
            Failure::Permanent(eyre!("{err}"))
        } else {
            Failure::Transient(eyre!("{err}"))
        }
    })?;
    let status = response.status();
    if status.is_success() {
        Ok(response)
    } else if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
        Err(Failure::Transient(eyre!("server returned {status}")))
    } else {
        Err(Failure::Permanent(eyre!("server returned {status}")))
    }
}
