//! Session-token import. No OAuth runs here.
//!
//! The app's Google sign-in returns its authorization code to a custom scheme
//! only the Android app can receive, and Google refuses every other redirect URI
//! for that client (loopback, LAN IP, https, OOB — each measured). A desktop or
//! server process therefore cannot complete the exchange itself, so this CLI does
//! not try. It imports the token the app already holds, which is a plain
//! `Bearer` string the API accepts (bundle 376798-376815).
//!
//! The value is proven with `GET /me` before it is stored, so a wrong or expired
//! token fails here rather than on the first real command.

use std::io::{BufRead, IsTerminal, Write};

use crate::config::Settings;
use crate::session::Session;

/// Take a token, prove it works, then store it.
pub fn import_token(
    settings: &Settings,
    g: &crate::cli::Global,
    args: &crate::cli::AuthImportArgs,
) -> i32 {
    let token = match args.token.as_deref().map(str::trim) {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => {
            if !args.stdin && std::io::stdin().is_terminal() {
                eprint!("token: ");
                std::io::stderr().flush().ok();
            }
            let mut line = String::new();
            match std::io::stdin().lock().read_line(&mut line) {
                Ok(n) if n > 0 => line.trim().to_string(),
                _ => {
                    eprintln!("error: no token on stdin");
                    eprintln!("hint: `sakiyomi auth import-token --token <value>`");
                    return crate::code::USAGE;
                }
            }
        }
    };

    if token.is_empty() {
        eprintln!("error: the token is empty");
        return crate::code::USAGE;
    }
    // A Bearer token is one opaque string: whitespace means the wrong text was
    // pasted (for example a whole `Authorization:` header).
    if token.split_whitespace().count() != 1 {
        eprintln!("error: the token contains whitespace; paste only the value");
        return crate::code::USAGE;
    }

    if g.dry_run {
        println!(
            "{}",
            crate::output::to_json(&serde_json::json!({
                "dry_run": true,
                "would_verify": "GET /me with the supplied token",
                "would_store": settings.state_path.display().to_string(),
                "token_length": token.chars().count(),
            }))
        );
        return crate::code::OK;
    }

    let transport = crate::transport::Transport::new(
        settings.base.clone(),
        Some(token.clone()),
        std::time::Duration::from_secs(settings.timeout_secs),
    );
    let api = crate::client::Api::new(settings.app_version.clone());
    let req = api.me();

    match transport.send(&req) {
        Ok(r) if r.status == 401 => {
            eprintln!("error: the server rejected this token (HTTP 401)");
            eprintln!("hint: it expired, or it belongs to a different account");
            return crate::code::AUTH;
        }
        Ok(r) if (200..300).contains(&r.status) => {}
        Ok(r) => {
            eprintln!("error: GET /me returned HTTP {}", r.status);
            return crate::code::FAILED;
        }
        Err(e) => {
            eprintln!("error: could not verify the token: {}", e);
            return crate::code::FAILED;
        }
    }

    let session = Session {
        session_token: Some(token),
        auth_provider: None,
        acquired_via: Some("import".into()),
    };

    match crate::session::save(&settings.state_path, &session) {
        Ok(()) => {
            println!(
                "{}",
                crate::output::to_json(&serde_json::json!({
                    "ok": true,
                    "verified": true,
                    "token_length": session.token_len(),
                    "state_path": settings.state_path.display().to_string(),
                }))
            );
            crate::code::OK
        }
        Err(e) => {
            eprintln!("error: the token works but could not be stored: {}", e);
            crate::code::FAILED
        }
    }
}
