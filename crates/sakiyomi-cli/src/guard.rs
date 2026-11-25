//! Money-moving actions and their confirmation gate.
//!
//! Rules enforced here, in this order:
//!   1. `--dry-run` prints the request and returns without any network I/O.
//!   2. Without `--yes`, a human must type `yes` on stdin. Anything else aborts.
//!   3. The request is issued **once** and never retried. [`MoneyGuard::run`]
//!      consumes `self`, so a second send is a compile error, not a runtime bug.
//!   4. Exit codes separate "never sent" from "sent, outcome unknown", because
//!      the operator's next action differs: retrying is safe in the first case
//!      and dangerous in the second.

use std::io::{BufRead, Write};

use crate::client::{classify, ApiError};
use crate::transport::{HttpResponse, Transport, TransportError};
use crate::wire::WireRequest;

/// Exit code meaning: the operator declined the confirmation.
/// Same value as `code::DECLINED`; kept so the guard module is self-contained.
#[allow(dead_code)]
pub const EXIT_DECLINED: i32 = 3;
/// Exit code meaning: the request was definitely not sent.
pub const EXIT_NOT_SENT: i32 = 4;
/// Exit code meaning: the request may have reached the server; do not retry.
pub const EXIT_OUTCOME_UNKNOWN: i32 = 5;

/// Describe exactly what will be sent, with the token redacted to a placeholder.
pub fn describe(req: &WireRequest, base: &str, token: Option<&str>) -> String {
    let mut out = String::new();
    out.push_str(&format!("{} {}\n", req.method.as_str(), req.url(base)));
    out.push_str("headers:\n");
    for (k, v) in req.headers(token.map(|_| "***")) {
        out.push_str(&format!("  {}: {}\n", k, v));
    }
    if let Some(t) = req.timeout_ms {
        out.push_str(&format!("timeout: {} ms\n", t));
    }
    match &req.body {
        Some(b) => out.push_str(&format!(
            "body:\n{}\n",
            serde_json::to_string_pretty(b).unwrap_or_else(|_| b.to_string())
        )),
        None => out.push_str("body: (none)\n"),
    }
    out
}

/// Ask for confirmation. Reads one line; anything but `yes` (case-insensitive,
/// trimmed) declines. EOF also declines.
pub fn confirm<R: BufRead, W: Write>(
    action: &str,
    detail: &str,
    input: &mut R,
    out: &mut W,
) -> std::io::Result<bool> {
    writeln!(out, "About to {}:", action)?;
    for line in detail.lines() {
        writeln!(out, "  {}", line)?;
    }
    write!(out, "Type 'yes' to continue: ")?;
    out.flush()?;

    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        writeln!(out)?;
        writeln!(out, "no input on stdin; aborted")?;
        return Ok(false);
    }
    Ok(line.trim().eq_ignore_ascii_case("yes"))
}

/// A money-moving request that can be executed at most once.
///
/// `run` takes `self`, so the borrow checker — not a runtime check — prevents a
/// second call. There is deliberately no `Clone` and no `Copy`.
pub struct MoneyGuard<'a> {
    transport: &'a Transport,
    req: WireRequest,
    what: String,
}

impl<'a> MoneyGuard<'a> {
    pub fn new(transport: &'a Transport, req: WireRequest, what: impl Into<String>) -> Self {
        Self {
            transport,
            req,
            what: what.into(),
        }
    }

    /// Execute exactly once.
    pub fn run(self) -> Outcome {
        let had_token = self.transport.has_token();
        let sent = self.transport.send(&self.req);
        match sent {
            Ok(r) => match classify(r, had_token, false) {
                Ok(r) => Outcome::Ok(r),
                Err(ApiError::Status {
                    status,
                    message,
                    body,
                }) => Outcome::Rejected {
                    status,
                    body,
                    what: format!("{} ({})", self.what, message),
                },
                Err(ApiError::Unauthorized { body }) => Outcome::Rejected {
                    status: 401,
                    body,
                    what: format!("{} (session invalid)", self.what),
                },
                Err(ApiError::Transport(t)) => classify_transport(t, &self.what),
            },
            Err(t) => classify_transport(t, &self.what),
        }
    }
}

fn classify_transport(t: TransportError, what: &str) -> Outcome {
    if t.definitely_not_sent() {
        Outcome::NotSent(format!("{}: {}", what, t))
    } else {
        Outcome::Unknown(format!("{}: {}", what, t))
    }
}

/// Result of a one-shot money action.
#[derive(Debug)]
pub enum Outcome {
    Ok(HttpResponse),
    /// The server answered and refused. Safe to inspect and decide again.
    Rejected {
        status: u16,
        body: serde_json::Value,
        what: String,
    },
    /// The request never reached the server (connection refused, DNS failure).
    /// Retrying is safe.
    NotSent(String),
    /// The request may have been processed. Retrying is NOT safe.
    Unknown(String),
}

impl Outcome {
    /// Exit code for this outcome, and the message to print.
    pub fn report(&self) -> (i32, String) {
        match self {
            Outcome::Ok(r) => (0, format!("ok: {}", crate::output::to_json(&r.body))),
            Outcome::Rejected { status, body, what } => {
                // A 401 is an authentication failure, not a business rejection:
                // the fix is to log in again, so it gets the auth exit code even
                // though the server did answer.
                let code = if *status == 401 { 6 } else { 1 };
                (
                    code,
                    format!(
                        "rejected by the server: {} -> HTTP {}\n{}",
                        what,
                        status,
                        crate::output::to_json(body)
                    ),
                )
            }
            Outcome::NotSent(m) => (
                EXIT_NOT_SENT,
                format!(
                    "NOT SENT — the request never reached the server ({}).\n\
                     It is safe to retry after fixing the cause.",
                    m
                ),
            ),
            Outcome::Unknown(m) => (
                EXIT_OUTCOME_UNKNOWN,
                format!(
                    "OUTCOME UNKNOWN — the request may have been processed ({}).\n\
                     Do NOT retry blindly: check the position/balance first.",
                    m
                ),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn req() -> WireRequest {
        WireRequest::post("/events/e1/buy")
            .body(serde_json::json!({"option_id": 1, "stake": 100}))
            .money()
    }

    #[test]
    fn describe_redacts_the_token() {
        let d = describe(&req(), "https://x/api", Some("supersecret"));
        assert!(!d.contains("supersecret"), "token must not appear");
        assert!(d.contains("Bearer ***"));
    }

    #[test]
    fn describe_shows_method_url_and_body() {
        let d = describe(&req(), "https://x/api", None);
        assert!(d.contains("POST https://x/api/events/e1/buy"));
        assert!(d.contains("option_id"));
        assert!(d.contains("100"));
    }

    #[test]
    fn describe_omits_authorization_without_a_token() {
        let d = describe(&req(), "https://x/api", None);
        assert!(!d.contains("authorization"));
    }

    #[test]
    fn confirm_accepts_only_yes() {
        for good in [&b"yes\n"[..], b"YES\n", b" yes \n"] {
            let mut out = Vec::new();
            let mut input = Cursor::new(good.to_vec());
            assert!(
                confirm("buy", "POST /buy", &mut input, &mut out).unwrap(),
                "{:?} must confirm",
                String::from_utf8_lossy(good)
            );
        }
        for bad in [&b"y\n"[..], b"no\n", b"\n", b"yes please\n", b"ok\n"] {
            let mut out = Vec::new();
            let mut input = Cursor::new(bad.to_vec());
            assert!(
                !confirm("buy", "POST /buy", &mut input, &mut out).unwrap(),
                "{:?} must not confirm",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn confirm_on_eof_declines() {
        let mut out = Vec::new();
        let mut input = Cursor::new(Vec::new());
        assert!(!confirm("buy", "x", &mut input, &mut out).unwrap());
    }

    #[test]
    fn os_codes_that_prove_nothing_was_sent() {
        // 10061 WSAECONNREFUSED, 111 ECONNREFUSED, 11001 WSAHOST_NOT_FOUND, 113 EHOSTUNREACH.
        for code in [10061, 111, 11001, 113] {
            let e = TransportError::Network {
                message: "localised text that must not matter".into(),
                raw_os_error: Some(code),
            };
            assert!(
                e.definitely_not_sent(),
                "os code {} must count as not sent",
                code
            );
        }
    }

    #[test]
    fn unknown_or_missing_os_code_is_not_assumed_safe() {
        let e = TransportError::Network {
            message: "something else".into(),
            raw_os_error: Some(9999),
        };
        assert!(!e.definitely_not_sent());
        assert!(!TransportError::network("no code available").definitely_not_sent());
    }

    #[test]
    fn timeout_phase_decides_whether_anything_was_sent() {
        // A global timeout can fire at any moment, including after the request
        // was written: never assume it is safe to retry.
        assert!(!TransportError::Timeout {
            message: "Global".into(),
            phase: "global".into(),
        }
        .definitely_not_sent());
        // Recv-phase timeouts mean the request definitely went out.
        assert!(!TransportError::Timeout {
            message: "RecvResponse".into(),
            phase: "recvresponse".into(),
        }
        .definitely_not_sent());
        // A connect or resolve timeout happens before anything is written.
        for phase in ["connect", "resolve"] {
            assert!(
                TransportError::Timeout {
                    message: phase.into(),
                    phase: phase.into(),
                }
                .definitely_not_sent(),
                "{} timeout must count as not sent",
                phase
            );
        }
    }

    #[test]
    fn outcome_exit_codes_are_distinct() {
        let cases = [
            Outcome::NotSent("x".into()),
            Outcome::Unknown("x".into()),
            Outcome::Rejected {
                status: 400,
                body: serde_json::json!({}),
                what: "buy".into(),
            },
            Outcome::Rejected {
                status: 401,
                body: serde_json::json!({"error": "ログインしてください"}),
                what: "buy".into(),
            },
        ];
        let codes: Vec<i32> = cases.iter().map(|c| c.report().0).collect();
        assert_eq!(codes[0], EXIT_NOT_SENT);
        assert_eq!(codes[1], EXIT_OUTCOME_UNKNOWN);
        assert_eq!(codes[2], 1, "a business rejection is a plain failure");
        assert_eq!(codes[3], 6, "a 401 is an auth failure, not a business one");
        assert_ne!(
            codes[0], codes[1],
            "the two failure kinds must be distinguishable"
        );
    }

    #[test]
    fn unknown_outcome_message_says_do_not_retry() {
        let (_, msg) = Outcome::Unknown("reset".into()).report();
        assert!(msg.contains("OUTCOME UNKNOWN"));
        assert!(msg.contains("Do NOT retry"));
    }

    #[test]
    fn not_sent_message_says_retry_is_safe() {
        let (_, msg) = Outcome::NotSent("refused".into()).report();
        assert!(msg.contains("NOT SENT"));
        assert!(msg.contains("safe to retry"));
    }
}
