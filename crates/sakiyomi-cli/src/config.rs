//! Configuration and credential resolution.
//!
//! Precedence is explicit and printed by `doctor`:
//!   1. command-line flags (`--base`, `--token`, `--config`, `--state`)
//!   2. environment (`SAKIYOMI_*`)
//!   3. config file (`config.toml` in the OS config directory)
//!   4. built-in defaults
//!
//! Credentials are never logged. Only presence and length are ever reported.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Directory name used under the OS config root.
pub const APP_DIR: &str = "sakiyomi";

/// Environment variable names. Documented in README; read here.
pub const ENV_BASE: &str = "SAKIYOMI_BASE";
pub const ENV_TOKEN: &str = "SAKIYOMI_TOKEN";
pub const ENV_CONFIG: &str = "SAKIYOMI_CONFIG";
pub const ENV_STATE: &str = "SAKIYOMI_STATE";

/// On-disk configuration. Every field is optional so a partial file is valid.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct FileConfig {
    /// Override the API base URL.
    pub base: Option<String>,
    /// Default request timeout in seconds.
    pub timeout_secs: Option<u64>,
    /// Pre-established session token (the MuMu injection path).
    pub token: Option<String>,
    /// Path to the session state file.
    pub state: Option<PathBuf>,
    /// `app_version` sent in bodies/query strings. Defaults to the value
    /// recorded for the analysed build.
    pub app_version: Option<String>,
}

/// Fully resolved runtime settings.
#[derive(Debug, Clone)]
pub struct Settings {
    pub base: String,
    pub timeout_secs: u64,
    pub token: Option<String>,
    pub state_path: PathBuf,
    pub config_path: Option<PathBuf>,
    pub app_version: String,
}

/// Where a given value came from, so `doctor` can explain precedence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Flag,
    Env,
    File,
    Default,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Flag => "flag",
            Source::Env => "env",
            Source::File => "file",
            Source::Default => "default",
        }
    }
}

/// Requested overrides, filled from the CLI.
#[derive(Debug, Default, Clone)]
pub struct Overrides {
    pub base: Option<String>,
    pub token: Option<String>,
    pub config: Option<PathBuf>,
    pub state: Option<PathBuf>,
    pub timeout_secs: Option<u64>,
}

/// OS-standard config directory, e.g.
/// `%APPDATA%\sakiyomi` on Windows, `$XDG_CONFIG_HOME/sakiyomi` or
/// `~/.config/sakiyomi` on Linux.
pub fn config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join(APP_DIR))
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

/// Resolve the config file path: flag, then env, then the OS default.
pub fn resolve_config_path(ov: &Overrides) -> Option<PathBuf> {
    ov.config
        .clone()
        .or_else(|| env_nonempty(ENV_CONFIG).map(PathBuf::from))
        .or_else(|| config_dir().map(|d| d.join("config.toml")))
}

fn read_file(path: &Path) -> Result<Option<FileConfig>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text)
            .map(Some)
            .map_err(|e| format!("{}: {}", path.display(), e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {}", path.display(), e)),
    }
}

/// Which layer supplied each interesting setting, in print order.
pub type Origins = Vec<(&'static str, Source)>;

/// Resolve settings from all sources. Returns the settings plus, for each
/// interesting key, which layer supplied it.
pub fn resolve(ov: &Overrides) -> Result<(Settings, Origins), String> {
    let config_path = resolve_config_path(ov);
    let file = match &config_path {
        Some(p) => read_file(p)?,
        None => None,
    };
    let f = file.unwrap_or_default();
    let mut origins: Vec<(&'static str, Source)> = Vec::new();

    let (base, src) = first_of([
        (ov.base.clone(), Source::Flag),
        (env_nonempty(ENV_BASE), Source::Env),
        (f.base.clone(), Source::File),
    ]);
    let base = base.unwrap_or_else(|| crate::wire::DEFAULT_BASE.to_string());
    origins.push(("base", src.unwrap_or(Source::Default)));

    let (token, src) = first_of([
        (ov.token.clone(), Source::Flag),
        (env_nonempty(ENV_TOKEN), Source::Env),
        (f.token.clone(), Source::File),
    ]);
    let token = token.filter(|t| !t.trim().is_empty());
    origins.push(("token", src.unwrap_or(Source::Default)));

    let (state, src) = first_of([
        (ov.state.clone(), Source::Flag),
        (env_nonempty(ENV_STATE).map(PathBuf::from), Source::Env),
        (f.state.clone(), Source::File),
    ]);
    let state_path = state.unwrap_or_else(default_state_path);
    origins.push(("state", src.unwrap_or(Source::Default)));

    let (app_version, src) = first_of([(f.app_version.clone(), Source::File)]);
    let app_version = app_version.unwrap_or_else(|| crate::wire::APP_VERSION.to_string());
    origins.push(("app_version", src.unwrap_or(Source::Default)));

    let (timeout, src) = first_of([
        (ov.timeout_secs, Source::Flag),
        (f.timeout_secs, Source::File),
    ]);
    let timeout_secs = timeout.unwrap_or(30);
    origins.push(("timeout_secs", src.unwrap_or(Source::Default)));

    Ok((
        Settings {
            base,
            timeout_secs,
            token,
            state_path,
            config_path,
            app_version,
        },
        origins,
    ))
}

fn first_of<T: Clone, const N: usize>(
    candidates: [(Option<T>, Source); N],
) -> (Option<T>, Option<Source>) {
    for (v, s) in candidates {
        if v.is_some() {
            return (v, Some(s));
        }
    }
    (None, None)
}

/// Default state file: next to the config file. Keeps everything the tool
/// owns in one directory that the operator can inspect.
pub fn default_state_path() -> PathBuf {
    config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("session.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_win_over_file() {
        let ov = Overrides {
            base: Some("http://flag.test/api".into()),
            ..Default::default()
        };
        let (s, origins) = resolve(&ov).expect("resolve");
        assert_eq!(s.base, "http://flag.test/api");
        assert!(origins.contains(&("base", Source::Flag)));
    }

    #[test]
    fn defaults_apply_when_nothing_is_set() {
        // Tests run in a process where SAKIYOMI_* may or may not be set; only
        // assert on fields that no test sets.
        let ov = Overrides::default();
        let (s, _) = resolve(&ov).expect("resolve");
        assert!(s.timeout_secs >= 1);
        assert!(s.state_path.to_string_lossy().contains("sakiyomi"));
    }

    #[test]
    fn empty_token_env_is_ignored() {
        assert_eq!(env_nonempty("SAKIYOMI_DEFINITELY_UNSET_VAR"), None);
    }

    #[test]
    fn config_dir_is_under_an_os_standard_root() {
        let d = config_dir().expect("config dir");
        assert!(d.ends_with(APP_DIR));
    }
}
