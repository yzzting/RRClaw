//! Daemon mode for RRClaw.
//!
//! Provides background process management so Telegram and other channels
//! continue running after the terminal is closed.

pub mod protocol;

#[cfg(unix)]
pub mod client;
#[cfg(unix)]
pub mod server;

/// Stub: daemon IPC client (Unix only).
#[cfg(not(unix))]
pub mod client {
    pub async fn run_chat() -> color_eyre::eyre::Result<()> {
        color_eyre::eyre::bail!("Daemon mode is only supported on Unix (macOS/Linux)")
    }
}

/// Stub: daemon server worker (Unix only).
#[cfg(not(unix))]
pub mod server {
    pub async fn run_daemon_worker() -> color_eyre::eyre::Result<()> {
        color_eyre::eyre::bail!("Daemon mode is only supported on Unix (macOS/Linux)")
    }
}

use color_eyre::eyre::{eyre, Result};
use std::path::PathBuf;
use tracing::info;

/// Returns `~/.rrclaw/daemon.pid`.
pub fn pid_path() -> Result<PathBuf> {
    Ok(rrclaw_home()?.join("daemon.pid"))
}

/// Returns `~/.rrclaw/daemon.sock`.
pub fn sock_path() -> Result<PathBuf> {
    Ok(rrclaw_home()?.join("daemon.sock"))
}

/// Returns `~/.rrclaw/logs/daemon.log`.
pub fn log_path() -> Result<PathBuf> {
    Ok(rrclaw_home()?.join("logs").join("daemon.log"))
}

/// `~/.rrclaw/`
fn rrclaw_home() -> Result<PathBuf> {
    let base =
        directories::BaseDirs::new().ok_or_else(|| eyre!("Cannot determine home directory"))?;
    Ok(base.home_dir().join(".rrclaw"))
}

// ─── Process helpers ──────────────────────────────────────────────────────────

/// Read PID from the pid file. Returns `None` if file doesn't exist.
fn read_pid(pid_file: &std::path::Path) -> Option<u32> {
    std::fs::read_to_string(pid_file)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
}

/// Check if a process with the given PID is alive (Unix `kill(pid, 0)`).
#[cfg(unix)]
fn is_process_alive(pid: u32) -> bool {
    // SAFETY: signal 0 does not send a signal, only checks if process exists
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

/// Remove stale PID and socket files.
fn cleanup_files(pid_file: &std::path::Path, sock_file: &std::path::Path) {
    let _ = std::fs::remove_file(pid_file);
    let _ = std::fs::remove_file(sock_file);
}

// ─── Public commands ──────────────────────────────────────────────────────────

/// `rrclaw start` — launch daemon in background via re-exec.
#[cfg(unix)]
pub fn start() -> Result<()> {
    let pid_file = pid_path()?;
    let sock_file = sock_path()?;
    let log_file = log_path()?;

    // Check if already running
    if let Some(pid) = read_pid(&pid_file) {
        if is_process_alive(pid) {
            println!("Daemon already running (pid {})", pid);
            return Ok(());
        }
        // Stale PID file
        cleanup_files(&pid_file, &sock_file);
    }

    // Ensure log directory exists
    if let Some(parent) = log_file.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Open log file (append mode)
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file)?;
    let log_err = log.try_clone()?;

    // Re-exec self with internal `--daemon-worker` flag
    let exe = std::env::current_exe()?;
    let child = std::process::Command::new(exe)
        .arg("daemon-worker")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log))
        .stderr(std::process::Stdio::from(log_err))
        .spawn()?;

    let child_pid = child.id();

    // Write PID file
    std::fs::write(&pid_file, child_pid.to_string())?;

    info!("Daemon started (pid {})", child_pid);
    println!("Daemon started (pid {})", child_pid);
    println!("Log: {}", log_file.display());
    println!("Run `rrclaw chat` to start a conversation.");

    Ok(())
}

/// `rrclaw stop` — send SIGTERM to daemon.
#[cfg(unix)]
pub fn stop() -> Result<()> {
    let pid_file = pid_path()?;
    let sock_file = sock_path()?;

    let pid = match read_pid(&pid_file) {
        Some(pid) => pid,
        None => {
            println!("Daemon not running (no pid file)");
            return Ok(());
        }
    };

    if !is_process_alive(pid) {
        println!("Daemon not running (stale pid file, cleaning up)");
        cleanup_files(&pid_file, &sock_file);
        return Ok(());
    }

    // Send SIGTERM
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }

    // Wait up to 5 seconds for process to exit
    for _ in 0..50 {
        if !is_process_alive(pid) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    // If still alive, SIGKILL
    if is_process_alive(pid) {
        unsafe {
            libc::kill(pid as i32, libc::SIGKILL);
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    cleanup_files(&pid_file, &sock_file);
    println!("Daemon stopped (pid {})", pid);

    Ok(())
}

/// `rrclaw restart` — stop then start.
#[cfg(unix)]
pub fn restart() -> Result<()> {
    stop()?;
    start()
}

/// `rrclaw status` — check if daemon is running.
#[cfg(unix)]
pub fn status() -> Result<()> {
    let pid_file = pid_path()?;
    let sock_file = sock_path()?;

    match read_pid(&pid_file) {
        Some(pid) if is_process_alive(pid) => {
            println!("● Daemon running (pid {})", pid);

            // Check if Telegram is configured
            if let Ok(config) = crate::config::Config::load_or_init() {
                if config.telegram.is_some() {
                    println!("  Telegram: enabled");
                } else {
                    println!("  Telegram: not configured");
                }
            }

            if sock_file.exists() {
                println!("  Socket: {}", sock_file.display());
            }
        }
        Some(pid) => {
            println!("○ Daemon not running (stale pid {}, cleaning up)", pid);
            cleanup_files(&pid_file, &sock_file);
        }
        None => {
            println!("○ Daemon not running");
        }
    }

    Ok(())
}

// ─── Non-Unix stubs ───────────────────────────────────────────────────────────

#[cfg(not(unix))]
pub fn start() -> Result<()> {
    color_eyre::eyre::bail!("Daemon mode is only supported on Unix (macOS/Linux)")
}

#[cfg(not(unix))]
pub fn stop() -> Result<()> {
    color_eyre::eyre::bail!("Daemon mode is only supported on Unix (macOS/Linux)")
}

#[cfg(not(unix))]
pub fn restart() -> Result<()> {
    color_eyre::eyre::bail!("Daemon mode is only supported on Unix (macOS/Linux)")
}

#[cfg(not(unix))]
pub fn status() -> Result<()> {
    color_eyre::eyre::bail!("Daemon mode is only supported on Unix (macOS/Linux)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_path_ends_with_daemon_pid() {
        let p = pid_path().unwrap();
        assert!(p.ends_with("daemon.pid"));
    }

    #[test]
    fn sock_path_ends_with_daemon_sock() {
        let p = sock_path().unwrap();
        assert!(p.ends_with("daemon.sock"));
    }

    #[test]
    fn log_path_ends_with_daemon_log() {
        let p = log_path().unwrap();
        assert!(p.ends_with("daemon.log"));
    }

    #[test]
    fn read_pid_nonexistent_returns_none() {
        let p = std::path::Path::new("/tmp/rrclaw-test-nonexistent.pid");
        assert!(read_pid(p).is_none());
    }

    #[test]
    fn read_pid_valid_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "12345").unwrap();
        assert_eq!(read_pid(tmp.path()), Some(12345));
    }

    #[test]
    fn read_pid_invalid_content() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "not-a-number").unwrap();
        assert!(read_pid(tmp.path()).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn is_process_alive_self() {
        let pid = std::process::id();
        assert!(is_process_alive(pid));
    }

    #[cfg(unix)]
    #[test]
    fn is_process_alive_nonexistent() {
        // PID 99999999 is very unlikely to exist
        assert!(!is_process_alive(99999999));
    }

    #[test]
    fn cleanup_files_removes_both() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let pid = tmp_dir.path().join("test.pid");
        let sock = tmp_dir.path().join("test.sock");
        std::fs::write(&pid, "123").unwrap();
        std::fs::write(&sock, "").unwrap();
        cleanup_files(&pid, &sock);
        assert!(!pid.exists());
        assert!(!sock.exists());
    }

    #[test]
    fn cleanup_files_no_error_if_missing() {
        let pid = std::path::Path::new("/tmp/rrclaw-test-missing.pid");
        let sock = std::path::Path::new("/tmp/rrclaw-test-missing.sock");
        cleanup_files(pid, sock); // should not panic
    }
}
