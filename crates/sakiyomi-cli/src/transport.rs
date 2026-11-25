//! HTTP transport: one ureq agent, explicit status handling, typed errors.
//!
//! Design notes tied to the bundle:
//! * `http_status_as_error(false)` is mandatory. The client distinguishes
//!   outcomes by status and reads `body.error` out of non-2xx bodies
//!   (bundle line 376583-376596), which ureq 3 cannot do if a 4xx becomes
//!   `Error::StatusCode` — that variant carries only the code, not the body.
//! * The client never retries at the transport layer (no loop exists in the
//!   `request()` generator). This module therefore does not retry either.
//!   Money-moving calls must never be replayed; that is enforced by
//!   [`crate::client::Client`], not here.

use std::time::Duration;

use ureq::{Agent, Error as UreqError};

use crate::wire::WireRequest;

/// Result of one HTTP exchange, whether the server liked it or not.
#[derive(Debug)]
pub struct HttpResponse {
    pub status: u16,
    pub body: serde_json::Value,
    /// Raw body text, kept so that a non-JSON error page is still reportable.
    #[allow(dead_code)]
    pub raw: String,
}

/// Failure modes, split the way the bundle splits them.
#[derive(Debug)]
pub enum TransportError {
    /// Network-level failure. The bundle surfaces this as
    /// `ApiError(0, "通信できません。接続を確認してください")`.
    ///
    /// `raw_os_error` is kept because operator-facing message text is localised
    /// (on this machine a refused connection reads 「対象のコンピューターによって
    /// 拒否されたため、接続できませんでした。 (os error 10061)」), so deciding
    /// whether a request was ever sent must key off the numeric code, never off
    /// English substrings.
    Network {
        message: String,
        raw_os_error: Option<i32>,
    },
    /// Timeout. The bundle's user-facing message is the same as for a network
    /// failure, but the *cause* differs and, for money, so does the safety
    /// answer: a timeout during connect/resolve proves no request bytes were
    /// written, while a timeout during receive does not.
    Timeout {
        #[allow(dead_code)]
        message: String,
        /// ureq's timeout phase, lowercased: `connect`, `resolve`, `global`,
        /// `recv_response`, `recv_body`, `send_request`, `send_body`, ...
        phase: String,
    },
    /// The response arrived but was not JSON. The bundle silently substitutes
    /// `{}` here (line 377102-377118); the CLI keeps the raw text instead so a
    /// caller can see what actually came back.
    NotJson { status: u16, raw: String },
}

/// OS codes that prove no connection was established, therefore no request
/// bytes can have reached the server.
///
/// * Windows: `WSAECONNREFUSED` 10061, `WSAEHOSTUNREACH` 10065,
///   `WSAENETUNREACH` 10051, `WSAHOST_NOT_FOUND` 11001.
/// * Unix: `ECONNREFUSED` 111, `EHOSTUNREACH` 113, `ENETUNREACH` 101,
///   `ENOENT` 2 (used by some resolvers for unknown hosts).
const NOT_SENT_OS_CODES: &[i32] = &[10061, 10065, 10051, 11001, 111, 113, 101, 2];

impl TransportError {
    pub fn network(message: impl Into<String>) -> Self {
        TransportError::Network {
            message: message.into(),
            raw_os_error: None,
        }
    }

    /// Whether this error proves the request never left this machine.
    ///
    /// Deliberately conservative: only connection-establishment failures count.
    /// A timeout, a reset mid-flight, or a bodiless response could all mean the
    /// server already acted, so those return `false` and the caller must treat
    /// the outcome as unknown rather than retrying.
    pub fn definitely_not_sent(&self) -> bool {
        match self {
            TransportError::Network { raw_os_error, .. } => raw_os_error
                .map(|c| NOT_SENT_OS_CODES.contains(&c))
                .unwrap_or(false),
            // A timeout before any bytes were written is as safe as a refused
            // connection. ureq reports which phase expired; only resolve and
            // connect are provably pre-send. `global` is NOT included: it can
            // fire at any moment, including after the request was written.
            TransportError::Timeout { phase, .. } => {
                phase.contains("connect") || phase.contains("resolve")
            }
            TransportError::NotJson { .. } => false,
        }
    }
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransportError::Network {
                message,
                raw_os_error,
            } => match raw_os_error {
                Some(c) => write!(f, "network error (os {}): {}", c, message),
                None => write!(f, "network error: {}", message),
            },
            TransportError::Timeout { phase, .. } => {
                // State the consequence, not just the phase name. For an
                // operator the only question that matters is whether anything
                // could have been sent.
                if phase.contains("connect") || phase.contains("resolve") {
                    write!(f, "timeout during {} (nothing was sent)", phase)
                } else {
                    write!(
                        f,
                        "timeout during {} (the request may already have been sent)",
                        phase
                    )
                }
            }
            TransportError::NotJson { status, raw } => {
                write!(
                    f,
                    "HTTP {} returned a non-JSON body ({} bytes)",
                    status,
                    raw.len()
                )
            }
        }
    }
}

impl std::error::Error for TransportError {}

/// A minimal synchronous HTTP client with the bundle's header shape applied.
pub struct Transport {
    agent: Agent,
    base: String,
    token: Option<String>,
}

impl Transport {
    pub fn new(base: impl Into<String>, token: Option<String>, timeout: Duration) -> Self {
        let config = Agent::config_builder()
            .timeout_global(Some(timeout))
            .timeout_connect(Some(Duration::from_secs(10)))
            .http_status_as_error(false)
            .build();
        Self {
            agent: config.into(),
            base: base.into(),
            token,
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    pub fn has_token(&self) -> bool {
        self.token.as_deref().is_some_and(|t| !t.is_empty())
    }

    #[allow(dead_code)]
    pub fn set_token(&mut self, token: Option<String>) {
        self.token = token;
    }

    /// Send one request. Status is not an error here; the caller decides.
    pub fn send(&self, req: &WireRequest) -> Result<HttpResponse, TransportError> {
        let url = req.url(&self.base);
        let headers = req.headers(self.token.as_deref());
        let body_text = req
            .body
            .as_ref()
            .map(|b| serde_json::to_string(b).unwrap_or_else(|_| "{}".to_string()));

        // The builder generics differ between body-less and body-carrying
        // requests, so the two paths are built separately rather than unified.
        let result = match req.method {
            crate::wire::Method::Get => {
                let mut b = self.agent.get(&url);
                for (k, v) in &headers {
                    b = b.header(k, v);
                }
                match req.timeout_ms {
                    Some(ms) => b
                        .config()
                        .timeout_global(Some(Duration::from_millis(ms)))
                        .build()
                        .call(),
                    None => b.call(),
                }
            }
            crate::wire::Method::Delete => {
                let mut b = self.agent.delete(&url);
                for (k, v) in &headers {
                    b = b.header(k, v);
                }
                match req.timeout_ms {
                    Some(ms) => b
                        .config()
                        .timeout_global(Some(Duration::from_millis(ms)))
                        .build()
                        .call(),
                    None => b.call(),
                }
            }
            crate::wire::Method::Post => {
                let mut b = self.agent.post(&url);
                for (k, v) in &headers {
                    b = b.header(k, v);
                }
                // A POST with no body must not claim to carry JSON. Measured
                // against the real server: `POST /login` with `content-length: 0`
                // and `content-type: application/json` is rejected with HTTP 400
                // 「うまく送信できませんでした」, while `{}` reaches the handler and
                // returns the real 401. The bundle likewise omits the body
                // entirely when `options.body` is null (376821-376835), so an
                // absent body is sent as an empty one *without* the JSON
                // content-type header.
                match body_text {
                    Some(payload) => match req.timeout_ms {
                        Some(ms) => b
                            .config()
                            .timeout_global(Some(Duration::from_millis(ms)))
                            .build()
                            .send(payload.as_str()),
                        None => b.send(payload.as_str()),
                    },
                    None => {
                        if let Some(ms) = req.timeout_ms {
                            b = b
                                .config()
                                .timeout_global(Some(Duration::from_millis(ms)))
                                .build();
                        }
                        b.send_empty()
                    }
                }
            }
        };

        let mut resp = match result {
            Ok(r) => r,
            Err(UreqError::Timeout(t)) => {
                // ureq reports the phase as `Timeout::Global`, `Timeout::Connect`,
                // etc. Keep it: reaching the global deadline says nothing about
                // whether bytes were written, but a connect-phase timeout does.
                let phase = format!("{:?}", t).to_ascii_lowercase();
                return Err(TransportError::Timeout {
                    message: format!("{:?}", t),
                    phase,
                });
            }
            Err(UreqError::Io(e)) => {
                return Err(TransportError::Network {
                    message: e.to_string(),
                    raw_os_error: e.raw_os_error(),
                });
            }
            // ureq classifies resolution/connection failures separately; treat
            // both as transport failures carrying the "no socket" code family.
            Err(UreqError::HostNotFound) => {
                return Err(TransportError::Network {
                    message: "host not found".into(),
                    raw_os_error: Some(11001),
                })
            }
            Err(UreqError::ConnectionFailed) => {
                return Err(TransportError::Network {
                    message: "connection failed".into(),
                    raw_os_error: Some(10061),
                })
            }
            Err(e) => {
                return Err(TransportError::Network {
                    message: e.to_string(),
                    raw_os_error: None,
                })
            }
        };

        let status = resp.status().as_u16();
        let raw = resp
            .body_mut()
            .read_to_string()
            .map_err(|e| TransportError::network(format!("reading body: {}", e)))?;

        let body = if raw.trim().is_empty() {
            serde_json::Value::Null
        } else {
            match serde_json::from_str(&raw) {
                Ok(v) => v,
                Err(_) => return Err(TransportError::NotJson { status, raw }),
            }
        };

        Ok(HttpResponse { status, body, raw })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::WireRequest;

    #[test]
    fn base_trailing_slash_is_normalised() {
        let t = Transport::new("https://example.test/api/", None, Duration::from_secs(5));
        let req = WireRequest::get("/config");
        assert_eq!(req.url(t.base()), "https://example.test/api/config");
    }

    #[test]
    fn token_presence_is_reported_without_leaking_it() {
        let t = Transport::new(
            "https://example.test/api",
            Some("secret".into()),
            Duration::from_secs(5),
        );
        assert!(t.has_token());
        let t = Transport::new(
            "https://example.test/api",
            Some(String::new()),
            Duration::from_secs(5),
        );
        assert!(!t.has_token());
        let t = Transport::new("https://example.test/api", None, Duration::from_secs(5));
        assert!(!t.has_token());
    }
}
