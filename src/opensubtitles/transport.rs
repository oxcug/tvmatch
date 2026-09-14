use super::{Result, fail};
use serde_json::Value;
use std::{
    io::Read,
    time::{Duration, Instant},
};
use ureq::http::Uri;
const API_HOST: &str = "api.opensubtitles.com";
const API_HOSTS: &[&str] = &[API_HOST, "vip-api.opensubtitles.com"];
// Exact reviewed origins only. Unexpected provider CDN hosts require explicit review.
const CDN_HOSTS: &[&str] = &["www.opensubtitles.com"];
const JSON_CAP: usize = 2 * 1024 * 1024;
const REQUEST_CAP: usize = 512;

pub(super) trait Transport {
    fn api(&mut self, path: &str, body: Option<Value>) -> Result<Value>;
    fn content(&mut self, url: &str) -> Result<Vec<u8>>;
    fn authenticated(&self) -> bool;
    fn quota(&mut self) -> Result<Option<u64>> {
        if !self.authenticated() {
            return Ok(None);
        }
        let info = self.api("infos/user", None)?;
        Ok(Some(super::catalog::counter(
            &info["data"]["remaining_downloads"],
        )?))
    }
}

/// Credentials stay only in memory and API headers; no Debug implementation or logger.
pub struct Online {
    api: ureq::Agent,
    cdn: ureq::Agent,
    key: String,
    bearer: Option<String>,
    started: Instant,
    last: Option<Instant>,
    requests: usize,
}
impl Online {
    pub fn from_env() -> Result<Self> {
        let key = std::env::var("OPENSUBTITLES_API_TOKEN").map_err(|_| {
            fail("OPENSUBTITLES_API_TOKEN app Api-Key is required for explicit acquisition")
        })?;
        let bearer = match std::env::var("OPENSUBTITLES_BEARER_TOKEN") {
            Ok(value) => Some(value),
            Err(std::env::VarError::NotPresent) => None,
            Err(_) => return Err(fail("invalid optional account bearer environment value")),
        };
        if !credential(&key) || bearer.as_ref().is_some_and(|b| !credential(b)) {
            return Err(fail("invalid credential header value"));
        }
        // No environment proxy, cookies, compression, redirect middleware or logging subscriber.
        let config = ureq::Agent::config_builder()
            .https_only(true)
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .max_response_header_size(16 * 1024)
            .timeout_global(Some(Duration::from_secs(30)))
            .timeout_connect(Some(Duration::from_secs(10)))
            .build();
        Ok(Self {
            api: config.clone().into(),
            cdn: config.into(),
            key,
            bearer,
            started: Instant::now(),
            last: None,
            requests: 0,
        })
    }
    fn pace(&mut self) -> Result<()> {
        if self.requests >= REQUEST_CAP || self.started.elapsed() > Duration::from_secs(1800) {
            return Err(fail("network request/elapsed budget exhausted"));
        }
        if let Some(last) = self.last
            && let Some(wait) = Duration::from_millis(1100).checked_sub(last.elapsed())
        {
            std::thread::sleep(wait);
        }
        self.last = Some(Instant::now());
        self.requests += 1;
        Ok(())
    }
    fn request(
        &mut self,
        url: &str,
        api: bool,
        body: Option<Vec<u8>>,
        cap: usize,
    ) -> Result<Reply> {
        let host = allowed_url(url, if api { API_HOSTS } else { CDN_HOSTS })?;
        // At most three credential-free CDN GETs for a transient status, on this in-memory URL.
        // API GET 429 handling remains two attempts; a POST is never retried.
        for attempt in 0..if api { 2 } else { 3 } {
            self.pace()?;
            let agent = if api { &self.api } else { &self.cdn };
            let response = if let Some(body) = &body {
                let mut request = agent
                    .post(url)
                    .header("Accept", "application/json")
                    .header("Accept-Encoding", "identity")
                    .header("User-Agent", "tvmatch v0.1.0")
                    .header("Content-Type", "application/json");
                if api {
                    request = request.header("Api-Key", &self.key);
                    if let Some(token) = &self.bearer {
                        request = request.header("Authorization", format!("Bearer {token}"));
                    }
                }
                request.send(body.as_slice())
            } else {
                let mut request = agent
                    .get(url)
                    .header("Accept-Encoding", "identity")
                    .header("User-Agent", "tvmatch v0.1.0");
                if api {
                    request = request
                        .header("Accept", "application/json")
                        .header("Api-Key", &self.key);
                    if let Some(token) = &self.bearer {
                        request = request.header("Authorization", format!("Bearer {token}"));
                    }
                }
                request.call()
            };
            let mut response = response.map_err(|_| {
                fail(if body.is_some() {
                    "API POST transport failure; charge uncertain, no replay permitted"
                } else {
                    "HTTPS GET transport failure (details redacted)"
                })
            })?;
            let status = response.status().as_u16();
            if retry_transient_content(api, body.is_some(), status, attempt) {
                eprintln!(
                    "Reference CDN GET status={status}; same-link retry {}/2 (no POST)",
                    attempt + 1
                );
                std::thread::sleep(Duration::from_millis(250 * (attempt + 1)));
                continue;
            }
            if status == 429
                && body.is_none()
                && attempt == 0
                && let Some(wait) = response
                    .headers()
                    .get("retry-after")
                    .and_then(|h| h.to_str().ok())
                    .and_then(retry_after)
            {
                std::thread::sleep(Duration::from_secs(wait));
                continue;
            }
            if (300..400).contains(&status) && body.is_none() {
                let location = response
                    .headers()
                    .get("location")
                    .and_then(|h| h.to_str().ok())
                    .filter(|s| s.len() <= 4096)
                    .ok_or_else(|| fail("CDN redirect missing bounded location"))?
                    .to_owned();
                return Ok(Reply::Redirect(redirect_url(url, &location)?));
            }
            if status != 200 {
                return Err(fail(&format!(
                    "HTTPS status={status} host={host}; stopped (POST never replayed)"
                )));
            }
            if response
                .headers()
                .get("content-encoding")
                .is_some_and(|h| h != "identity")
            {
                return Err(fail("unexpected content encoding"));
            }
            if api
                && !response
                    .headers()
                    .get("content-type")
                    .and_then(|h| h.to_str().ok())
                    .is_some_and(|s| s.starts_with("application/json"))
            {
                return Err(fail("API response is not JSON content type"));
            }
            if response
                .headers()
                .get("content-length")
                .and_then(|h| h.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .is_some_and(|n| n > cap as u64)
            {
                return Err(fail("HTTP body byte cap exceeded"));
            }
            let mut bytes = Vec::new();
            response
                .body_mut()
                .as_reader()
                .take(cap as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| fail("HTTP body read failed (details redacted)"))?;
            if bytes.len() > cap {
                return Err(fail("HTTP body byte cap exceeded"));
            }
            return Ok(Reply::Bytes(bytes));
        }
        Err(fail("GET retry budget exhausted"))
    }
}
enum Reply {
    Bytes(Vec<u8>),
    Redirect(String),
}
impl Transport for Online {
    fn api(&mut self, path: &str, body: Option<Value>) -> Result<Value> {
        if path.len() > 2048 || path.starts_with('/') || path.contains('#') || path.contains("://")
        {
            return Err(fail("invalid API path"));
        }
        let mut url = format!("https://{API_HOST}/api/v1/{path}");
        let body = body
            .map(|v| serde_json::to_vec(&v).map_err(|_| fail("request JSON serialization failed")))
            .transpose()?;
        for _ in 0..=3 {
            match self.request(&url, true, body.clone(), JSON_CAP)? {
                Reply::Bytes(bytes) => {
                    return serde_json::from_slice(&bytes)
                        .map_err(|_| fail("invalid bounded API JSON (details redacted)"));
                }
                Reply::Redirect(next) => {
                    let host = allowed_url(&next, API_HOSTS)?;
                    let uri: Uri = next.parse().map_err(|_| fail("invalid API redirect"))?;
                    if body.is_some()
                        || !uri.path().starts_with("/api/v1/")
                        || (host == "vip-api.opensubtitles.com" && self.bearer.is_none())
                    {
                        return Err(fail("API redirect refused: endpoint/account boundary"));
                    }
                    url = next;
                }
            }
        }
        Err(fail("API redirect hop limit exceeded"))
    }
    fn content(&mut self, url: &str) -> Result<Vec<u8>> {
        let mut url = url.to_owned();
        for _ in 0..=3 {
            match self.request(&url, false, None, crate::srt::MAX_SRT_BYTES)? {
                // Cache must durably retain bounded raw bytes before strict parsing.
                Reply::Bytes(bytes) => return Ok(bytes),
                Reply::Redirect(next) => {
                    allowed_url(&next, CDN_HOSTS)?;
                    url = next;
                }
            }
        }
        Err(fail("CDN redirect hop limit exceeded"))
    }
    fn authenticated(&self) -> bool {
        self.bearer.is_some()
    }
}
fn retry_transient_content(api: bool, post: bool, status: u16, attempt: u64) -> bool {
    !api && !post && attempt < 2 && matches!(status, 502..=504)
}
// Only our fixed, redacted status messages are inspected, never server bodies or URLs.
pub(super) fn stops_acquisition(error: &super::Failure) -> bool {
    [401, 403, 406, 429]
        .iter()
        .any(|status| error.0.starts_with(&format!("HTTPS status={status} ")))
        || error.0 == "network request/elapsed budget exhausted"
}
fn redirect_url(current: &str, location: &str) -> Result<String> {
    if location.starts_with('/') && !location.starts_with("//") {
        let uri: Uri = current
            .parse()
            .map_err(|_| fail("invalid redirect origin"))?;
        let authority = uri
            .authority()
            .ok_or_else(|| fail("missing redirect origin"))?;
        Ok(format!("https://{authority}{location}"))
    } else if location.starts_with("https://") {
        Ok(location.into())
    } else {
        Err(fail(
            "unsupported relative/downgrade redirect (details redacted)",
        ))
    }
}
fn credential(value: &str) -> bool {
    !value.is_empty() && value.len() <= 8192 && value.bytes().all(|b| (33..=126).contains(&b))
}
fn retry_after(value: &str) -> Option<u64> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    value.parse::<u64>().ok().filter(|n| *n <= 30)
}
fn allowed_url(url: &str, hosts: &[&str]) -> Result<String> {
    if url.len() > 4096
        || url.contains('#')
        || url.contains('@')
        || url.contains('\\')
        || url.chars().any(char::is_control)
    {
        return Err(fail("unsafe URL rejected (details redacted)"));
    }
    let uri: Uri = url
        .parse()
        .map_err(|_| fail("invalid HTTPS URL (details redacted)"))?;
    let host = uri.host().ok_or_else(|| fail("URL missing host"))?;
    if uri.scheme_str() != Some("https") || uri.port_u16().is_some_and(|p| p != 443) {
        return Err(fail("HTTPS port 443 required"));
    }
    if !hosts.contains(&host) {
        // URI host cannot contain signed query material; still only print conventional host chars.
        let safe = host.len() <= 253
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b));
        return Err(fail(&format!(
            "unapproved HTTPS host={}; review required before acquisition",
            if safe { host } else { "[redacted]" }
        )));
    }
    Ok(host.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credential_redirect_and_signed_error_redaction() {
        for url in [
            "http://www.opensubtitles.com/a?secret",
            "https://api.opensubtitles.com.evil/a?secret",
            "https://127.0.0.1/a?secret",
            "https://u:p@www.opensubtitles.com/a?secret",
            "https://www.opensubtitles.com:444/a?secret",
            "https://www.opensubtitles.com/a#secret",
        ] {
            let error = allowed_url(url, CDN_HOSTS).unwrap_err().to_string();
            assert!(!error.contains("secret"));
        }
        assert!(allowed_url("https://api.opensubtitles.com/a", CDN_HOSTS).is_err());
        assert!(allowed_url("https://www.opensubtitles.com/a", API_HOSTS).is_err());
        assert!(allowed_url("https://www.opensubtitles.com/a?q=x", CDN_HOSTS).is_ok());
    }
    #[test]
    fn transient_same_link_gets_bounded_and_never_api_or_post() {
        for status in [502, 503, 504] {
            assert_eq!(
                (0..4)
                    .filter(|attempt| retry_transient_content(false, false, status, *attempt))
                    .count(),
                2
            );
            for attempt in 0..4 {
                assert!(!retry_transient_content(true, false, status, attempt));
                assert!(!retry_transient_content(false, true, status, attempt));
                assert!(!retry_transient_content(true, true, status, attempt));
            }
        }
        for status in [200, 301, 400, 401, 403, 404, 406, 429, 500] {
            assert!(!retry_transient_content(false, false, status, 0));
        }
    }
    #[test]
    fn retry_after_is_bounded_and_numeric() {
        assert_eq!(retry_after("30"), Some(30));
        for v in ["31", "-1", "+1", "99999999999999999999999999", "tomorrow"] {
            assert_eq!(retry_after(v), None);
        }
    }
}
