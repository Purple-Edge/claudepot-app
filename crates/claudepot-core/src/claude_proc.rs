//! What a Windows `claude.exe` process actually is.
//!
//! Three different programs run under the image name `claude.exe` on
//! Windows, and the kernel reports all of them in lower case:
//!
//! - **Claude Desktop** itself, plus every Electron helper it spawns.
//!   The Microsoft Store (MSIX) build lives under
//!   `…\WindowsApps\Claude_<version>_<arch>__pzs8sxrjxfjjc\app\`; the
//!   older Squirrel build under `%LOCALAPPDATA%\Programs\Claude\`, and a
//!   machine-wide install under `%ProgramFiles%\Claude\`.
//! - **Desktop's Code tab** — a Claude Code binary Desktop downloads and
//!   runs itself, under `%APPDATA%\Claude\claude-code\<ver>\…`. Under
//!   MSIX the real path is the virtualized
//!   `%LOCALAPPDATA%\Packages\Claude_pzs8sxrjxfjjc\LocalCache\Roaming\Claude\claude-code\…`
//!   (that is what `sysinfo` reports; `Get-Process` shows the
//!   unvirtualized one). It authenticates through Desktop and does not
//!   read or rotate `~/.claude/.credentials.json`.
//! - **The Claude Code CLI** (`~\.local\bin\claude.exe`, npm, WinGet, …)
//!   — the only one that holds the CLI slot's refresh token.
//!
//! A name-only match therefore reports "Claude Code is running" whenever
//! Desktop is open, which gates every CLI switch behind an override,
//! keeps auto-rotation's "skip while CC is running" from ever firing and
//! starves the active account's background refresh. This module tells
//! the three apart by executable path, and checks the one exemption a
//! path cannot prove on its own — that a Code-tab binary is really
//! running under Desktop — against the process tree.
//!
//! ## Rules this module keeps
//!
//! **Uncertain means live.** A process whose executable path cannot be
//! read is [`ClaudeProcKind::Unattributed`] and still counts as holding
//! the CLI token: a false positive costs one skipped refresh, a false
//! negative signs a live session out.
//!
//! **Exemptions need positive evidence.** A CLI-token holder is let off
//! only when some source vouches for that exact process — its pid AND
//! its start time — so a recycled pid inherits nothing. A missing record
//! never exempts anyone. Desktop-hosted Claude Code is exempt only with
//! a Desktop process as its ancestor ([`classify_in_snapshot`]), and a
//! registry `entrypoint` only with a Desktop process as its direct
//! parent ([`desktop_registry_exemptions`]): both the bundled binary and
//! the inherited `entrypoint` can turn up in a plain terminal.
//!
//! **String-based, so tested everywhere.** The classifier works on path
//! strings, not `Path`, because a Windows path does not split on `\` on
//! macOS or Linux. Everything except the `sysinfo` snapshot is compiled
//! on every platform so the tests run on all CI hosts; off Windows the
//! production callers are legitimately absent (see `wsl_probe`).
#![cfg_attr(not(windows), allow(dead_code))]

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::Serialize;

use crate::path_utils::simplify_windows_path;
use crate::session_live::registry::{self, ProcessCheck};
use crate::session_live::types::PidRecord;

/// Publisher id in Claude Desktop's MSIX package family
/// (`Claude_pzs8sxrjxfjjc`). Same value as `desktop_backend::windows`'s
/// known family; it is part of the signing identity, not the version.
pub const DESKTOP_PUBLISHER_ID: &str = "pzs8sxrjxfjjc";

/// Registry `entrypoint` values that mean "this Claude Code was started
/// by Claude Desktop and authenticates through it". `local-agent` is a
/// desktop-host surface for transcript retention too, but nobody has
/// verified how it authenticates, so it is deliberately not here.
pub const DESKTOP_HOST_ENTRYPOINTS: &[&str] = &["claude-desktop", "claude-desktop-3p"];

/// The Windows folders the classifier anchors on. Callers fill it from
/// the environment ([`WinRoots::from_env`]); tests build it literally,
/// never by setting process-wide env vars.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WinRoots {
    /// `%LOCALAPPDATA%`, e.g. `C:\Users\u\AppData\Local`.
    pub local_app_data: Option<String>,
    /// `%APPDATA%`, e.g. `C:\Users\u\AppData\Roaming`.
    pub app_data: Option<String>,
    /// `%ProgramFiles%` and `%ProgramW6432%` (they differ for a 32-bit
    /// process).
    pub program_files: Vec<String>,
}

impl WinRoots {
    /// Read the roots from the environment, falling back to the
    /// known-folder answers `dirs` gives when a variable is unset.
    pub fn from_env() -> Self {
        let var = |name: &str| {
            std::env::var_os(name)
                .map(|v| v.to_string_lossy().into_owned())
                .filter(|v| !v.is_empty())
        };
        let local_app_data = var("LOCALAPPDATA")
            .or_else(|| dirs::data_local_dir().map(|d| d.to_string_lossy().into_owned()));
        let app_data =
            var("APPDATA").or_else(|| dirs::data_dir().map(|d| d.to_string_lossy().into_owned()));
        let mut program_files: Vec<String> = Vec::new();
        for pf in ["ProgramFiles", "ProgramW6432"].into_iter().filter_map(var) {
            if !program_files.iter().any(|p| p.eq_ignore_ascii_case(&pf)) {
                program_files.push(pf);
            }
        }
        Self {
            local_app_data,
            app_data,
            program_files,
        }
    }
}

/// What a process is, as far as the CLI slot's token is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeProcKind {
    /// Not a `claude.exe` at all.
    NotClaude,
    /// Claude Desktop or one of its Electron helpers.
    DesktopApp,
    /// The Claude Code binary Desktop runs for its Code tab.
    DesktopHostedCode,
    /// A Claude Code CLI — reads the shared login.
    Cli,
    /// A `claude.exe` whose executable path could not be read.
    Unattributed,
}

impl ClaudeProcKind {
    /// Could this process hold — and rotate — the CLI slot's refresh
    /// token? `Unattributed` answers yes: that is the safety bias.
    pub fn holds_cli_token(self) -> bool {
        matches!(self, Self::Cli | Self::Unattributed)
    }
}

/// Lower-case, `\`-separated, verbatim prefix stripped, no trailing
/// separator. The one normalisation both sides of every comparison go
/// through, so case, `/` and `\\?\` cannot make two spellings of one
/// folder disagree.
fn normalize(path: &str) -> String {
    let unified = path.replace('/', "\\");
    let mut s = simplify_windows_path(&unified).to_ascii_lowercase();
    while s.len() > 1 && s.ends_with('\\') {
        s.pop();
    }
    s
}

/// `path` lies strictly inside `dir`, at a separator boundary — so
/// `…\claude-code` does not contain `…\claude-codeX\claude.exe`.
fn is_under(path: &str, dir: &str) -> bool {
    path.len() > dir.len() + 1
        && path.starts_with(dir)
        && path.as_bytes().get(dir.len()) == Some(&b'\\')
}

fn parent_of(path: &str) -> Option<&str> {
    path.rfind('\\').map(|i| &path[..i])
}

/// Is `component` (lower-cased) the package *full name* of Claude
/// Desktop — `claude_<version>_<arch>__<publisher>`, the double
/// underscore being the empty resource id? The bare family name
/// `claude_<publisher>` is not: it is also the name of the user-writable
/// app-execution-alias folder under `%LOCALAPPDATA%\Microsoft\WindowsApps`.
fn is_desktop_package_full_name(component: &str) -> bool {
    let Some(middle) = component
        .strip_prefix("claude_")
        .and_then(|rest| rest.strip_suffix(&format!("__{DESKTOP_PUBLISHER_ID}")))
    else {
        return false;
    };
    match middle.split_once('_') {
        Some((version, arch)) => {
            !version.is_empty()
                && version.chars().all(|c| c.is_ascii_digit() || c == '.')
                && !arch.is_empty()
                && arch.chars().all(|c| c.is_ascii_alphanumeric())
        }
        None => false,
    }
}

/// Is any folder on the path an MSIX install folder of Claude Desktop
/// (`WindowsApps\Claude_<version>_<arch>__<publisher>`)? Any drive: a
/// Store app can be moved off the system drive, so this does not anchor
/// on `%ProgramFiles%`.
fn under_desktop_msix_package(norm: &str) -> bool {
    let parts: Vec<&str> = norm.split('\\').collect();
    parts
        .windows(2)
        .any(|w| w[0] == "windowsapps" && is_desktop_package_full_name(w[1]))
}

/// Classify one process from its image name and executable path alone.
///
/// This is the *path* verdict. For `DesktopHostedCode` it is only half
/// the evidence: Desktop's bundled Claude Code is a complete CC binary,
/// and run from a terminal it would read the shared login like any CLI.
/// [`classify_in_snapshot`] supplies the other half — a Desktop process
/// as its ancestor — and every live-session decision goes through it.
///
/// Rules, first match wins:
/// 1. image name other than `claude.exe` → `NotClaude`;
/// 2. no executable path → `Unattributed` (counts as live);
/// 3. inside `…\WindowsApps\Claude_<ver>_<arch>__pzs8sxrjxfjjc\` → `DesktopApp`;
/// 4. directly in `<LOCALAPPDATA>\Programs\Claude` or
///    `<ProgramFiles>\Claude` → `DesktopApp`;
/// 5. under `<APPDATA>\Claude\claude-code\` or
///    `<LOCALAPPDATA>\Packages\Claude_pzs8sxrjxfjjc\LocalCache\Roaming\Claude\claude-code\`
///    → `DesktopHostedCode`;
/// 6. anything else → `Cli`.
pub fn classify_windows(name: &str, exe: Option<&str>, roots: &WinRoots) -> ClaudeProcKind {
    if !name.eq_ignore_ascii_case("claude.exe") {
        return ClaudeProcKind::NotClaude;
    }
    let Some(exe) = exe.filter(|e| !e.trim().is_empty()) else {
        return ClaudeProcKind::Unattributed;
    };
    let exe = normalize(exe);

    if under_desktop_msix_package(&exe) {
        return ClaudeProcKind::DesktopApp;
    }

    let local = roots.local_app_data.as_deref().map(normalize);
    if let Some(dir) = parent_of(&exe) {
        let mut install_dirs: Vec<String> = roots
            .program_files
            .iter()
            .map(|pf| format!(r"{}\claude", normalize(pf)))
            .collect();
        if let Some(l) = &local {
            install_dirs.push(format!(r"{l}\programs\claude"));
        }
        if install_dirs.iter().any(|d| d == dir) {
            return ClaudeProcKind::DesktopApp;
        }
    }

    let hosted_code_dirs = [
        roots
            .app_data
            .as_deref()
            .map(|a| format!(r"{}\claude\claude-code", normalize(a))),
        local.as_ref().map(|l| {
            format!(
                r"{l}\packages\claude_{DESKTOP_PUBLISHER_ID}\localcache\roaming\claude\claude-code"
            )
        }),
    ];
    if hosted_code_dirs.iter().flatten().any(|d| is_under(&exe, d)) {
        return ClaudeProcKind::DesktopHostedCode;
    }

    ClaudeProcKind::Cli
}

/// The few facts about one process the classifier and the aggregations
/// need. Built from `sysinfo` by [`snapshot`] on Windows and literally
/// in tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcFacts {
    pub pid: u32,
    /// Image name as the kernel reports it (lower case on Windows).
    pub name: String,
    /// Executable path; `None` when it could not be read.
    pub exe: Option<String>,
    pub parent: Option<u32>,
    /// Unix seconds the process started, as `sysinfo` reports it.
    /// `0` means unknown, and an unknown start time never matches an
    /// exemption.
    pub start_secs: u64,
}

/// Is `p` vouched for by `exempt` — same pid AND same start time?
fn is_exempt(p: &ProcFacts, exempt: &HashMap<u32, u64>) -> bool {
    p.start_secs != 0 && exempt.get(&p.pid) == Some(&p.start_secs)
}

/// `child`'s recorded parent, if that pid is in the snapshot AND is
/// plausibly the same process that started `child`: both start times
/// known, the parent's no later than the child's. Windows never updates
/// a child's parent pid, so after the parent exits the number can belong
/// to an unrelated, younger process.
fn parent_in<'a>(child: &ProcFacts, by_pid: &HashMap<u32, &'a ProcFacts>) -> Option<&'a ProcFacts> {
    let parent = by_pid.get(&child.parent?)?;
    (parent.start_secs != 0 && child.start_secs != 0 && parent.start_secs <= child.start_secs)
        .then_some(*parent)
}

/// The kind of every `claude.exe` in `procs`, with the path verdict
/// checked against the process tree: a `DesktopHostedCode` path counts
/// only when its chain of `DesktopHostedCode` ancestors ends at a
/// `DesktopApp` process — the Code tab is Desktop's direct child, and
/// anything it runs with the same binary sits under it. Otherwise it is
/// `Cli`, the safe direction. All other kinds are the path verdict.
/// Processes that are not `claude.exe` are left out.
pub fn classify_in_snapshot(procs: &[ProcFacts], roots: &WinRoots) -> HashMap<u32, ClaudeProcKind> {
    let by_pid: HashMap<u32, &ProcFacts> = procs.iter().map(|p| (p.pid, p)).collect();
    let path_kind: HashMap<u32, ClaudeProcKind> = procs
        .iter()
        .map(|p| (p.pid, classify_windows(&p.name, p.exe.as_deref(), roots)))
        .filter(|(_, k)| *k != ClaudeProcKind::NotClaude)
        .collect();
    let corroborated = |p: &ProcFacts| -> bool {
        let mut seen: HashSet<u32> = HashSet::from([p.pid]);
        let mut cur = p;
        while let Some(parent) = parent_in(cur, &by_pid) {
            if !seen.insert(parent.pid) {
                return false;
            }
            match path_kind.get(&parent.pid) {
                Some(ClaudeProcKind::DesktopApp) => return true,
                Some(ClaudeProcKind::DesktopHostedCode) => cur = parent,
                _ => return false,
            }
        }
        false
    };
    procs
        .iter()
        .filter_map(|p| {
            let kind = *path_kind.get(&p.pid)?;
            let kind = if kind == ClaudeProcKind::DesktopHostedCode && !corroborated(p) {
                ClaudeProcKind::Cli
            } else {
                kind
            };
            Some((p.pid, kind))
        })
        .collect()
}

/// Every process that may hold the CLI slot's token and is not exempt,
/// with its kind ([`classify_in_snapshot`]), in `procs` order. Empty
/// means "no live CLI session".
///
/// `exempt` maps pid → start time (unix seconds) of processes known
/// not to read the shared login. A pid is let off only when its start
/// time matches; a recycled pid, or one whose start time is unknown,
/// stays live.
pub fn live_cli_holders(
    procs: &[ProcFacts],
    roots: &WinRoots,
    exempt: &HashMap<u32, u64>,
) -> Vec<(u32, ClaudeProcKind)> {
    let kinds = classify_in_snapshot(procs, roots);
    procs
        .iter()
        .filter_map(|p| {
            let kind = *kinds.get(&p.pid)?;
            (kind.holds_cli_token() && !is_exempt(p, exempt)).then_some((p.pid, kind))
        })
        .collect()
}

/// Is any Claude Desktop process (main or helper) in `procs`?
pub fn desktop_app_present(procs: &[ProcFacts], roots: &WinRoots) -> bool {
    procs
        .iter()
        .any(|p| classify_windows(&p.name, p.exe.as_deref(), roots) == ClaudeProcKind::DesktopApp)
}

/// The pids to end to quit Claude Desktop: every Desktop process whose
/// parent is not itself a Desktop process. Killing these with their
/// tree takes the helpers and the Code tab with them and touches no
/// CLI session — unlike `taskkill /IM Claude.exe`, whose image-name
/// filter is case-insensitive and matches every `claude.exe` there is.
pub fn desktop_app_root_pids(procs: &[ProcFacts], roots: &WinRoots) -> Vec<u32> {
    let desktop: HashSet<u32> = procs
        .iter()
        .filter(|p| {
            classify_windows(&p.name, p.exe.as_deref(), roots) == ClaudeProcKind::DesktopApp
        })
        .map(|p| p.pid)
        .collect();
    let mut roots_out: Vec<u32> = procs
        .iter()
        .filter(|p| desktop.contains(&p.pid))
        .filter(|p| !p.parent.is_some_and(|pp| desktop.contains(&pp)))
        .map(|p| p.pid)
        .collect();
    roots_out.sort_unstable();
    roots_out
}

/// Exemptions from Claude Code's own session registry: a live record
/// whose `entrypoint` says Desktop started it, for a process that
///
/// - began no later than the record says the session did — one that
///   began afterwards is a recycled pid wearing a dead session's record;
/// - and is a **direct child of a Desktop process** in the same snapshot.
///
/// The second condition is because the `entrypoint` is self-reported
/// and inherited: CC keeps an existing `CLAUDE_CODE_ENTRYPOINT` rather
/// than overwriting it, so a CLI started from the Code tab's shell
/// writes `claude-desktop` too while reading the shared login. The real
/// Code tab is Desktop's own child; such a CLI's parent is a shell.
pub fn desktop_registry_exemptions(
    records: &[PidRecord],
    procs: &[ProcFacts],
    roots: &WinRoots,
) -> HashMap<u32, u64> {
    let by_pid: HashMap<u32, &ProcFacts> = procs.iter().map(|p| (p.pid, p)).collect();
    records
        .iter()
        .filter(|r| {
            r.entrypoint
                .as_deref()
                .is_some_and(|e| DESKTOP_HOST_ENTRYPOINTS.contains(&e))
        })
        .filter_map(|r| {
            let p = by_pid.get(&r.pid)?;
            let started = i64::try_from(p.start_secs).ok()?;
            let fresh =
                p.start_secs != 0 && r.started_at_ms > 0 && started <= r.started_at_ms / 1000;
            let desktop_child = parent_in(p, &by_pid).is_some_and(|parent| {
                classify_windows(&parent.name, parent.exe.as_deref(), roots)
                    == ClaudeProcKind::DesktopApp
            });
            (fresh && desktop_child).then_some((r.pid, p.start_secs))
        })
        .collect()
}

/// Live registry records, with "live" judged against the same process
/// list the classifier sees rather than a second scan. An unreadable
/// registry yields nothing — it can only ever add exemptions, so
/// missing it errs towards "live".
pub fn registry_records(sessions_dir: &Path, procs: &[ProcFacts]) -> Vec<PidRecord> {
    struct SnapshotCheck(HashSet<u32>);
    impl ProcessCheck for SnapshotCheck {
        fn is_running(&self, pid: u32) -> bool {
            self.0.contains(&pid)
        }
    }
    let check = SnapshotCheck(procs.iter().map(|p| p.pid).collect());
    registry::poll_dir(sessions_dir, &check)
        .map(|o| o.live)
        .unwrap_or_default()
}

/// Read the live process table: one `sysinfo` refresh, with executable
/// paths, sorted by pid. An empty executable path is reported as
/// unreadable.
#[cfg(windows)]
pub fn snapshot() -> Vec<ProcFacts> {
    use sysinfo::{ProcessRefreshKind, RefreshKind, System, UpdateKind};
    // sysinfo 0.32 API: `new()` is the empty-refresh constructor;
    // `nothing()` is the 0.33+ rename.
    let sys = System::new_with_specifics(
        RefreshKind::new().with_processes(ProcessRefreshKind::new().with_exe(UpdateKind::Always)),
    );
    let mut procs: Vec<ProcFacts> = sys
        .processes()
        .values()
        .map(|p| ProcFacts {
            pid: p.pid().as_u32(),
            name: p.name().to_string_lossy().into_owned(),
            exe: p
                .exe()
                .map(|e| e.to_string_lossy().into_owned())
                .filter(|e| !e.is_empty()),
            parent: p.parent().map(|pp| pp.as_u32()),
            start_secs: p.start_time(),
        })
        .collect();
    // Pid order, so every caller's output is stable across scans.
    procs.sort_unstable_by_key(|p| p.pid);
    procs
}

/// The Windows-native live-session answer: scan, classify, and exempt
/// the Desktop-started sessions in CC's registry.
#[cfg(windows)]
pub fn scan_live_cli_holders() -> Vec<(u32, ClaudeProcKind)> {
    live_cli_holders_in(&snapshot())
}

/// [`scan_live_cli_holders`] over a snapshot the caller already took, for
/// a caller that needs more of each process than its pid.
#[cfg(windows)]
pub fn live_cli_holders_in(procs: &[ProcFacts]) -> Vec<(u32, ClaudeProcKind)> {
    let roots = WinRoots::from_env();
    let records = registry_records(&registry::default_sessions_dir(), procs);
    let exempt = desktop_registry_exemptions(&records, procs, &roots);
    live_cli_holders(procs, &roots, &exempt)
}

/// Is Claude Desktop (main process or a helper) running right now?
#[cfg(windows)]
pub fn desktop_app_running() -> bool {
    desktop_app_present(&snapshot(), &WinRoots::from_env())
}

#[cfg(test)]
#[path = "claude_proc_tests.rs"]
mod tests;
