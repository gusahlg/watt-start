//! HTTP for the launcher self-update.
//!
//! [`UreqFetcher`] is the production client: ureq 3 on rustls with the webpki root set, gzip
//! decoding, and a `watt-start/<version>` user agent. Connect and response-header timeouts are
//! 10 seconds. Reading a release binary is allowed 10 minutes so a slow download can finish;
//! the header budget is what makes a dead server fail fast.
//!
//! Tests use the [`Fetch`] trait instead of the network.

use std::io::Read;
use std::time::Duration;

use anyhow::{Context as _, Result};

/// A response body the caller reads. HTTP errors (including 404) are `Ok` with a status, so the
/// updater can treat "no manifest" differently from a transport failure.
pub struct Fetched {
    /// HTTP status code.
    pub status: u16,
    /// `Content-Length` when the server sent one and the body is not being decompressed.
    pub content_length: Option<u64>,
    /// The body. Empty when the status is not interesting.
    pub body: Box<dyn Read + Send>,
}

/// One GET. Object-safe so a test can hand the updater a fake.
pub trait Fetch: Send + Sync {
    /// GET `url`. Transport failures are `Err`. An HTTP status, including 404, is `Ok`.
    fn get(&self, url: &str) -> Result<Fetched>;
}

/// ureq with rustls, the webpki roots, and gzip.
pub struct UreqFetcher {
    agent: ureq::Agent,
}

impl UreqFetcher {
    /// Build the client used for manifest and binary downloads.
    pub fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_send_request(Some(Duration::from_secs(10)))
            .timeout_recv_response(Some(Duration::from_secs(10)))
            .timeout_recv_body(Some(Duration::from_secs(10 * 60)))
            .user_agent(format!("watt-start/{}", env!("CARGO_PKG_VERSION")))
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
        }
    }
}

impl Default for UreqFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Fetch for UreqFetcher {
    fn get(&self, url: &str) -> Result<Fetched> {
        let response = self
            .agent
            .get(url)
            .call()
            .with_context(|| format!("cannot fetch {url}"))?;
        let status = response.status().as_u16();
        let content_length = response.body().content_length();
        let body = response.into_body().into_reader();
        Ok(Fetched {
            status,
            content_length,
            body: Box::new(body),
        })
    }
}
