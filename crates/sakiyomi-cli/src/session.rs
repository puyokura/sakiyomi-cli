//! Session state: the token the CLI reuses between runs.
//!
//! Mirrors the app's own two keys (`session_token`, `auth_provider`) but stores
//! them in one JSON file with 0600 on Unix, written atomically so an interrupted
//! write cannot leave a half-file. The token value is never printed; only its
//! presence and length.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Persisted session. Field names match the app's SecureStore keys so the two
/// can be reasoned about together.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Session {
    /// The bearer token (app key: `session_token`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_token: Option<String>,
    /// Which login produced it: `null` for email+password, `apple`/`google`
    /// for social (app key: `auth_provider`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_provider: Option<String>,
    /// Where the token came from, for `doctor`: `login` or `injected`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acquired_via: Option<String>,
}

impl Session {
    /// Whether the stored session carries no usable token. Used by the tests
    /// that assert an empty session file round-trips.
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.session_token
            .as_deref()
            .is_none_or(|t| t.trim().is_empty())
    }

    pub fn token(&self) -> Option<&str> {
        self.session_token
            .as_deref()
            .filter(|t| !t.trim().is_empty())
    }

    /// Length only — never the value.
    pub fn token_len(&self) -> usize {
        self.token().map(|t| t.chars().count()).unwrap_or(0)
    }
}

/// Load a session file. A missing file is not an error: it means "not logged
/// in yet". A corrupt file IS an error, because silently discarding it would
/// log the operator out without saying so.
pub fn load(path: &Path) -> Result<Session, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            if text.trim().is_empty() {
                return Ok(Session::default());
            }
            serde_json::from_str(&text).map_err(|e| format!("{}: {}", path.display(), e))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Session::default()),
        Err(e) => Err(format!("{}: {}", path.display(), e)),
    }
}

/// Write atomically: temp file in the same directory, then rename.
pub fn save(path: &Path, session: &Session) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {}", dir.display(), e))?;
        restrict_dir(dir)?;
    }
    let tmp = temp_path(path);
    let text = serde_json::to_string_pretty(session).map_err(|e| e.to_string())?;

    {
        let mut f = std::fs::File::create(&tmp).map_err(|e| format!("{}: {}", tmp.display(), e))?;
        f.write_all(text.as_bytes())
            .map_err(|e| format!("{}: {}", tmp.display(), e))?;
        f.write_all(b"\n")
            .map_err(|e| format!("{}: {}", tmp.display(), e))?;
        f.sync_all()
            .map_err(|e| format!("{}: {}", tmp.display(), e))?;
    }
    restrict_file(&tmp)?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {}", path.display(), e)
    })?;
    Ok(())
}

/// Remove the session file entirely (used by `auth logout --forget`).
pub fn clear(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("{}: {}", path.display(), e)),
    }
}

fn temp_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".tmp");
    path.with_file_name(name)
}

#[cfg(unix)]
fn restrict_file(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("{}: {}", path.display(), e))
}

#[cfg(not(unix))]
fn restrict_file(_path: &Path) -> Result<(), String> {
    // Windows: the file inherits the user profile ACL, which is already
    // per-user. Nothing to do, and nothing to pretend we did.
    Ok(())
}

#[cfg(unix)]
fn restrict_dir(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("{}: {}", path.display(), e))
}

#[cfg(not(unix))]
fn restrict_dir(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// Redaction for anything that might show up in a log line or JSON dump.
#[allow(dead_code)]
pub fn redact(token: Option<&str>) -> String {
    match token {
        Some(t) if !t.is_empty() => format!("***({} chars)", t.chars().count()),
        _ => "(none)".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("sakiyomi-test-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("tmpdir");
        p
    }

    #[test]
    fn roundtrip_preserves_fields() {
        let dir = tmpdir("rt");
        let path = dir.join("session.json");
        let s = Session {
            session_token: Some("abc".into()),
            auth_provider: None,
            acquired_via: Some("login".into()),
        };
        save(&path, &s).expect("save");
        let back = load(&path).expect("load");
        assert_eq!(back.session_token.as_deref(), Some("abc"));
        assert_eq!(back.acquired_via.as_deref(), Some("login"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_is_empty_not_error() {
        let dir = tmpdir("missing");
        let path = dir.join("nope.json");
        let s = load(&path).expect("load");
        assert!(s.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_file_is_an_error() {
        let dir = tmpdir("corrupt");
        let path = dir.join("session.json");
        std::fs::write(&path, "{not json").expect("write");
        assert!(
            load(&path).is_err(),
            "corrupt state must not be silently dropped"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn redaction_never_contains_the_token() {
        let s = redact(Some("supersecret"));
        assert!(!s.contains("supersecret"));
        assert!(s.contains("11 chars"));
        assert_eq!(redact(None), "(none)");
    }

    #[test]
    fn empty_token_counts_as_absent() {
        let s = Session {
            session_token: Some("   ".into()),
            ..Default::default()
        };
        assert!(s.is_empty());
        assert!(s.token().is_none());
    }
}
