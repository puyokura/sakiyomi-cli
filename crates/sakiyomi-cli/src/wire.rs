//! Wire-level constants and request construction.
//!
//! Every value in this module is taken from the shipped Android bundle
//! (`analysis/bundle/index.js`) and recorded with its source line in
//! `analysis/api-contracts.md`. Nothing here is guessed: if a header or a
//! default is not visible in the bundle, it is absent rather than invented.

/// Base URL of the API, as concatenated in the client.
///
/// Evidence: line 377584 `r4 = 'https://sakiyomi.app/api'` is stored into the
/// closure that owns the request helper, and the helper concatenates that
/// constant with the path and no separator (lines 376779-376782).
pub const DEFAULT_BASE: &str = "https://sakiyomi.app/api";

/// Fixed header value the client sends on every request.
///
/// Evidence: line 376798 `'x-prob-display': '100'`. Never varied anywhere in
/// the bundle; a single literal.
pub const PROB_DISPLAY: &str = "100";

/// Value of `x-option-colors`.
///
/// Evidence: line 376798 builds the header as `String(OPTION_COLORS_KNOWN)`,
/// and the theme module binds that export to the number 8 at line 384099
/// (`r0 = 8; var _closure0_slot9 = r0;`) with the getter for
/// `OPTION_COLORS_KNOWN` reading slot 9 (lines 383380-383410).
pub const OPTION_COLORS: &str = "8";

/// Value the bundle uses for `app_version` in bodies and query strings.
///
/// Evidence: `APP_VERSION` is read from a build-info module
/// (lines 410998, 502205, 715319). The literal is not inlined in the bundle,
/// so this is the `versionName` recorded in REPORT.md §1 rather than a value
/// read out of the bundle. Callers can override it.
pub const APP_VERSION: &str = "1.0.42";

/// HTTP method for a call. The bundle's helper defaults to GET when the
/// options object has no `method` key (lines 376788-376793).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Delete,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Delete => "DELETE",
        }
    }
}

/// A fully-resolved request, before it touches the network.
#[derive(Debug, Clone)]
pub struct WireRequest {
    pub method: Method,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub body: Option<serde_json::Value>,
    pub timeout_ms: Option<u64>,
    /// Whether this request changes money or state in a way that must not be
    /// retried. Set by the caller that constructs the request.
    pub money: bool,
}

impl WireRequest {
    pub fn get(path: impl Into<String>) -> Self {
        Self {
            method: Method::Get,
            path: path.into(),
            query: Vec::new(),
            body: None,
            timeout_ms: None,
            money: false,
        }
    }

    pub fn post(path: impl Into<String>) -> Self {
        Self {
            method: Method::Post,
            path: path.into(),
            query: Vec::new(),
            body: None,
            timeout_ms: None,
            money: false,
        }
    }

    pub fn delete(path: impl Into<String>) -> Self {
        Self {
            method: Method::Delete,
            path: path.into(),
            query: Vec::new(),
            body: None,
            timeout_ms: None,
            money: false,
        }
    }

    pub fn query(mut self, key: &str, value: impl Into<String>) -> Self {
        self.query.push((key.to_string(), value.into()));
        self
    }

    pub fn body(mut self, body: serde_json::Value) -> Self {
        self.body = Some(body);
        self
    }

    pub fn timeout_ms(mut self, ms: u64) -> Self {
        self.timeout_ms = Some(ms);
        self
    }

    pub fn money(mut self) -> Self {
        self.money = true;
        self
    }

    /// Serialize the query string the way the client's `qs()` helper does:
    /// `?k=v&k=v` with both sides percent-encoded, appended only when there is
    /// at least one pair (lines 377388-377470).
    pub fn query_string(&self) -> String {
        if self.query.is_empty() {
            return String::new();
        }
        let mut out = String::from("?");
        for (i, (k, v)) in self.query.iter().enumerate() {
            if i > 0 {
                out.push('&');
            }
            out.push_str(&encode(k));
            out.push('=');
            out.push_str(&encode(v));
        }
        out
    }

    /// Full URL: base + path + query. The client concatenates with no
    /// separator, so `path` must begin with `/` (line 376779-376782).
    pub fn url(&self, base: &str) -> String {
        let base = base.trim_end_matches('/');
        format!("{}{}{}", base, self.path, self.query_string())
    }

    /// Headers, exactly as the bundle builds them at lines 376795-376815.
    pub fn headers(&self, token: Option<&str>) -> Vec<(String, String)> {
        let mut h = vec![
            ("content-type".to_string(), "application/json".to_string()),
            ("x-option-colors".to_string(), OPTION_COLORS.to_string()),
            ("x-prob-display".to_string(), PROB_DISPLAY.to_string()),
        ];
        if let Some(t) = token {
            // Evidence: line 376814 concatenates 'Bearer ' with the token.
            h.push(("authorization".to_string(), format!("Bearer {}", t)));
        }
        h
    }
}

/// Percent-encode a query component. `encodeURIComponent` leaves
/// `A-Za-z0-9-_.!~*'()` alone; the client uses it on both key and value.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        let keep = matches!(b,
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9'
            | b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')');
        if keep {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

/// Percent-encode a path segment the way the client does for ids that are
/// interpolated into a URL (it calls `encodeURIComponent` there: lines
/// 496710-496714, 499439-499449).
pub fn encode_path_segment(s: &str) -> String {
    encode(s)
}

/// Coerce an id string to the JSON type the app sends.
///
/// The bundle's ids are JSON numbers (`{"id": 44775, "options":[{"id":59383}]}`,
/// observed live), and `option_id` is passed straight through from that number
/// (bundle 665770-665773). Sending `"59383"` instead of `59383` would be a
/// different request than the app makes, so an all-digit value becomes a number
/// and anything else stays a string rather than being silently mangled.
pub fn id_value(raw: &str) -> serde_json::Value {
    let trimmed = raw.trim();
    if !trimmed.is_empty() && trimmed.bytes().all(|b| b.is_ascii_digit()) {
        if let Ok(n) = trimmed.parse::<u64>() {
            return serde_json::Value::Number(n.into());
        }
    }
    serde_json::Value::String(raw.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_ids_become_json_numbers() {
        assert_eq!(id_value("59383"), serde_json::json!(59383));
        assert_eq!(id_value(" 42 "), serde_json::json!(42));
    }

    #[test]
    fn non_numeric_ids_stay_strings() {
        assert_eq!(id_value("abc"), serde_json::json!("abc"));
        assert_eq!(id_value("12ab"), serde_json::json!("12ab"));
        assert_eq!(id_value(""), serde_json::json!(""));
    }

    #[test]
    fn headers_match_bundle_shape() {
        let req = WireRequest::get("/config");
        let h = req.headers(None);
        assert_eq!(h[0], ("content-type".into(), "application/json".into()));
        assert_eq!(h[1], ("x-option-colors".into(), "8".into()));
        assert_eq!(h[2], ("x-prob-display".into(), "100".into()));
        assert_eq!(h.len(), 3, "no authorization header without a token");
    }

    #[test]
    fn authorization_is_bearer_prefixed() {
        let req = WireRequest::get("/me");
        let h = req.headers(Some("abc123"));
        let auth = h.iter().find(|(k, _)| k == "authorization").unwrap();
        assert_eq!(auth.1, "Bearer abc123");
    }

    #[test]
    fn url_concatenates_without_separator() {
        let req = WireRequest::get("/events");
        assert_eq!(
            req.url("https://sakiyomi.app/api"),
            "https://sakiyomi.app/api/events"
        );
        // A trailing slash on the base must not double up.
        assert_eq!(
            req.url("https://sakiyomi.app/api/"),
            "https://sakiyomi.app/api/events"
        );
    }

    #[test]
    fn query_string_matches_encode_uri_component() {
        let req = WireRequest::get("/events").query("ids", "a,b");
        assert_eq!(req.query_string(), "?ids=a%2Cb");
        let req = WireRequest::get("/subcategories")
            .query("category", "soccer")
            .query("groups", "1");
        assert_eq!(req.query_string(), "?category=soccer&groups=1");
    }

    #[test]
    fn empty_query_produces_no_question_mark() {
        assert_eq!(WireRequest::get("/missions").query_string(), "");
    }

    #[test]
    fn path_segments_are_encoded() {
        assert_eq!(encode_path_segment("a b"), "a%20b");
        assert_eq!(encode_path_segment("user/1"), "user%2F1");
    }
}
