//! Optional bash isolation. When enabled via the `/sandbox` toggle, shell
//! commands run inside a platform sandbox that restricts filesystem writes to
//! the working directory (plus `/tmp`) and blocks network access.
//!
//! Linux uses `bwrap` (bubblewrap); macOS uses `sandbox-exec` (seatbelt). On
//! any other platform, or when the sandbox binary is missing, wrapping is a
//! no-op and commands run unsandboxed.

use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Flip the sandbox on or off. Returns the new state.
pub fn toggle() -> bool {
    !ENABLED.fetch_xor(true, Ordering::Relaxed)
}

pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Build a sandboxed `Command` running `cmd` through the platform shell, or
/// `None` when the sandbox is disabled or unavailable on this platform.
pub fn wrap_command(cmd: &str, cwd: &Path) -> Option<Command> {
    if !is_enabled() {
        return None;
    }
    #[cfg(target_os = "linux")]
    {
        wrap_bwrap(cmd, cwd)
    }
    #[cfg(target_os = "macos")]
    {
        wrap_seatbelt(cmd, cwd)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (cmd, cwd);
        None
    }
}

#[cfg(target_os = "linux")]
fn wrap_bwrap(cmd: &str, cwd: &Path) -> Option<Command> {
    let cwd_str = cwd.to_str()?;
    let mut command = Command::new("bwrap");
    command.args([
        "--ro-bind",
        "/",
        "/",
        "--bind",
        cwd_str,
        cwd_str,
        "--bind",
        "/tmp",
        "/tmp",
        "--dev",
        "/dev",
        "--proc",
        "/proc",
        "--unshare-net",
        "--",
        "bash",
        "-c",
        cmd,
    ]);
    Some(command)
}

#[cfg(target_os = "macos")]
fn wrap_seatbelt(cmd: &str, cwd: &Path) -> Option<Command> {
    let cwd_str = cwd.to_str()?;
    let profile = format!(
        "(version 1)\n\
         (allow default)\n\
         (deny network*)\n\
         (deny file-write*)\n\
         (allow file-write* (subpath \"{cwd_str}\"))\n\
         (allow file-write* (subpath \"/tmp\"))\n\
         (allow file-write* (subpath \"/private/tmp\"))"
    );
    let mut command = Command::new("sandbox-exec");
    command.args(["-p", &profile, "bash", "-c", cmd]);
    Some(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_returns_none_when_disabled() {
        // Default state is disabled.
        assert!(!is_enabled());
        assert!(wrap_command("echo hi", Path::new("/tmp")).is_none());
    }

    #[test]
    fn toggle_flips_state() {
        // Isolated from other tests: save and restore.
        let start = is_enabled();
        let on = toggle();
        assert_eq!(on, !start);
        let off = toggle();
        assert_eq!(off, start);
    }
}
