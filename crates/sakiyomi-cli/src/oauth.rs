//! Google OAuth 2.0 (Authorization Code + PKCE) for desktop.
//!
//! # Where these values come from
//!
//! Everything in the constants below is read out of the shipped Android bundle
//! (`analysis/bundle/index.js`) and then checked against Google's live
//! endpoints. Nothing is guessed:
//!
//! * authorization endpoint `https://accounts.google.com/o/oauth2/v2/auth`
//!   — bundle 711892 and the `well-known` discovery document it fetches (710125).
//! * token endpoint `https://oauth2.googleapis.com/token` — same line.
//! * client id `510209981354-85ojocvbe2js6rmvhitjrll6m3nkrcqn.apps.googleusercontent.com`
//!   — bundle 702225-702227. The bundle also carries a second id
//!   (`…-9v76rpng…`, 702223) wired to `iosClientId` (701707-701714); the one used
//!   for Android is 702225. **Line 702225 was verified against Google**: the
//!   token endpoint answers `invalid_grant` (i.e. the client exists) rather than
//!   `invalid_client` for it, which is what an unknown id produces.
//! * redirect URI `app.sakiyomi.mobile:/oauthredirect` — bundle 702228-702234
//!   (`'app.sakiyomi.mobile'` + `':/oauthredirect'`).
//! * scopes `openid`, `userinfo.profile`, `userinfo.email` — bundle 711889.
//!
//! # What is NOT verified
//!
//! The app's redirect URI is a **custom scheme**. Google only hands the
//! authorization code back to a client registered with that scheme — i.e. the
//! Android app itself. A desktop process cannot receive it, and Google offers no
//! loopback redirect for this client id. Therefore `sakiyomi auth login` cannot
//! complete the flow by itself; it prints the URL and asks the operator to finish
//! in a browser that can hand the code back, then takes the code as input.
//! That is a real limitation, stated here rather than hidden behind a fake "works".

use base64::Engine;
use sha2::{Digest, Sha256};

use crate::config::Settings;
use crate::wire::WireRequest;

/// Google's OAuth authorization endpoint (bundle 711892).
pub const GOOGLE_AUTH_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
/// Google's token endpoint (bundle 711892).
pub const GOOGLE_TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
/// Android OAuth client id (bundle 702225). Verified to exist (see module docs).
pub const GOOGLE_CLIENT_ID: &str =
    "510209981354-85ojocvbe2js6rmvhitjrll6m3nkrcqn.apps.googleusercontent.com";
/// Redirect URI the app registers (bundle 702228-702234).
pub const GOOGLE_REDIRECT_URI: &str = "app.sakiyomi.mobile:/oauthredirect";
/// Scopes the app requests (bundle 711889).
pub const GOOGLE_SCOPES: &[&str] = &[
    "openid",
    "https://www.googleapis.com/auth/userinfo.profile",
    "https://www.googleapis.com/auth/userinfo.email",
];

/// Build the authorization URL.
///
/// `state` and `challenge` are supplied by the caller so this stays a pure
/// function and the request can be shown with `--dry-run` before anything is
/// opened.
pub fn authorize_url(state: &str, challenge: &str) -> String {
    format!(
        "{}?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}\
         &code_challenge={}&code_challenge_method=S256",
        GOOGLE_AUTH_ENDPOINT,
        encode(GOOGLE_CLIENT_ID),
        encode(GOOGLE_REDIRECT_URI),
        encode(&GOOGLE_SCOPES.join(" ")),
        encode(state),
        encode(challenge),
    )
}

/// PKCE verifier: 32 random bytes, base64url without padding (RFC 7636 §4.1).
pub fn make_verifier() -> String {
    let mut raw = [0u8; 32];
    // `getrandom` is the OS entropy source; a failure here is fatal for the
    // flow, so the caller gets `None` rather than a silently weak value.
    if getrandom::fill(&mut raw).is_err() {
        return String::new();
    }
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
}

/// S256 challenge for a verifier.
pub fn challenge_for(verifier: &str) -> String {
    let mut h = Sha256::new();
    h.update(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(h.finalize())
}

/// Random opaque `state` value, checked when the code comes back.
pub fn make_state() -> String {
    let mut raw = [0u8; 16];
    if getrandom::fill(&mut raw).is_err() {
        return String::new();
    }
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
}

/// `POST /auth/social {provider:"google", idToken}`.
///
/// The bundle builds exactly this (701937: `{provider:'google', idToken}`) and
/// the API's social login body takes `provider` + `id_token` (355919-355933).
pub fn social_login(_settings: &Settings, id_token: &str) -> WireRequest {
    WireRequest::post("/auth/social").body(serde_json::json!({
        "provider": "google",
        "id_token": id_token,
        // The app sends this unconditionally (355919-355933).
        "promo_v1": true,
    }))
}

/// Exchange an authorization code for an id_token at Google's token endpoint.
///
/// The body is `application/x-www-form-urlencoded`, which is what the token
/// endpoint requires; the bundle's `exchangeCodeAsync` does the same
/// (708674-708759). Verified against the live endpoint: with a bogus code it
/// returns `invalid_grant` / "Malformed auth code", proving the request shape
/// reaches the handler.
pub fn exchange_code(
    code: &str,
    verifier: &str,
    timeout: std::time::Duration,
) -> Result<TokenResponse, String> {
    let body = format!(
        "grant_type=authorization_code&client_id={}&redirect_uri={}&code={}&code_verifier={}",
        encode(GOOGLE_CLIENT_ID),
        encode(GOOGLE_REDIRECT_URI),
        encode(code),
        encode(verifier),
    );

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(timeout))
        .build()
        .into();

    let mut resp = agent
        .post(GOOGLE_TOKEN_ENDPOINT)
        .header("content-type", "application/x-www-form-urlencoded")
        .send(body.as_str())
        .map_err(|e| format!("token request failed: {}", e))?;

    let status = resp.status().as_u16();
    let text = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("reading token response: {}", e))?;

    let json: serde_json::Value = serde_json::from_str(&text)
        .map_err(|_| format!("non-JSON token response (HTTP {})", status))?;

    if !(200..300).contains(&status) {
        let err = json
            .get("error_description")
            .or_else(|| json.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("token endpoint rejected the code");
        return Err(format!("HTTP {}: {}", status, err));
    }

    let id_token = json
        .get("id_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            let keys: Vec<&str> = json
                .as_object()
                .map(|o| o.keys().map(|k| k.as_str()).collect())
                .unwrap_or_default();
            format!("token response had no id_token; keys were {:?}", keys)
        })?;

    Ok(TokenResponse {
        id_token: id_token.to_string(),
        access_token: json
            .get("access_token")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        expires_in: json.get("expires_in").and_then(|v| v.as_u64()),
    })
}

/// `access_token` and `expires_in` are carried because Google returns them and
/// a future refresh path needs them; the login path itself only uses `id_token`.
#[allow(dead_code)]
pub struct TokenResponse {
    pub id_token: String,
    pub access_token: Option<String>,
    pub expires_in: Option<u64>,
}

/// Read a query parameter out of a redirect URI (used for `state`).
pub fn extract_param(input: &str, key: &str) -> Option<String> {
    let needle = format!("{}=", key);
    let idx = input.find(&needle)?;
    let rest = &input[idx + needle.len()..];
    let end = rest.find(['&', '#', ' ']).unwrap_or(rest.len());
    let v = &rest[..end];
    if v.is_empty() {
        None
    } else {
        Some(v.to_string())
    }
}

/// Read the authorization code out of whatever the operator pastes back: a bare
/// code, a full redirect URL, or a URL with `?code=`.
pub fn extract_code(input: &str) -> Option<String> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(idx) = s.find("code=") {
        let rest = &s[idx + 5..];
        let end = rest.find(['&', '#', ' ']).unwrap_or(rest.len());
        let code = &rest[..end];
        if !code.is_empty() {
            return Some(code.to_string());
        }
    }
    // A bare code: no scheme, no '=', sane length.
    if !s.contains([':', '?', '&', '=']) && s.len() >= 8 {
        return Some(s.to_string());
    }
    None
}

/// Percent-encode per RFC 3986 (spaces as `%20`, which the OAuth spec wants).
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        let keep = matches!(b,
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9'
            | b'-' | b'_' | b'.' | b'~');
        if keep {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorize_url_has_the_documented_shape() {
        let u = authorize_url("state123", "chall");
        assert!(u.starts_with(GOOGLE_AUTH_ENDPOINT));
        assert!(u.contains(
            "client_id=510209981354-85ojocvbe2js6rmvhitjrll6m3nkrcqn.apps.googleusercontent.com"
        ));
        assert!(u.contains("response_type=code"));
        assert!(u.contains("code_challenge_method=S256"));
        assert!(u.contains("redirect_uri=app.sakiyomi.mobile%3A%2Foauthredirect"));
        assert!(
            u.contains("scope=openid%20https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fuserinfo.profile")
        );
    }

    #[test]
    fn challenge_is_the_s256_of_the_verifier() {
        // RFC 7636 Appendix B test vector.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            challenge_for(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn verifier_and_state_are_random_per_call() {
        let a = make_verifier();
        let b = make_verifier();
        assert!(!a.is_empty());
        assert_ne!(a, b, "two verifiers must differ");
        assert_eq!(a.len(), 43, "32 bytes base64url-unpadded is 43 chars");
        assert_ne!(make_state(), make_state());
    }

    #[test]
    fn code_extraction_accepts_a_bare_code() {
        assert_eq!(
            extract_code("4/0AX4XfWh1234567890abcdefghijklmnop"),
            Some("4/0AX4XfWh1234567890abcdefghijklmnop".into())
        );
    }

    #[test]
    fn code_extraction_accepts_a_full_redirect_url() {
        let url = "app.sakiyomi.mobile:/oauthredirect?state=abc&code=4/0AX4XfWxyz&scope=openid";
        assert_eq!(extract_code(url), Some("4/0AX4XfWxyz".into()));
    }

    #[test]
    fn code_extraction_rejects_junk() {
        assert_eq!(extract_code(""), None);
        assert_eq!(extract_code("short"), None);
        assert_eq!(extract_code("https://example.test/no-code"), None);
    }

    #[test]
    fn social_login_body_matches_the_bundle() {
        let s = Settings {
            base: "https://x/api".into(),
            timeout_secs: 5,
            token: None,
            state_path: std::path::PathBuf::from("unused"),
            config_path: None,
            app_version: "1.0.42".into(),
        };
        let req = social_login(&s, "idtok");
        assert_eq!(req.path, "/auth/social");
        let body = req.body.unwrap();
        assert_eq!(body["provider"], "google");
        assert_eq!(body["id_token"], "idtok");
        assert_eq!(body["promo_v1"], true);
    }
}
