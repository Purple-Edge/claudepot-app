//! Will the Claude Code sessions running right now follow an account
//! switch, or does one of them hold the old login in a way that undoes it?
//!
//! # What changed, and how it was established
//!
//! Older Claude Code kept the login it started with in memory and, at
//! its next token refresh, wrote that login back over the credential
//! slot — silently reverting a swap Claudepot had just made, and
//! leaving Claudepot holding a spent copy of the outgoing account. That
//! is why switching used to refuse while any `claude` process ran.
//!
//! Claude Code now re-reads its credential store when it changes. On
//! 2026-10-12, against **2.1.296** on macOS: an interactive session was
//! started on account A with A's stored expiry pulled forward, the slot
//! was force-switched to B, and the session was prompted inside A's
//! refresh window and again after A's expiry. It answered every time,
//! A's login was never refreshed or written back, and the slot still
//! held B. A's login was then renewed from Claudepot's private slot —
//! which revokes the access token the session had started with — and
//! both that session and an unrelated long-running one kept working.
//!
//! So a switch is safe when every running Claude Code is at least
//! [`SWITCH_FOLLOWED_SINCE`]. The floor is the version that was
//! observed, not the version the behaviour arrived in, which is
//! unknown; lowering it needs the same experiment against the older
//! binary. Per `.claude/rules/cc-upstream-watch.md` this is a dated pin
//! with a row in `crates/xtask/cc-upstream-watch.md`.
//!
//! # Running version, not installed version
//!
//! A session keeps the binary it was started with: on the day the floor
//! was set, one session on this machine was still 2.1.295 while the
//! installed binary was 2.1.296. So the version is read per process,
//! from the path of its executable (the native installer runs
//! `…/claude/versions/<version>`). On macOS that path comes from the
//! kernel ([`running_exe`]): `sysinfo` reports the path the process was
//! *launched as*, `~/.local/bin/claude`, and resolving that symlink names
//! the *installed* version — which would read the 2.1.295 session above
//! as 2.1.296 and let the switch through. A process whose version cannot be
//! read — an npm install running under `node`, a WSL process, a
//! permission-denied executable path — counts as **too old**: the
//! old refusal is the fail-closed answer.
//!
//! # What this does not cover
//!
//! Only the swap. Spending a refresh token that a running Claude Code
//! also holds was not part of the experiment, so every refresh gate
//! keeps using [`super::swap::is_cc_process_running_public`]'s plain
//! "is anything running".

use crate::cc_capability::CcVersion;

/// The oldest Claude Code observed to follow a credential-slot switch
/// without writing the previous login back. See the module docs.
pub const SWITCH_FOLLOWED_SINCE: CcVersion = CcVersion::new(2, 1, 296);

/// The version of one running Claude Code process, or `None` when it
/// could not be read.
pub type RunningVersion = Option<CcVersion>;

/// Read a Claude Code version from the path of its executable.
///
/// Matches the native installer's layout, `…/versions/<major.minor.patch>`
/// with an optional `.exe`, under either separator so that a Windows
/// path parses identically on every host (`rules/paths.md`). Anything
/// else — a `node` binary, a symlink name like `claude` — is `None`.
pub fn version_from_exe(exe: &str) -> RunningVersion {
    // Linux's `/proc/<pid>/exe` names a binary the updater has since
    // removed as `<path> (deleted)`; the process is still that version.
    let exe = exe.strip_suffix(" (deleted)").unwrap_or(exe);
    let mut parts = exe.rsplit(['/', '\\']).filter(|p| !p.is_empty());
    let file = parts.next()?;
    let dir = parts.next()?;
    if !dir.eq_ignore_ascii_case("versions") {
        return None;
    }
    let stem = file
        .strip_suffix(".exe")
        .or_else(|| file.strip_suffix(".EXE"))
        .unwrap_or(file);
    CcVersion::parse(stem)
}

/// Would a switch be undone by one of these running sessions?
///
/// `true` when any session's version is unknown or older than
/// [`SWITCH_FOLLOWED_SINCE`]. No running sessions is `false`.
pub fn switch_would_be_reverted(running: &[RunningVersion]) -> bool {
    running
        .iter()
        .any(|v| v.is_none_or(|v| v < SWITCH_FOLLOWED_SINCE))
}

/// The running Claude Code CLI processes on this machine, with their
/// versions. Uses the same identification as the swap gate's own scan —
/// a process named `claude` off Windows; on Windows the `claude.exe`
/// processes `claude_proc` attributes to the CLI, which leaves out Claude
/// Desktop and the Claude Code its Code tab runs — plus WSL on Windows,
/// whose processes are always of unknown version.
pub async fn running_versions() -> Vec<RunningVersion> {
    // A scan that panicked tells us nothing, so it reads as one session
    // of unknown version — the fail-closed answer.
    let versions = tokio::task::spawn_blocking(scan_processes)
        .await
        .unwrap_or_else(|_| vec![None]);
    #[cfg(windows)]
    let versions = {
        let mut versions = versions;
        if super::wsl_probe::any_claude_in_wsl().await {
            versions.push(None);
        }
        versions
    };
    versions
}

/// True when a switch made now would be reverted by a running session.
/// The question every "Claude Code is running" warning about switching
/// actually asks.
pub async fn switch_needs_cc_restart() -> bool {
    switch_would_be_reverted(&running_versions().await)
}

#[cfg(not(windows))]
fn scan_processes() -> Vec<RunningVersion> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::new().with_exe(UpdateKind::Always),
    );
    sys.processes()
        .values()
        .filter(|p| is_claude_cli(p))
        .map(|p| running_exe(p).as_deref().and_then(version_from_exe))
        .collect()
}

/// Windows: the processes `claude_proc` counts as holding the CLI login,
/// from one snapshot. Its executable paths come from
/// `QueryFullProcessImageNameW`, so they name the running binary.
#[cfg(windows)]
fn scan_processes() -> Vec<RunningVersion> {
    use crate::claude_proc;

    let procs = claude_proc::snapshot();
    let holders: std::collections::HashSet<u32> = claude_proc::live_cli_holders_in(&procs)
        .into_iter()
        .map(|(pid, _)| pid)
        .collect();
    procs
        .iter()
        .filter(|p| holders.contains(&p.pid))
        .map(|p| p.exe.as_deref().and_then(version_from_exe))
        .collect()
}

/// The binary a process is actually executing.
///
/// On macOS `sysinfo` derives `exe()` from the launch path, so a session
/// started through the `~/.local/bin/claude` symlink reports the symlink;
/// the kernel's `proc_pidpath` names the file that is running. Linux
/// (`/proc/<pid>/exe`) and Windows (`QueryFullProcessImageNameW`) already
/// report the running binary through `sysinfo`.
#[cfg(target_os = "macos")]
fn running_exe(p: &sysinfo::Process) -> Option<String> {
    let pid = libc::c_int::try_from(p.pid().as_u32()).ok()?;
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: `buf` is a live, writable allocation of exactly the length
    // passed; `proc_pidpath` writes at most that many bytes and returns
    // how many it wrote, or <= 0 on failure.
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    let n = usize::try_from(n).ok().filter(|&n| n > 0)?;
    buf.truncate(n);
    String::from_utf8(buf).ok()
}

#[cfg(not(any(target_os = "macos", windows)))]
fn running_exe(p: &sysinfo::Process) -> Option<String> {
    p.exe().and_then(|e| e.to_str()).map(String::from)
}

/// A Claude Code CLI process: named `claude` (the symlink it is usually
/// launched through), or running a binary from the native installer's
/// `versions/` directory under any name.
#[cfg(not(windows))]
fn is_claude_cli(p: &sysinfo::Process) -> bool {
    p.name() == "claude"
        || running_exe(p).is_some_and(|e| e.contains("claude") && version_from_exe(&e).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_native_installer_layout_in_every_path_shape() {
        let v = Some(CcVersion::new(2, 1, 296));
        for exe in [
            "/Users/someone/.local/share/claude/versions/2.1.296",
            "/home/someone/.local/share/claude/versions/2.1.296",
            r"C:\Users\someone\.local\share\claude\versions\2.1.296.exe",
            r"\\server\share\someone\.local\share\claude\versions\2.1.296.exe",
            r"\\?\C:\Users\someone\.local\share\claude\versions\2.1.296.exe",
            "/home/someone/.local/share/claude/versions/2.1.296 (deleted)",
        ] {
            assert_eq!(version_from_exe(exe), v, "{exe}");
        }
    }

    #[test]
    fn anything_else_has_no_version() {
        for exe in [
            "/opt/homebrew/bin/node",
            "/Users/someone/.local/bin/claude",
            "/Users/someone/.local/share/claude/versions/latest",
            "/Users/someone/other/2.1.296",
            "2.1.296",
            "",
        ] {
            assert_eq!(version_from_exe(exe), None, "{exe:?}");
        }
    }

    /// The macOS bug in this module's first draft: a session launched
    /// through the `claude` symlink reported the symlink, which has no
    /// version, so every session read as unknown. Launch a child through
    /// a symlink and require the target, not the link.
    #[cfg(target_os = "macos")]
    #[test]
    fn reads_the_running_binary_not_the_symlink_it_was_launched_through() {
        use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("claude");
        std::os::unix::fs::symlink("/bin/sleep", &link).unwrap();
        let mut child = std::process::Command::new(&link).arg("30").spawn().unwrap();

        let pid = Pid::from_u32(child.id());
        let mut sys = System::new();
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::new().with_exe(UpdateKind::Always),
        );
        let exe = running_exe(sys.process(pid).unwrap());
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(exe.as_deref(), Some("/bin/sleep"));
    }

    #[test]
    fn no_running_session_never_blocks() {
        assert!(!switch_would_be_reverted(&[]));
    }

    #[test]
    fn sessions_at_or_above_the_floor_follow_the_switch() {
        let at = Some(SWITCH_FOLLOWED_SINCE);
        let above = Some(CcVersion::new(2, 2, 0));
        assert!(!switch_would_be_reverted(&[at, above]));
    }

    /// The case that existed on the day the floor was set: one session
    /// left on the previous release.
    #[test]
    fn one_older_session_blocks() {
        let at = Some(SWITCH_FOLLOWED_SINCE);
        let older = Some(CcVersion::new(2, 1, 295));
        assert!(switch_would_be_reverted(&[at, older]));
    }

    #[test]
    fn an_unknown_version_blocks() {
        assert!(switch_would_be_reverted(&[
            Some(SWITCH_FOLLOWED_SINCE),
            None
        ]));
    }
}
