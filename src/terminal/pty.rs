//! PTY (pseudo-terminal) management
//!
//! Creates PTY pair with forkpty and spawns shell in child process.
//! Provides master side read/write and terminal size setting.

#![allow(dead_code)]

use anyhow::{anyhow, Result};
use log::info;
use nix::pty::{forkpty, ForkptyResult, Winsize};
use nix::sys::wait::{waitpid, WaitPidFlag};
use nix::unistd::{ForkResult, Pid};
use std::collections::HashMap;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
/// `/etc/profile.d/ncon-login-env.sh`, written on start (best effort).
///
/// ncon's root/console path spawns `agetty` → `login(1)`, and login calls
/// `clearenv()`; PAM's `pam_env` then only applies `/etc/environment`, which
/// usually carries no locale. The resulting VT shell runs in the C locale, so
/// programs compute UTF-8/wide-character widths byte-wise and their output
/// misaligns (long package names and yay/pacman progress bars are the usual
/// victims). Only fill in what is missing, so explicit settings always win.
const LOGIN_ENV_HELPER: &str = r#"# Written by ncon (regenerated on start; see src/terminal/pty.rs).
# login(1) clears the environment and pam_env only reads /etc/environment, so a
# VT login shell can end up with no locale at all (C locale -> UTF-8 width bugs,
# misaligned output from programs like yay). Restore the system locale, but only
# when nothing set one explicitly.
if [ -z "${LANG:-}" ] && [ -r /etc/locale.conf ]; then
    LANG=$(sed -n 's/^[[:space:]]*LANG=//p' /etc/locale.conf | head -n 1)
    [ -n "$LANG" ] && export LANG
fi
"#;

/// Write the profile.d helper if needed (idempotent, once per process).
fn ensure_login_env_helper() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        const PATH: &str = "/etc/profile.d/ncon-login-env.sh";
        if std::fs::read_to_string(PATH).is_ok_and(|existing| existing == LOGIN_ENV_HELPER) {
            return;
        }
        match std::fs::write(PATH, LOGIN_ENV_HELPER) {
            Ok(()) => {
                let _ = std::fs::set_permissions(PATH, std::fs::Permissions::from_mode(0o644));
                log::info!("Wrote {} (locale for login sessions)", PATH);
            }
            // Not root (or read-only /etc): the direct-fork path already has a
            // full environment, so this is only needed for the login path.
            Err(e) => log::debug!("Cannot write {}: {}", PATH, e),
        }
    });
}

/// PTY management structure
pub struct Pty {
    /// Master side file descriptor
    master: OwnedFd,
    /// Child process PID
    child_pid: Pid,
}

impl Pty {
    /// Create PTY and spawn shell
    ///
    /// Specify initial terminal size with `cols`, `rows`.
    /// `term_env` sets the TERM environment variable.
    pub fn spawn(cols: u16, rows: u16, term_env: &str) -> Result<Self> {
        Self::spawn_with_pixels(cols, rows, 0, 0, term_env, &[])
    }

    /// Create PTY and spawn shell with extra environment variables
    pub fn spawn_with_env(
        cols: u16,
        rows: u16,
        term_env: &str,
        extra_env: &[(&str, &str)],
    ) -> Result<Self> {
        Self::spawn_with_pixels(cols, rows, 0, 0, term_env, extra_env)
    }

    /// Create PTY and spawn user's shell directly (for split panes / new tabs).
    ///
    /// Unlike `spawn()` which runs `/bin/login` when root, this method
    /// drops privileges to the specified user and runs their shell directly.
    /// Used when the user is already authenticated (e.g., split pane from existing session).
    pub fn spawn_as_user(
        cols: u16,
        rows: u16,
        term_env: &str,
        extra_env: &[(&str, &str)],
        uid: u32,
    ) -> Result<Self> {
        let winsize = Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };

        // Look up user info before fork
        let pwd = unsafe { libc::getpwuid(uid) };
        if pwd.is_null() {
            return Err(anyhow!("getpwuid({}) failed", uid));
        }
        let (shell, home, user_name, gid) = unsafe {
            let shell = std::ffi::CStr::from_ptr((*pwd).pw_shell)
                .to_str()
                .unwrap_or("/bin/sh")
                .to_string();
            let home = std::ffi::CStr::from_ptr((*pwd).pw_dir)
                .to_str()
                .unwrap_or("/")
                .to_string();
            let name = std::ffi::CStr::from_ptr((*pwd).pw_name)
                .to_str()
                .unwrap_or("")
                .to_string();
            let gid = (*pwd).pw_gid;
            (shell, home, name, gid)
        };

        let ForkptyResult {
            master,
            fork_result,
        } = unsafe { forkpty(Some(&winsize), None)? };

        match fork_result {
            ForkResult::Child => {
                // Drop privileges: set groups, gid, uid
                unsafe {
                    let user_cstr = std::ffi::CString::new(user_name.as_str())
                        .unwrap_or_else(|_| std::ffi::CString::new("").unwrap());
                    libc::initgroups(user_cstr.as_ptr(), gid);
                    libc::setgid(gid);
                    libc::setuid(uid);
                }

                // Set environment
                std::env::set_var("TERM", term_env);
                std::env::set_var("COLORTERM", "truecolor");
                std::env::set_var("TERM_PROGRAM", "ncon");
                std::env::set_var("HOME", &home);
                std::env::set_var("USER", &user_name);
                std::env::set_var("LOGNAME", &user_name);
                std::env::set_var("SHELL", &shell);

                for (key, value) in extra_env {
                    std::env::set_var(key, value);
                }

                // cd to home directory
                let _ = std::env::set_current_dir(&home);

                // Exec user's shell as login shell
                let shell_cstr = match std::ffi::CString::new(shell.as_str()) {
                    Ok(s) => s,
                    Err(_) => std::process::exit(1),
                };
                let shell_name = std::path::Path::new(&shell)
                    .file_name()
                    .map(|n| format!("-{}", n.to_string_lossy()))
                    .unwrap_or_else(|| "-sh".to_string());
                let argv0 = match std::ffi::CString::new(shell_name) {
                    Ok(s) => s,
                    Err(_) => std::process::exit(1),
                };

                match nix::unistd::execvp(&shell_cstr, &[&argv0]) {
                    Ok(infallible) => match infallible {},
                    Err(e) => {
                        nprint!("ncon: failed to spawn shell: {}", e);
                        std::process::exit(1);
                    }
                }
            }
            ForkResult::Parent { child } => {
                info!(
                    "PTY spawned: pid={}, master_fd={} (as user uid={})",
                    child,
                    master.as_raw_fd(),
                    uid
                );

                // Set master fd to non-blocking
                let flags = nix::fcntl::fcntl(master.as_raw_fd(), nix::fcntl::FcntlArg::F_GETFL)?;
                let mut flags = nix::fcntl::OFlag::from_bits_truncate(flags);
                flags.insert(nix::fcntl::OFlag::O_NONBLOCK);
                nix::fcntl::fcntl(master.as_raw_fd(), nix::fcntl::FcntlArg::F_SETFL(flags))?;

                Ok(Self {
                    master,
                    child_pid: child,
                })
            }
        }
    }

    /// Create PTY and spawn shell (with pixel size)
    ///
    /// Specify initial terminal size with `cols`, `rows`,
    /// and pixel size with `xpixel`, `ypixel`.
    /// `term_env` sets the TERM environment variable.
    /// `extra_env` sets additional environment variables for the child process.
    pub fn spawn_with_pixels(
        cols: u16,
        rows: u16,
        xpixel: u16,
        ypixel: u16,
        term_env: &str,
        extra_env: &[(&str, &str)],
    ) -> Result<Self> {
        ensure_login_env_helper();

        let winsize = Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: xpixel,
            ws_ypixel: ypixel,
        };

        let ForkptyResult {
            master,
            fork_result,
        } = unsafe { forkpty(Some(&winsize), None)? };

        match fork_result {
            ForkResult::Child => {
                // Child process: set environment variables and spawn shell
                std::env::set_var("TERM", term_env);
                std::env::set_var("COLORTERM", "truecolor");
                std::env::set_var("TERM_PROGRAM", "ncon");

                // Set extra environment variables (e.g., DBUS_SESSION_BUS_ADDRESS for IME)
                for (key, value) in extra_env {
                    std::env::set_var(key, value);
                }

                // If running as root (uid=0), use /bin/login for authentication
                // Otherwise, spawn user's shell directly
                if unsafe { libc::getuid() } == 0 {
                    // Running as root (e.g., systemd service) - use agetty as the
                    // login prompt. Note: /bin/login honors LOGIN_TIMEOUT from
                    // /etc/login.defs (the env var is ignored), so with nobody at
                    // the console it exited every 60s, taking ncon down in a
                    // restart loop. agetty's username prompt has no timeout by
                    // design (same as systemd's getty@.service).
                    let agetty = match std::ffi::CString::new("/usr/bin/agetty") {
                        Ok(s) => s,
                        Err(_) => std::process::exit(1),
                    };
                    let argv0 = match std::ffi::CString::new("agetty") {
                        Ok(s) => s,
                        Err(_) => std::process::exit(1),
                    };
                    let arg_noissue = match std::ffi::CString::new("--noissue") {
                        Ok(s) => s,
                        Err(_) => std::process::exit(1),
                    };
                    let arg_noclear = match std::ffi::CString::new("--noclear") {
                        Ok(s) => s,
                        Err(_) => std::process::exit(1),
                    };
                    let arg_noreset = match std::ffi::CString::new("--noreset") {
                        Ok(s) => s,
                        Err(_) => std::process::exit(1),
                    };
                    let arg_dash = match std::ffi::CString::new("-") {
                        Ok(s) => s,
                        Err(_) => std::process::exit(1),
                    };
                    // The login program gets the TERM ncon was configured with.
                    // Reading ncon's own $TERM is not useful: the systemd unit
                    // runs with a bare environment, so that fell back to "linux"
                    // and the login shell advertised a VT terminal even though
                    // ncon emulates xterm-256color.
                    let arg_term = match std::ffi::CString::new(term_env) {
                        Ok(s) => s,
                        Err(_) => std::process::exit(1),
                    };
                    match nix::unistd::execvp(
                        &agetty,
                        &[
                            &argv0,
                            &arg_noissue,
                            &arg_noclear,
                            &arg_noreset,
                            &arg_dash,
                            &arg_term,
                        ],
                    ) {
                        Ok(infallible) => match infallible {},
                        Err(e) => {
                            nprint!("ncon: failed to exec /usr/bin/agetty: {}", e);
                            std::process::exit(1);
                        }
                    }
                } else {
                    // Running as normal user - spawn shell directly
                    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
                    let shell_cstr = match std::ffi::CString::new(shell.as_str()) {
                        Ok(s) => s,
                        Err(_) => {
                            nprint!("ncon: invalid shell path (contains NUL byte)");
                            std::process::exit(1);
                        }
                    };

                    // Launch as login shell (prefix with '-')
                    let shell_name = std::path::Path::new(&shell)
                        .file_name()
                        .map(|n| format!("-{}", n.to_string_lossy()))
                        .unwrap_or_else(|| "-sh".to_string());
                    let argv0 = match std::ffi::CString::new(shell_name) {
                        Ok(s) => s,
                        Err(_) => {
                            nprint!("ncon: invalid shell name (contains NUL byte)");
                            std::process::exit(1);
                        }
                    };

                    match nix::unistd::execvp(&shell_cstr, &[&argv0]) {
                        Ok(infallible) => match infallible {},
                        Err(e) => {
                            nprint!("ncon: failed to spawn shell: {}", e);
                            std::process::exit(1);
                        }
                    }
                }
            }
            ForkResult::Parent { child } => {
                info!(
                    "PTY spawned: pid={}, master_fd={}",
                    child,
                    master.as_raw_fd()
                );

                // Set master fd to non-blocking
                let flags = nix::fcntl::fcntl(master.as_raw_fd(), nix::fcntl::FcntlArg::F_GETFL)?;
                let mut flags = nix::fcntl::OFlag::from_bits_truncate(flags);
                flags.insert(nix::fcntl::OFlag::O_NONBLOCK);
                nix::fcntl::fcntl(master.as_raw_fd(), nix::fcntl::FcntlArg::F_SETFL(flags))?;

                Ok(Self {
                    master,
                    child_pid: child,
                })
            }
        }
    }

    /// Non-blocking read from PTY
    ///
    /// Returns number of bytes read if data available.
    /// Returns Ok(0) if no data.
    pub fn read(&self, buf: &mut [u8]) -> Result<usize> {
        match nix::unistd::read(self.master.as_raw_fd(), buf) {
            Ok(n) => Ok(n),
            Err(nix::errno::Errno::EAGAIN) => Ok(0),
            Err(e) => Err(anyhow!("PTY read error: {}", e)),
        }
    }

    /// Write data to PTY (single attempt, may be partial)
    pub fn write(&self, data: &[u8]) -> Result<usize> {
        match nix::unistd::write(self.master.as_raw_fd(), data) {
            Ok(n) => Ok(n),
            Err(nix::errno::Errno::EAGAIN) => Ok(0),
            Err(e) => Err(anyhow!("PTY write error: {}", e)),
        }
    }

    /// Write all data to PTY (handles partial writes and EAGAIN)
    ///
    /// Retries up to 100 times with short delays for EAGAIN/partial writes.
    /// Use this for important data like paste operations.
    pub fn write_all(&self, data: &[u8]) -> Result<()> {
        let mut written = 0;
        let mut retries = 0;
        const MAX_RETRIES: usize = 100;

        while written < data.len() && retries < MAX_RETRIES {
            match nix::unistd::write(self.master.as_raw_fd(), &data[written..]) {
                Ok(0) => {
                    // No progress, wait and retry
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    retries += 1;
                }
                Ok(n) => {
                    written += n;
                    retries = 0; // Reset retries on successful write
                }
                Err(nix::errno::Errno::EAGAIN) => {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    retries += 1;
                }
                Err(e) => return Err(anyhow!("PTY write error: {}", e)),
            }
        }

        if written < data.len() {
            Err(anyhow!(
                "PTY write incomplete: {} of {} bytes after {} retries",
                written,
                data.len(),
                retries
            ))
        } else {
            Ok(())
        }
    }

    /// Change terminal size (TIOCSWINSZ) - without pixel size
    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        self.set_size_with_pixels(cols, rows, 0, 0)
    }

    /// Change terminal size (TIOCSWINSZ) - with pixel size
    pub fn resize_with_pixels(&self, cols: u16, rows: u16, xpixel: u16, ypixel: u16) -> Result<()> {
        self.set_size_with_pixels(cols, rows, xpixel, ypixel)
    }

    /// Change terminal size (TIOCSWINSZ)
    pub fn set_size(&self, cols: u16, rows: u16) -> Result<()> {
        self.set_size_with_pixels(cols, rows, 0, 0)
    }

    /// Change terminal size (TIOCSWINSZ) - with pixel size
    pub fn set_size_with_pixels(
        &self,
        cols: u16,
        rows: u16,
        xpixel: u16,
        ypixel: u16,
    ) -> Result<()> {
        let winsize = Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: xpixel,
            ws_ypixel: ypixel,
        };

        unsafe {
            let ret = libc::ioctl(
                self.master.as_raw_fd(),
                libc::TIOCSWINSZ,
                &winsize as *const Winsize,
            );
            if ret < 0 {
                return Err(anyhow!("TIOCSWINSZ failed: {}", io::Error::last_os_error()));
            }
        }

        // Send SIGWINCH to child process
        let _ = nix::sys::signal::kill(self.child_pid, nix::sys::signal::Signal::SIGWINCH);

        Ok(())
    }

    /// Send SIGHUP to child process group (for pre-shutdown)
    pub fn send_sighup(&self) {
        let pgid = Pid::from_raw(-self.child_pid.as_raw());
        let _ = nix::sys::signal::kill(pgid, nix::sys::signal::Signal::SIGHUP);
    }

    /// Check if child process is alive
    pub fn is_alive(&self) -> bool {
        match waitpid(self.child_pid, Some(WaitPidFlag::WNOHANG)) {
            Ok(nix::sys::wait::WaitStatus::StillAlive) => true,
            Ok(_) => false, // Exited
            Err(_) => false,
        }
    }

    /// Get foreground process name
    ///
    /// Gets the PTY's foreground process group
    /// and returns the leader process name.
    /// Can also detect processes inside tmux/screen/zellij.
    /// Returns None if unavailable.
    pub fn foreground_process_name(&self) -> Option<String> {
        // Get foreground process group with tcgetpgrp
        let pgid = unsafe { libc::tcgetpgrp(self.master.as_raw_fd()) };
        if pgid <= 0 {
            return None;
        }

        // Get process name
        let proc_name = Self::get_process_name(pgid)?;

        // For terminal multiplexers, find the foreground process inside
        match proc_name.as_str() {
            // tmux: get current command via tmux display-message
            name if name == "tmux: client" || name.starts_with("tmux") => {
                Self::get_tmux_foreground_command().or_else(|| Self::find_leaf_process(pgid))
            }
            // screen: screen -Q title or process tree
            "screen" | "SCREEN" => {
                Self::get_screen_foreground_command().or_else(|| Self::find_leaf_process(pgid))
            }
            // zellij: traverse process tree
            "zellij" => {
                Self::find_zellij_foreground(pgid).or_else(|| Self::find_leaf_process(pgid))
            }
            // Normal process
            _ => Some(proc_name),
        }
    }

    /// Get process name from PID
    fn get_process_name(pid: i32) -> Option<String> {
        let comm_path = format!("/proc/{}/comm", pid);
        std::fs::read_to_string(&comm_path)
            .ok()
            .map(|s| s.trim().to_string())
    }

    /// Get currently running command in tmux's current pane
    fn get_tmux_foreground_command() -> Option<String> {
        let output = std::process::Command::new("tmux")
            .args(["display-message", "-p", "#{pane_current_command}"])
            .output()
            .ok()?;

        if output.status.success() {
            let cmd = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !cmd.is_empty() {
                return Some(cmd);
            }
        }
        None
    }

    /// Get currently running command in screen's current window
    fn get_screen_foreground_command() -> Option<String> {
        // Get current window title with screen -Q title
        // (In many cases, the running command name becomes the title)
        let output = std::process::Command::new("screen")
            .args(["-Q", "title"])
            .output()
            .ok()?;

        if output.status.success() {
            let title = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !title.is_empty() && title != "bash" && title != "zsh" && title != "sh" {
                return Some(title);
            }
        }

        // Fallback: try to get from hardstatus
        None
    }

    /// Find currently running process in zellij's current pane
    fn find_zellij_foreground(_client_pid: i32) -> Option<String> {
        // zellij server process manages child processes
        // Find pane process via server from client PID

        // First find zellij-server process
        if let Ok(entries) = std::fs::read_dir("/proc") {
            for entry in entries.flatten() {
                if let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() {
                    if let Some(name) = Self::get_process_name(pid) {
                        if name == "zellij" {
                            // Find deepest process from zellij server's child processes
                            if let Some(leaf) = Self::find_leaf_process(pid) {
                                // Prefer non-shell processes
                                if !matches!(leaf.as_str(), "bash" | "zsh" | "sh" | "fish") {
                                    return Some(leaf);
                                }
                            }
                        }
                    }
                }
            }
        }
        None
    }

    /// Traverse process tree to find deepest leaf process
    /// Excludes multiplexers and shells to find actual command
    fn find_leaf_process(pid: i32) -> Option<String> {
        let mut current_pid = pid;
        let mut last_meaningful_name: Option<String> = None;
        let mut visited = std::collections::HashSet::new();

        loop {
            if visited.contains(&current_pid) {
                break;
            }
            visited.insert(current_pid);

            // Get child processes
            let children = Self::get_children(current_pid);

            if children.is_empty() {
                // Leaf process
                if let Some(name) = Self::get_process_name(current_pid) {
                    return Some(name);
                }
                break;
            }

            // Save current process name (excluding multiplexers/shells)
            if let Some(name) = Self::get_process_name(current_pid) {
                if !Self::is_wrapper_process(&name) {
                    last_meaningful_name = Some(name);
                }
            }

            // Follow first child process (assumed to be active pane)
            current_pid = children[0];
        }

        last_meaningful_name
    }

    /// Get list of child process IDs
    fn get_children(pid: i32) -> Vec<i32> {
        let children_path = format!("/proc/{}/task/{}/children", pid, pid);
        std::fs::read_to_string(&children_path)
            .unwrap_or_default()
            .split_whitespace()
            .filter_map(|s| s.parse().ok())
            .collect()
    }

    /// Check if process is a wrapper (multiplexer, shell, etc.)
    fn is_wrapper_process(name: &str) -> bool {
        matches!(
            name,
            "tmux"
                | "tmux: client"
                | "tmux: server"
                | "screen"
                | "SCREEN"
                | "zellij"
                | "bash"
                | "zsh"
                | "sh"
                | "fish"
                | "dash"
                | "ksh"
                | "tcsh"
                | "csh"
        ) || name.starts_with("tmux:")
    }

    /// Get the UID of the child process
    pub fn child_uid(&self) -> Option<u32> {
        Self::uid_of_pid(self.child_pid.as_raw())
    }

    /// Get the UID of the logged-in user (walks process tree).
    ///
    /// `/bin/login` forks: the direct child stays as root (waiting),
    /// and the grandchild becomes the user's shell. This method checks
    /// the child first, then walks to grandchildren to find a non-root UID.
    pub fn logged_in_uid(&self) -> Option<u32> {
        // Check direct child first
        let uid = Self::uid_of_pid(self.child_pid.as_raw());
        if let Some(u) = uid {
            if u != 0 {
                return Some(u);
            }
        }

        // Direct child is root (login parent), check grandchildren
        let children_path = format!(
            "/proc/{}/task/{}/children",
            self.child_pid, self.child_pid
        );
        if let Ok(children) = std::fs::read_to_string(&children_path) {
            for pid_str in children.split_whitespace() {
                if let Ok(pid) = pid_str.parse::<i32>() {
                    if let Some(u) = Self::uid_of_pid(pid) {
                        if u != 0 {
                            return Some(u);
                        }
                    }
                }
            }
        }

        None
    }

    /// Read the real UID of a process from /proc/<pid>/status
    fn uid_of_pid(pid: i32) -> Option<u32> {
        let content = std::fs::read_to_string(format!("/proc/{}/status", pid)).ok()?;
        for line in content.lines() {
            if line.starts_with("Uid:") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    return parts[1].parse().ok();
                }
            }
        }
        None
    }

    /// Get the home directory of the child process's owner
    pub fn child_home_dir(&self) -> Option<String> {
        home_dir_for_uid(self.child_uid()?)
    }
}

/// Home directory for a UID (via the password database).
pub fn home_dir_for_uid(uid: u32) -> Option<String> {
    unsafe {
        let pwd = libc::getpwuid(uid);
        if pwd.is_null() {
            return None;
        }
        let home = std::ffi::CStr::from_ptr((*pwd).pw_dir);
        home.to_str().ok().map(|s| s.to_string())
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // Close master fd first — this causes EIO on the slave side,
        // prompting well-behaved children to exit on their own.
        // Note: OwnedFd::drop will try to close this fd again (EBADF), which is harmless
        // during single-threaded shutdown.
        unsafe {
            libc::close(self.master.as_raw_fd());
        }

        // Send SIGHUP to the entire process group (bash + all its children).
        // forkpty calls setsid(), so child_pid == process group ID.
        let pgid = Pid::from_raw(-self.child_pid.as_raw());
        let _ = nix::sys::signal::kill(pgid, nix::sys::signal::Signal::SIGHUP);

        // Wait with timeout: try non-blocking waits for up to 500ms, then SIGKILL
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
        loop {
            match waitpid(self.child_pid, Some(WaitPidFlag::WNOHANG)) {
                Ok(nix::sys::wait::WaitStatus::StillAlive) => {
                    if std::time::Instant::now() >= deadline {
                        log::warn!(
                            "Child {} did not exit after SIGHUP, sending SIGKILL to process group",
                            self.child_pid
                        );
                        let _ = nix::sys::signal::kill(pgid, nix::sys::signal::Signal::SIGKILL);
                        // Non-blocking wait after SIGKILL (max 500ms)
                        let kill_deadline =
                            std::time::Instant::now() + std::time::Duration::from_millis(500);
                        loop {
                            match waitpid(self.child_pid, Some(WaitPidFlag::WNOHANG)) {
                                Ok(nix::sys::wait::WaitStatus::StillAlive) => {
                                    if std::time::Instant::now() >= kill_deadline {
                                        log::error!(
                                            "Child {} still alive after SIGKILL, abandoning",
                                            self.child_pid
                                        );
                                        break;
                                    }
                                    std::thread::sleep(std::time::Duration::from_millis(10));
                                }
                                _ => break,
                            }
                        }
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                _ => break, // Exited or error
            }
        }
    }
}

/// Read and expand the issue text the way agetty does.
///
/// The primary issue file is `/etc/issue`; `.issue` fragments next to it are
/// appended in version-sort order (agetty's `/etc/issue.d` extension). Since
/// util-linux 2.41 the same file+directory pair under `/run` and `/usr/lib` is
/// read as well, which lets a distribution ship a generated message
/// independently of the host-specific file.
///
/// The content is rendered by [`expand_issue`], so it may use the getty escape
/// sequences (notably `\r` for the running kernel release and
/// `\S{PRETTY_NAME}` from `/etc/os-release`).
///
/// Returns None when no issue file exists.
pub fn read_issue(tty_name: &str) -> Option<String> {
    let mut content = String::new();

    for base in ["/etc", "/run", "/usr/lib"] {
        let file = format!("{}/issue", base);
        let Ok(text) = std::fs::read_to_string(&file) else {
            // agetty treats the directory as an extension of the file in the
            // same location and ignores it when the file is missing.
            continue;
        };
        content.push_str(&text);

        let dir = format!("{}/issue.d", base);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut fragments: Vec<_> = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("issue"))
            .collect();
        fragments.sort_by(|a, b| {
            let a = a.file_name().unwrap_or_default().to_string_lossy();
            let b = b.file_name().unwrap_or_default().to_string_lossy();
            version_sort_cmp(&a, &b)
        });
        for fragment in fragments {
            if let Ok(text) = std::fs::read_to_string(&fragment) {
                content.push_str(&text);
            }
        }
    }

    if content.is_empty() {
        return None;
    }
    Some(expand_issue(&content, tty_name))
}

/// Expand the getty escape sequences in issue text.
///
/// Supported: `\d` date, `\l` tty line, `\m` machine, `\n` nodename, `\o` NIS
/// domain, `\r` kernel release, `\s` system name, `\t` time, `\v` kernel
/// version, `\S` / `\S{VAR}` os-release field, `\\` literal backslash.
fn expand_issue(content: &str, tty_name: &str) -> String {
    let os_release = parse_os_release();
    expand_issue_with(content, tty_name, &os_release)
}

/// [`expand_issue`] with an injected os-release map (helps testing).
fn expand_issue_with(
    content: &str,
    tty_name: &str,
    os_release: &HashMap<String, String>,
) -> String {
    let mut result = String::with_capacity(content.len() * 2);
    let mut chars = content.chars().peekable();

    // Get system info (cached)
    let uname = get_uname();
    let hostname = uname.nodename.clone();
    let machine = uname.machine.clone();
    let release = uname.release.clone();
    let sysname = uname.sysname.clone();
    let version = uname.version.clone();

    // Get domain name
    let domainname = get_domainname().unwrap_or_default();

    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('d') => {
                    // Current date
                    let now = chrono::Local::now();
                    result.push_str(&now.format("%a %b %d %Y").to_string());
                }
                Some('l') => {
                    // TTY name
                    result.push_str(tty_name);
                }
                Some('m') => {
                    // Machine architecture
                    result.push_str(&machine);
                }
                Some('n') => {
                    // Hostname
                    result.push_str(&hostname);
                }
                Some('o') => {
                    // Domain name
                    result.push_str(&domainname);
                }
                Some('r') => {
                    // Kernel release
                    result.push_str(&release);
                }
                Some('s') => {
                    // Kernel name
                    result.push_str(&sysname);
                }
                Some('t') => {
                    // Current time
                    let now = chrono::Local::now();
                    result.push_str(&now.format("%H:%M:%S").to_string());
                }
                Some('v') => {
                    // Kernel version
                    result.push_str(&version);
                }
                Some('S') => {
                    // os-release field: \S{VARIABLE}, or bare \S for PRETTY_NAME.
                    let name = if chars.peek() == Some(&'{') {
                        chars.next();
                        let mut name = String::new();
                        for ch in chars.by_ref() {
                            if ch == '}' {
                                break;
                            }
                            name.push(ch);
                        }
                        name
                    } else {
                        String::new()
                    };
                    push_os_release(&mut result, &name, &sysname, os_release);
                }
                Some('\\') => {
                    result.push('\\');
                }
                Some(other) => {
                    // Unknown escape, keep as-is
                    result.push('\\');
                    result.push(other);
                }
                None => {
                    result.push('\\');
                }
            }
        } else {
            result.push(c);
        }
    }

    result
}

/// Append an os-release value for agetty's `\S{VARIABLE}`.
///
/// An empty name (bare `\S`) means PRETTY_NAME, falling back to the system
/// name. An unknown variable expands to nothing. `ANSI_COLOR` is special-cased
/// into a real terminal escape sequence, matching agetty.
fn push_os_release(
    out: &mut String,
    name: &str,
    sysname: &str,
    os_release: &HashMap<String, String>,
) {
    if name.is_empty() {
        out.push_str(
            os_release
                .get("PRETTY_NAME")
                .map(String::as_str)
                .unwrap_or(sysname),
        );
        return;
    }
    match os_release.get(name) {
        Some(value) if name == "ANSI_COLOR" => {
            out.push('\x1b');
            out.push('[');
            out.push_str(value);
            out.push('m');
        }
        Some(value) => out.push_str(value),
        None => {}
    }
}

/// Parse `/etc/os-release`, falling back to `/usr/lib/os-release`.
fn parse_os_release() -> HashMap<String, String> {
    for path in ["/etc/os-release", "/usr/lib/os-release"] {
        if let Ok(text) = std::fs::read_to_string(path) {
            return parse_os_release_text(&text);
        }
    }
    HashMap::new()
}

/// Parse os-release `KEY=value` lines. Values may be single- or double-quoted;
/// the shell-style escapes the format allows are not interpreted, which is
/// enough for the fields used in issue files (PRETTY_NAME, ANSI_COLOR, ...).
fn parse_os_release_text(text: &str) -> HashMap<String, String> {
    let mut values = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
            .unwrap_or(value);
        values.insert(key.trim().to_string(), value.to_string());
    }
    values
}

/// Order two `.issue` fragment names the way agetty does (version sort), so
/// `9-a.issue` sorts before `10-b.issue`.
fn version_sort_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    let (a, b) = (a.as_bytes(), b.as_bytes());
    let (mut ai, mut bi) = (0usize, 0usize);

    while ai < a.len() && bi < b.len() {
        if a[ai].is_ascii_digit() && b[bi].is_ascii_digit() {
            let (a_start, b_start) = (ai, bi);
            while ai < a.len() && a[ai].is_ascii_digit() {
                ai += 1;
            }
            while bi < b.len() && b[bi].is_ascii_digit() {
                bi += 1;
            }
            let a_run = &a[a_start..ai];
            let b_run = &b[b_start..bi];
            // Longer run of significant digits wins ("10" > "9").
            let a_digits = a_run.iter().skip_while(|&&c| c == b'0').count();
            let b_digits = b_run.iter().skip_while(|&&c| c == b'0').count();
            match a_digits.cmp(&b_digits).then_with(|| a_run.cmp(b_run)) {
                Ordering::Equal => {}
                other => return other,
            }
        } else {
            match a[ai].cmp(&b[bi]) {
                Ordering::Equal => {
                    ai += 1;
                    bi += 1;
                }
                other => return other,
            }
        }
    }

    (a.len() - ai).cmp(&(b.len() - bi))
}

/// System info from uname()
struct UnameInfo {
    sysname: String,
    nodename: String,
    release: String,
    version: String,
    machine: String,
}

fn get_uname() -> UnameInfo {
    let mut utsname: libc::utsname = unsafe { std::mem::zeroed() };
    unsafe { libc::uname(&mut utsname) };

    // Use CStr::from_ptr for portable handling of utsname fields
    // (i8 on x86_64, u8 on aarch64)
    unsafe fn field_to_string(ptr: *const libc::c_char) -> String {
        std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned()
    }

    unsafe {
        UnameInfo {
            sysname: field_to_string(utsname.sysname.as_ptr() as *const libc::c_char),
            nodename: field_to_string(utsname.nodename.as_ptr() as *const libc::c_char),
            release: field_to_string(utsname.release.as_ptr() as *const libc::c_char),
            version: field_to_string(utsname.version.as_ptr() as *const libc::c_char),
            machine: field_to_string(utsname.machine.as_ptr() as *const libc::c_char),
        }
    }
}

fn get_domainname() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/domainname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s != "(none)")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The helper is sourced by whatever login shell the user runs; a syntax
    /// error there would break every login. Let the shell itself check it (no
    /// root needed: read/write in the temp dir).
    #[test]
    fn login_env_helper_is_valid_posix_sh() {
        let path = std::env::temp_dir().join(format!(
            "ncon-login-env-{}.sh",
            std::process::id()
        ));
        std::fs::write(&path, LOGIN_ENV_HELPER).expect("write temp script");
        let status = std::process::Command::new("/bin/sh")
            .arg("-n")
            .arg(&path)
            .status()
            .expect("/bin/sh -n");
        let _ = std::fs::remove_file(&path);
        assert!(status.success(), "profile.d helper must be valid POSIX sh");
    }

    /// Guard against the regression this file was added for: the helper must
    /// never override an explicitly set locale.
    #[test]
    fn login_env_helper_only_fills_missing_lang() {
        assert!(LOGIN_ENV_HELPER.contains(r#"[ -z "${LANG:-}" ]"#));
    }

    #[test]
    fn expand_issue_kernel_release_escape() {
        let out = expand_issue_with("Arch \\r (\\l)", "tty1", &HashMap::new());
        assert!(out.starts_with("Arch "), "{out}");
        assert!(out.ends_with(" (tty1)"), "{out}");
        // uname -r looks like "7.2.9-arch1-1".
        assert!(out.contains('-'), "{out}");
    }

    #[test]
    fn expand_issue_os_release_pretty_name_and_default() {
        let mut os = HashMap::new();
        os.insert("PRETTY_NAME".to_string(), "Arch Linux".to_string());
        assert_eq!(
            expand_issue_with("\\S{PRETTY_NAME}", "tty1", &os),
            "Arch Linux"
        );
        // Bare \S means PRETTY_NAME.
        assert_eq!(expand_issue_with("\\S", "tty1", &os), "Arch Linux");
    }

    #[test]
    fn expand_issue_os_release_missing_and_fallback() {
        let os = HashMap::new();
        // Unknown variable expands to nothing.
        assert_eq!(expand_issue_with("[\\S{NOPE}]", "tty1", &os), "[]");
        // Bare \S with no PRETTY_NAME falls back to the system name.
        assert_eq!(expand_issue_with("\\S", "tty1", &os), "Linux");
    }

    #[test]
    fn expand_issue_os_release_ansi_color() {
        let mut os = HashMap::new();
        os.insert("ANSI_COLOR".to_string(), "38;2;23;147;209".to_string());
        assert_eq!(
            expand_issue_with("\\S{ANSI_COLOR}", "tty1", &os),
            "\x1b[38;2;23;147;209m"
        );
    }

    #[test]
    fn os_release_parser_handles_quotes_and_comments() {
        let text = "# comment\nNAME=Arch\nPRETTY_NAME=\"Arch Linux\"\nID='arch'\n\nEMPTY=\n";
        let map = parse_os_release_text(text);
        assert_eq!(map.get("NAME").map(String::as_str), Some("Arch"));
        assert_eq!(
            map.get("PRETTY_NAME").map(String::as_str),
            Some("Arch Linux")
        );
        assert_eq!(map.get("ID").map(String::as_str), Some("arch"));
        assert_eq!(map.get("EMPTY").map(String::as_str), Some(""));
    }

    #[test]
    fn issue_fragments_sort_in_version_order() {
        let mut names = vec!["10-b.issue", "9-a.issue", "2-c.issue", "README"];
        names.sort_by(|a, b| version_sort_cmp(a, b));
        assert_eq!(
            names,
            vec!["2-c.issue", "9-a.issue", "10-b.issue", "README"]
        );
        // "10-b" < "2-c" byte-wise, but version sort puts 2 first.
        assert_eq!(
            version_sort_cmp("2-c.issue", "10-b.issue"),
            std::cmp::Ordering::Less
        );
    }
}
