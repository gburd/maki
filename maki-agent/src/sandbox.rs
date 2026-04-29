use std::path::Path;
use std::process::Command as StdCommand;
use std::sync::atomic::{AtomicBool, Ordering};

static ENABLED: AtomicBool = AtomicBool::new(false);

pub fn toggle() -> bool {
    let prev = ENABLED.fetch_xor(true, Ordering::Relaxed);
    !prev
}

pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Wraps a command in a platform-specific sandbox.
/// Returns `None` if sandboxing is not enabled or not available.
pub fn wrap_command(command: &str, cwd: &Path) -> Option<StdCommand> {
    if !is_enabled() {
        return None;
    }

    #[cfg(target_os = "linux")]
    {
        wrap_bwrap(command, cwd)
    }

    #[cfg(target_os = "macos")]
    {
        wrap_seatbelt(command, cwd)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (command, cwd);
        None
    }
}

#[cfg(target_os = "linux")]
fn wrap_bwrap(command: &str, cwd: &Path) -> Option<StdCommand> {
    let cwd_str = cwd.to_str()?;
    let mut cmd = StdCommand::new("bwrap");
    cmd.args([
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
        command,
    ]);
    Some(cmd)
}

#[cfg(target_os = "macos")]
fn wrap_seatbelt(command: &str, cwd: &Path) -> Option<StdCommand> {
    let cwd_str = cwd.to_str()?;
    let profile = format!(
        r#"(version 1)
(allow default)
(deny network*)
(deny file-write*)
(allow file-write* (subpath "{}"))
(allow file-write* (subpath "/tmp"))
(allow file-write* (subpath "/private/tmp"))"#,
        cwd_str
    );
    let mut cmd = StdCommand::new("sandbox-exec");
    cmd.args(["-p", &profile, "bash", "-c", command]);
    Some(cmd)
}
