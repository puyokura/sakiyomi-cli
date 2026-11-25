//! Custom-scheme handler registration for the OAuth redirect.
//!
//! Google will only send the authorization code back to the redirect URI the
//! client registered, which for this app is the custom scheme
//! `app.sakiyomi.mobile:/oauthredirect` (bundle 702228-702234). Loopback
//! redirects are *not* usable: Google blocks them for this client
//! ("The loopback flow has been blocked in order to keep users secure", verified
//! live against all three loopback spellings).
//!
//! So the desktop process must become the handler for that scheme. Then the
//! browser hands the URI straight to us and no code has to be pasted.
//!
//! # Portability
//!
//! Every path and command is overridable by environment variable, so the same
//! binary works on a machine where the defaults are wrong:
//!
//! | Variable | Default (Windows) | Default (Linux) |
//! |---|---|---|
//! | `SAKIYOMI_SCHEME_ROOT` | `HKCU\Software\Classes` | — |
//! | `SAKIYOMI_DESKTOP_DIR` | — | `~/.local/share/applications` |
//! | `SAKIYOMI_XDG_MIME` | — | `xdg-mime` |
//! | `SAKIYOMI_DESKTOP_DATABASE` | — | `update-desktop-database` |
//!
//! Registration overwrites any existing handler and does **not** restore it.
//! That is a deliberate choice for this tool: it is the only way to receive the
//! code, and keeping a stale registration for a rival binary would silently
//! steal the redirect back. `auth unregister` removes ours outright.

/// The scheme the app registers (bundle 702231: `'app.sakiyomi.mobile'`).
pub const SCHEME: &str = "app.sakiyomi.mobile";

/// Environment override for the Windows registry root.
#[cfg_attr(not(windows), allow(dead_code))]
const ENV_SCHEME_ROOT: &str = "SAKIYOMI_SCHEME_ROOT";
/// Environment override for the Linux applications directory.
/// Linux-only: unused on Windows by design, not dead.
#[allow(dead_code)]
const ENV_DESKTOP_DIR: &str = "SAKIYOMI_DESKTOP_DIR";
/// Environment override for the `xdg-mime` binary.
#[allow(dead_code)]
const ENV_XDG_MIME: &str = "SAKIYOMI_XDG_MIME";
/// Environment override for the `update-desktop-database` binary.
#[allow(dead_code)]
const ENV_DESKTOP_DATABASE: &str = "SAKIYOMI_DESKTOP_DATABASE";

/// Relay target recorded at registration time, if any.
/// Both the Windows registry command and the Linux desktop entry embed it.
#[cfg_attr(not(windows), allow(dead_code))]
fn relay_target() -> Option<String> {
    env_override("SAKIYOMI_RELAY")
}

fn env_override(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

/// Absolute path to the running binary, which the handler must invoke.
fn current_exe() -> Result<std::path::PathBuf, String> {
    std::env::current_exe().map_err(|e| format!("cannot determine own path: {}", e))
}

/// Whether our handler is currently registered.
pub fn is_registered(state_path: &std::path::Path) -> bool {
    #[cfg(windows)]
    {
        windows_reg_query(state_path)
    }
    #[cfg(not(windows))]
    {
        linux_is_registered(state_path)
    }
}

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------

#[cfg(windows)]
fn windows_reg_query(state_path: &std::path::Path) -> bool {
    let root =
        env_override(ENV_SCHEME_ROOT).unwrap_or_else(|| "HKCU\\Software\\Classes".to_string());
    let key = format!("{}\\{}", root, SCHEME);
    // A stale registration (pointing at another --state) must not count: the
    // callback would write its result where nobody is listening.
    let command_key = format!("{}\\shell\\open\\command", key);
    let out = match std::process::Command::new("reg")
        .args(["query", &command_key, "/ve"])
        .output()
    {
        Ok(out) if out.status.success() => out,
        _ => return false,
    };
    let text = String::from_utf8_lossy(&out.stdout);
    text.contains(&state_path.display().to_string())
}

#[cfg(windows)]
fn windows_register(state_path: &std::path::Path) -> Result<String, String> {
    let root =
        env_override(ENV_SCHEME_ROOT).unwrap_or_else(|| "HKCU\\Software\\Classes".to_string());
    let exe = current_exe()?;
    let key = format!("{}\\{}", root, SCHEME);
    let command_key = format!("{}\\shell\\open\\command", key);
    // The handler invocation. `"%1"` is replaced by the OS with the full URI.
    // The OS starts the handler without inheriting our environment, so the relay
    // target cannot come from the environment of the registering process. The
    // handler command therefore carries no relay argument, and the browser-side
    // machine reads `SAKIYOMI_RELAY` from its own user environment when it needs
    // to forward. To make that automatic, register with the argument below.
    // `--state` must be baked in: the OS starts the handler with a fresh
    // environment, so a SAKIYOMI_STATE set in the waiting shell is not visible
    // there. Without it the callback writes its result somewhere else and the
    // waiting process times out.
    let mut command = format!(
        "\"{}\" auth callback \"%1\" --state \"{}\"",
        exe.display(),
        state_path.display()
    );
    if let Some(target) = relay_target() {
        command.push_str(&format!(" --relay \"{}\"", target));
    }

    let run = |args: &[&str]| -> Result<(), String> {
        let out = std::process::Command::new("reg")
            .args(args)
            .output()
            .map_err(|e| format!("reg failed: {}", e))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "reg {} failed: {}",
                args[1],
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    };

    run(&["add", &key, "/ve", "/d", "URL:sakiyomi OAuth", "/f"])?;
    run(&["add", &key, "/v", "URL Protocol", "/d", "", "/f"])?;
    run(&["add", &command_key, "/ve", "/d", &command, "/f"])?;

    Ok(format!("registered {} -> {}", key, command))
}

#[cfg(windows)]
fn windows_unregister() -> Result<String, String> {
    let root =
        env_override(ENV_SCHEME_ROOT).unwrap_or_else(|| "HKCU\\Software\\Classes".to_string());
    let key = format!("{}\\{}", root, SCHEME);
    let out = std::process::Command::new("reg")
        .args(["delete", &key, "/f"])
        .output()
        .map_err(|e| format!("reg failed: {}", e))?;
    if out.status.success() {
        Ok(format!("removed {}", key))
    } else {
        Err(format!(
            "reg delete failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

// ---------------------------------------------------------------------------
// Linux / other Unix
// ---------------------------------------------------------------------------

#[cfg(not(windows))]
fn linux_desktop_dir() -> std::path::PathBuf {
    if let Some(d) = env_override(ENV_DESKTOP_DIR) {
        return std::path::PathBuf::from(d);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    std::path::PathBuf::from(home).join(".local/share/applications")
}

#[cfg(not(windows))]
fn desktop_file_path() -> std::path::PathBuf {
    linux_desktop_dir().join("sakiyomi-oauth.desktop")
}

#[cfg(not(windows))]
fn desktop_file_name() -> &'static str {
    "sakiyomi-oauth.desktop"
}

#[cfg(not(windows))]
fn linux_is_registered(state_path: &std::path::Path) -> bool {
    let path = desktop_file_path();
    if path.exists() {
        // A stale entry (pointing at another --state) must not count.
        return match std::fs::read_to_string(&path) {
            Ok(text) => text.contains(&state_path.display().to_string()),
            Err(_) => false,
        };
    }
    // Fall back to asking the mime database, in case the file lives elsewhere.
    let xdg = env_override(ENV_XDG_MIME).unwrap_or_else(|| "xdg-mime".to_string());
    match std::process::Command::new(&xdg)
        .args(["query", "default", &format!("x-scheme-handler/{}", SCHEME)])
        .output()
    {
        Ok(out) if out.status.success() => !String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        _ => false,
    }
}

#[cfg(not(windows))]
fn linux_register(state_path: &std::path::Path) -> Result<String, String> {
    let dir = linux_desktop_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {}", dir.display(), e))?;

    let exe = current_exe()?;
    // %u carries the full URI. The quoting matters: the URI contains a colon
    // and a slash, and the scheme handler receives it verbatim.
    // Same reason as the Windows registry: the OS launches the handler without
    // the waiting shell's environment, so `--state` is baked into the entry.
    let mut exec = format!(
        "{} auth callback %u --state \"{}\"",
        exe.display(),
        state_path.display()
    );
    if let Some(target) = relay_target() {
        exec.push_str(&format!(" --relay \"{}\"", target));
    }

    let contents = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=sakiyomi OAuth callback\n\
         Comment=Receives the app.sakiyomi.mobile OAuth redirect for the sakiyomi CLI\n\
         Exec={}\n\
         Terminal=true\n\
         NoDisplay=true\n\
         MimeType=x-scheme-handler/{};\n\
         Categories=Utility;\n",
        exec, SCHEME
    );

    let path = desktop_file_path();
    std::fs::write(&path, contents).map_err(|e| format!("{}: {}", path.display(), e))?;

    let xdg = env_override(ENV_XDG_MIME).unwrap_or_else(|| "xdg-mime".to_string());
    let out = std::process::Command::new(&xdg)
        .args([
            "default",
            desktop_file_name(),
            &format!("x-scheme-handler/{}", SCHEME),
        ])
        .output()
        .map_err(|e| format!("{}: {}", xdg, e))?;
    if !out.status.success() {
        return Err(format!(
            "{} default failed: {}",
            xdg,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }

    // Refresh the mime cache if the tool is installed; absence is not fatal.
    let db =
        env_override(ENV_DESKTOP_DATABASE).unwrap_or_else(|| "update-desktop-database".to_string());
    let _ = std::process::Command::new(&db).arg(&dir).output();

    Ok(format!(
        "registered x-scheme-handler/{} -> {}",
        SCHEME,
        path.display()
    ))
}

#[cfg(not(windows))]
fn linux_unregister() -> Result<String, String> {
    let path = desktop_file_path();
    let mut removed = false;
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| format!("{}: {}", path.display(), e))?;
        removed = true;
    }
    let xdg = env_override(ENV_XDG_MIME).unwrap_or_else(|| "xdg-mime".to_string());
    // Point the scheme back at nothing recognisable. `xdg-mime default` needs
    // an existing file, so an empty answer is not expressible; deleting our
    // desktop entry is what actually unregisters the handler.
    let _ = std::process::Command::new(&xdg)
        .args([
            "default",
            "nothing.desktop",
            &format!("x-scheme-handler/{}", SCHEME),
        ])
        .output();
    Ok(if removed {
        format!("removed {}", path.display())
    } else {
        "no handler was registered".to_string()
    })
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Register this binary as the `app.sakiyomi.mobile` handler.
pub fn register(state_path: &std::path::Path) -> Result<String, String> {
    #[cfg(windows)]
    {
        windows_register(state_path)
    }
    #[cfg(not(windows))]
    {
        linux_register(state_path)
    }
}

/// Remove our registration.
pub fn unregister() -> Result<String, String> {
    #[cfg(windows)]
    {
        windows_unregister()
    }
    #[cfg(not(windows))]
    {
        linux_unregister()
    }
}

/// Open a URL in the user's browser without blocking.
pub fn open_browser(url: &str) -> Result<(), String> {
    // `SAKIYOMI_BROWSER` overrides the browser on every platform. That is how
    // a machine with a non-default browser (or a headless CI run) stays usable
    // without touching the OS default.
    if let Some(opener) = env_override("SAKIYOMI_BROWSER") {
        return std::process::Command::new(&opener)
            .arg(url)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("could not run {}: {}", opener, e));
    }
    #[cfg(windows)]
    {
        // NOT `cmd /C start "<url>"`: cmd splits on `&`, and an OAuth URL is
        // nothing but `&`-separated parameters, so it would run fragments of the
        // URL as commands. `rundll32 url.dll,FileProtocolHandler` is an ordinary
        // program receiving one argv entry, so the URL arrives intact.
        std::process::Command::new("rundll32.exe")
            .arg("url.dll,FileProtocolHandler")
            .arg(url)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("could not open a browser: {}", e))
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("could not run xdg-open: {}", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheme_is_the_bundle_value() {
        assert_eq!(SCHEME, "app.sakiyomi.mobile");
        // The redirect URI the app builds (702228-702234) is scheme + ':/oauthredirect'.
        assert_eq!(
            format!("{}:/oauthredirect", SCHEME),
            crate::oauth::GOOGLE_REDIRECT_URI
        );
    }

    #[test]
    fn env_overrides_are_honoured() {
        std::env::set_var("SAKIYOMI_TEST_ONLY_UNRELATED", "1");
        assert_eq!(
            env_override("SAKIYOMI_TEST_ONLY_UNRELATED"),
            Some("1".into())
        );
        assert_eq!(env_override("SAKIYOMI_DEFINITELY_UNSET_VAR"), None);
        std::env::remove_var("SAKIYOMI_TEST_ONLY_UNRELATED");
    }
}
