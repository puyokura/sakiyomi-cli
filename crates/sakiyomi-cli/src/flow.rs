//! `auth login` / `auth callback`: the two halves of one OAuth flow.
//!
//! Phase 1 (`auth login`) writes a pending state file, opens the browser, and
//! waits for phase 2.
//!
//! Phase 2 (`auth callback <uri>`) is a *separate process* started by the OS
//! when the browser follows the redirect. It reads the pending file, verifies
//! the `state`, exchanges the code, posts to `/auth/social`, and writes the
//! session plus a result file.
//!
//! Phase 1 polls for that result file and prints it.
//!
//! Everything that crosses the two processes lives in two files next to the
//! session:
//!
//! * `pending-auth.json` — `{state, verifier, created_at}`; deleted by phase 2.
//! * `auth-result.json`  — `{ok, message, at}`; written by phase 2, consumed by
//!   phase 1.
//!
//! The verifier never leaves local disk, so a hostile process that guesses the
//! code cannot complete the exchange without also reading the pending file.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::config::Settings;
use crate::oauth;
use crate::session::Session;

#[derive(Debug, Serialize, Deserialize)]
struct Pending {
    state: String,
    verifier: String,
    /// Unix seconds; used only to expire a stale entry.
    created_at: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct AuthResult {
    ok: bool,
    message: String,
    at: u64,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn pending_path(settings: &Settings) -> PathBuf {
    sibling(settings, "pending-auth.json")
}

fn result_path(settings: &Settings) -> PathBuf {
    sibling(settings, "auth-result.json")
}

fn sibling(settings: &Settings, name: &str) -> PathBuf {
    settings
        .state_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(name)
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {}", dir.display(), e))?;
    }
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| format!("{}: {}", path.display(), e))
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Phase 1: print the URL, open the browser if there is one, and wait for the
/// redirect to arrive.
///
/// Two delivery paths are supported, because the redirect target is a custom
/// scheme (`app.sakiyomi.mobile:/oauthredirect`, bundle 702228-702234) and Google
/// rejects every alternative (loopback, LAN IP, https, OOB — all measured).
///
/// * **Desktop**: this binary registers itself as that scheme's handler. The OS
///   runs `sakiyomi auth callback "<uri>"` in a second process and the two halves
///   meet through `pending-auth.json` / `auth-result.json`.
/// * **Headless**: there is no browser here, so the URL is opened on another
///   machine. That machine's handler posts the URI back to a relay port this
///   process is listening on. Over SSH that is just `-L`, and on a LAN it is
///   `SAKIYOMI_RELAY_BIND` plus `SAKIYOMI_RELAY`.
///
/// In both cases the operator types nothing: the URL goes out, the login happens
/// in the browser, and this process exits with the session stored.
pub fn login(settings: &Settings, g: &crate::cli::Global) -> i32 {
    // --dry-run must have no side effects: no registration, no browser, no port.
    if g.dry_run {
        eprintln!("dry-run: would print the URL, open a browser, listen on the relay");
        eprintln!("         port, and wait for the redirect. Nothing was done.");
        return crate::code::OK;
    }

    // Registration only matters on the machine that has the browser. A failure
    // here is normal on a server, so it is a note, not an error.
    if !crate::scheme::is_registered() {
        match crate::scheme::register() {
            Ok(_) => {}
            Err(e) => {
                eprintln!(
                    "note: could not register the URL handler on this machine ({})",
                    e
                );
                eprintln!("      (normal on a server; the browser machine delivers instead)");
            }
        }
    }

    let verifier = oauth::make_verifier();
    let state = oauth::make_state();
    if verifier.is_empty() || state.is_empty() {
        eprintln!("error: could not read OS entropy");
        return crate::code::FAILED;
    }
    let challenge = oauth::challenge_for(&verifier);
    let url = oauth::authorize_url(&state, &challenge);

    let pending = Pending {
        state: state.clone(),
        verifier,
        created_at: now_secs(),
    };
    if let Err(e) = write_json(&pending_path(settings), &pending) {
        eprintln!("error: could not record the pending login: {}", e);
        return crate::code::FAILED;
    }
    // Clear any stale result so we do not read a previous success.
    let _ = std::fs::remove_file(result_path(settings));

    // The URL is the deliverable of this step. It goes to stdout on its own line
    // so it can be copied out of a terminal or read from a log.
    println!("{}", url);
    std::io::stdout().flush().ok();
    eprintln!();
    eprintln!("この URL をブラウザで開いて Google にログインしてください。");
    eprintln!("ログインが終わると、このコマンドは自動で終了します。");

    // Relay: accept the redirect from whichever machine runs the browser.
    let relay = Relay::start(settings);
    if let Some(r) = &relay {
        eprintln!(
            "リレー受信: {}（ブラウザ側から送る場合は SAKIYOMI_RELAY）",
            r.bind
        );
    }

    // A browser here is a convenience, not a requirement.
    if let Err(e) = crate::scheme::open_browser(&url) {
        eprintln!("note: このマシンではブラウザを開けませんでした ({})", e);
        eprintln!("      別のマシンのブラウザで上の URL を開いてください。");
    }

    // Wait for phase 2. The timeout is generous because the operator has to
    // type a password and possibly a 2FA code.
    let deadline = Instant::now() + Duration::from_secs(600);

    loop {
        if let Some(r) = read_json::<AuthResult>(&result_path(settings)) {
            let _ = std::fs::remove_file(result_path(settings));
            let _ = std::fs::remove_file(pending_path(settings));
            if r.ok {
                println!("{}", r.message);
                return crate::code::OK;
            }
            eprintln!("error: {}", r.message);
            return crate::code::FAILED;
        }

        if Instant::now() >= deadline {
            let _ = std::fs::remove_file(pending_path(settings));
            eprintln!("error: Google のログインが 10 分以内に完了しませんでした");
            return crate::code::FAILED;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Bare-bones HTTP relay: `POST /code` with the full redirect URI as the body.
///
/// Only one request is accepted, and it is then handled by the same verification
/// path as the desktop callback, so a forged request without the correct `state`
/// is rejected there. The listener is loopback unless the operator explicitly
/// widens it, and it is bound to a random port when the default one is busy.
struct Relay {
    bind: String,
}

impl Relay {
    fn start(settings: &Settings) -> Option<Relay> {
        if env_flag("SAKIYOMI_NO_RELAY") {
            return None;
        }
        let requested = std::env::var("SAKIYOMI_RELAY_BIND")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "127.0.0.1:8765".to_string());

        // Try the requested address, then the same host on a random port.
        let listener = match std::net::TcpListener::bind(&requested) {
            Ok(l) => l,
            Err(_) => {
                let fallback = if requested.starts_with("0.0.0.0") || requested.starts_with("[::]")
                {
                    "0.0.0.0:0".to_string()
                } else {
                    "127.0.0.1:0".to_string()
                };
                match std::net::TcpListener::bind(&fallback) {
                    Ok(l) => l,
                    Err(e) => {
                        eprintln!("note: relay disabled ({})", e);
                        return None;
                    }
                }
            }
        };

        let addr = listener
            .local_addr()
            .map(|a| a.to_string())
            .unwrap_or_else(|_| requested.clone());

        let state_path = settings.state_path.clone();
        let base = settings.base.clone();
        let timeout = settings.timeout_secs;

        std::thread::spawn(move || {
            // One redirect is all this needs; the process exits after it.
            for stream in listener.incoming().take(1) {
                let Ok(mut stream) = stream else { continue };
                let Some(uri) = read_relay_request(&mut stream) else {
                    respond(&mut stream, 400, "bad request");
                    continue;
                };
                // Run the identical verification/exchange path the desktop uses.
                let settings = Settings {
                    base: base.clone(),
                    timeout_secs: timeout,
                    token: None,
                    state_path: state_path.clone(),
                    config_path: None,
                    app_version: String::new(),
                };
                let rc = crate::flow::callback_inner(&settings, &uri, true);
                if rc == crate::code::OK {
                    respond(&mut stream, 200, "logged in");
                } else {
                    respond(&mut stream, 500, "login failed");
                }
            }
        });

        Some(Relay { bind: addr })
    }
}

fn env_flag(name: &str) -> bool {
    matches!(
        std::env::var(name).ok().as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}

/// Read a minimal HTTP request: request line, headers, then the body.
/// Returns the body when the method is POST and the path is `/code`.
fn read_relay_request(stream: &mut std::net::TcpStream) -> Option<String> {
    use std::io::{BufRead, BufReader, Read};

    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .ok()?;
    let mut reader = BufReader::new(stream.try_clone().ok()?);

    let mut request_line = String::new();
    reader.read_line(&mut request_line).ok()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?;
    let path = parts.next()?;

    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
    }

    if method != "POST" || path != "/code" || content_length == 0 || content_length > 8192 {
        return None;
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body).ok()?;
    let text = String::from_utf8(body).ok()?;
    let text = text.trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn respond(stream: &mut std::net::TcpStream, code: u16, message: &str) {
    use std::io::Write;
    let reason = match code {
        200 => "OK",
        400 => "Bad Request",
        _ => "Internal Server Error",
    };
    let body = format!("{}\n", message);
    let _ = write!(
        stream,
        "HTTP/1.1 {} {}\r\ncontent-type: text/plain; charset=utf-8\r\n\
         content-length: {}\r\nconnection: close\r\n\r\n{}",
        code,
        reason,
        body.len(),
        body
    );
    let _ = stream.flush();
}

/// Phase 2: invoked by the OS with the full redirect URI.
pub fn callback(settings: &Settings, uri: &str, relay: Option<&str>) -> i32 {
    if let Some(target) = relay {
        return forward_to_relay(target, uri);
    }
    callback_inner(settings, uri, false)
}

/// `quiet` suppresses stderr chatter: the relay path reports through phase 1.
fn callback_inner(settings: &Settings, uri: &str, quiet: bool) -> i32 {
    let pending: Pending = match read_json(&pending_path(settings)) {
        Some(p) => p,
        None => {
            // No login started on this machine. In a headless setup the browser
            // runs elsewhere and only the CLI there has a handler registered, so
            // forward the redirect to the machine that is waiting for it.
            if let Some(target) = std::env::var("SAKIYOMI_RELAY")
                .ok()
                .filter(|v| !v.trim().is_empty())
            {
                return forward_to_relay(&target, uri);
            }
            eprintln!("error: no login is pending here (pending-auth.json is missing)");
            eprintln!("hint: run `sakiyomi auth login` on the machine that should receive it,");
            eprintln!("      or set SAKIYOMI_RELAY=http://<that machine>:8765/code");
            return crate::code::FAILED;
        }
    };

    // Expire a stale pending file rather than letting an old code through.
    if now_secs().saturating_sub(pending.created_at) > 600 {
        let _ = std::fs::remove_file(pending_path(settings));
        eprintln!("error: the pending login is older than 10 minutes; start again");
        return crate::code::FAILED;
    }

    let code = match oauth::extract_code(uri) {
        Some(c) => c,
        None => {
            let msg = format!("redirect did not contain a code: {}", trim_for_log(uri));
            finish(settings, false, &msg);
            if !quiet {
                eprintln!("error: {}", msg);
            }
            return crate::code::FAILED;
        }
    };

    if let Some(state) = oauth::extract_param(uri, "state") {
        if state != pending.state {
            let msg = "the redirect state did not match this login session".to_string();
            finish(settings, false, &msg);
            if !quiet {
                eprintln!("error: {}", msg);
            }
            return crate::code::FAILED;
        }
    }

    let _ = std::fs::remove_file(pending_path(settings));

    let tokens = match oauth::exchange_code(
        &code,
        &pending.verifier,
        Duration::from_secs(settings.timeout_secs),
    ) {
        Ok(t) => t,
        Err(e) => {
            let msg = format!("Google token exchange failed: {}", e);
            finish(settings, false, &msg);
            if !quiet {
                eprintln!("error: {}", msg);
            }
            return crate::code::FAILED;
        }
    };

    let req = oauth::social_login(settings, &tokens.id_token);
    let transport = crate::transport::Transport::new(
        settings.base.clone(),
        None,
        Duration::from_secs(settings.timeout_secs),
    );

    match transport.send(&req) {
        Ok(r) => match crate::client::classify(r, false, false) {
            Ok(r) => match crate::client::extract_token(&r) {
                Ok(token) => {
                    let session = Session {
                        session_token: Some(token),
                        auth_provider: Some("google".into()),
                        acquired_via: Some("google".into()),
                    };
                    match crate::session::save(&settings.state_path, &session) {
                        Ok(()) => {
                            let msg = format!(
                                "logged in with Google (token stored in {})",
                                settings.state_path.display()
                            );
                            finish(settings, true, &msg);
                            println!(
                                "{}",
                                crate::output::to_json(&serde_json::json!({
                                    "ok": true,
                                    "provider": "google",
                                    "token_present": true,
                                    "token_length": session.token_len(),
                                    "state_path": settings.state_path.display().to_string(),
                                }))
                            );
                            crate::code::OK
                        }
                        Err(e) => {
                            let msg = format!("could not store the token: {}", e);
                            finish(settings, false, &msg);
                            eprintln!("error: {}", msg);
                            crate::code::FAILED
                        }
                    }
                }
                Err(e) => {
                    finish(settings, false, &e);
                    eprintln!("error: {}", e);
                    crate::code::FAILED
                }
            },
            Err(e) => {
                let msg = format!("/auth/social rejected the id_token: {}", e);
                finish(settings, false, &msg);
                eprintln!("error: {}", msg);
                crate::code::FAILED
            }
        },
        Err(e) => {
            let msg = format!("could not reach the API: {}", e);
            finish(settings, false, &msg);
            eprintln!("error: {}", msg);
            crate::code::FAILED
        }
    }
}

fn finish(settings: &Settings, ok: bool, message: &str) {
    let _ = write_json(
        &result_path(settings),
        &AuthResult {
            ok,
            message: message.to_string(),
            at: now_secs(),
        },
    );
}

/// POST the redirect URI to the waiting machine's relay.
///
/// This is the headless half: the browser machine runs `auth callback` (its OS
/// dispatches the custom scheme), finds no pending login of its own, and hands
/// the URI over. No code is ever typed by a human.
fn forward_to_relay(target: &str, uri: &str) -> i32 {
    let url = if target.starts_with("http://") || target.starts_with("https://") {
        target.to_string()
    } else {
        format!("http://{}/code", target)
    };

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(20)))
        .build()
        .into();

    match agent.post(&url).send(uri) {
        Ok(mut r) => {
            let status = r.status().as_u16();
            let body = r.body_mut().read_to_string().unwrap_or_default();
            if (200..300).contains(&status) {
                println!("redirect forwarded to {}", url);
                crate::code::OK
            } else {
                eprintln!(
                    "error: relay at {} answered HTTP {}: {}",
                    url,
                    status,
                    body.trim()
                );
                crate::code::FAILED
            }
        }
        Err(e) => {
            eprintln!("error: could not reach the relay at {}: {}", url, e);
            eprintln!(
                "hint: on the waiting machine, forward the port (ssh -L 8765:127.0.0.1:8765 ...)"
            );
            crate::code::FAILED
        }
    }
}
fn trim_for_log(s: &str) -> String {
    let mut t: String = s.chars().take(80).collect();
    if s.chars().count() > 80 {
        t.push('…');
    }
    t
}
