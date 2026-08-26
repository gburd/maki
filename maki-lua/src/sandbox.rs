//! Optional job isolation. When enabled via the `/sandbox` toggle, jobs run
//! inside a platform sandbox that restricts filesystem writes to the working
//! directory (plus `/tmp`) and blocks network access.
//!
//! Linux uses `bwrap` (bubblewrap); macOS uses `sandbox-exec` (seatbelt). On
//! any other platform wrapping is a no-op and jobs run unsandboxed.

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

/// Sandbox a shell line, or `None` when disabled or unavailable here.
pub fn wrap_shell(cmd: &str, cwd: &Path) -> Option<Command> {
    let shell = if cfg!(windows) { "cmd.exe" } else { "bash" };
    let flag = if cfg!(windows) { "/C" } else { "-c" };
    wrap(&[shell, flag, cmd], cwd)
}

/// Sandbox an argv the caller built itself, or `None` when disabled or
/// unavailable here.
pub fn wrap_argv(argv: &[String], cwd: &Path) -> Option<Command> {
    let borrowed: Vec<&str> = argv.iter().map(String::as_str).collect();
    wrap(&borrowed, cwd)
}

fn wrap(argv: &[&str], cwd: &Path) -> Option<Command> {
    if !is_enabled() {
        return None;
    }
    let cwd = cwd.to_str()?;
    #[cfg(target_os = "linux")]
    {
        Some(bwrap(argv, cwd))
    }
    #[cfg(target_os = "macos")]
    {
        Some(seatbelt(argv, cwd))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (argv, cwd);
        None
    }
}

#[cfg(target_os = "linux")]
fn bwrap(argv: &[&str], cwd: &str) -> Command {
    let mut command = Command::new("bwrap");
    command.args([
        "--ro-bind",
        "/",
        "/",
        "--bind",
        cwd,
        cwd,
        "--bind",
        "/tmp",
        "/tmp",
        "--dev",
        "/dev",
        "--proc",
        "/proc",
        "--unshare-net",
        "--",
    ]);
    command.args(argv);
    command
}

#[cfg(target_os = "macos")]
fn seatbelt(argv: &[&str], cwd: &str) -> Command {
    let profile = format!(
        "(version 1)\n\
         (allow default)\n\
         (deny network*)\n\
         (deny file-write*)\n\
         (allow file-write* (subpath \"{cwd}\"))\n\
         (allow file-write* (subpath \"/tmp\"))\n\
         (allow file-write* (subpath \"/private/tmp\"))"
    );
    let mut command = Command::new("sandbox-exec");
    command.args(["-p", &profile]);
    command.args(argv);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_returns_none_when_disabled() {
        assert!(!is_enabled());
        assert!(wrap_shell("echo hi", Path::new("/tmp")).is_none());
        assert!(wrap_argv(&["echo".to_string()], Path::new("/tmp")).is_none());
    }

    #[test]
    fn toggle_flips_state() {
        let start = is_enabled();
        assert_eq!(toggle(), !start);
        assert_eq!(toggle(), start);
    }
}
