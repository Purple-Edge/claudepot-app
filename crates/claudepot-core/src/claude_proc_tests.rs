//! Tests for `claude_proc`. Everything except the last two is a pure
//! string test and runs on every host; the paths are the ones observed
//! on a real Windows 11 machine with the Store build of Claude Desktop.

use super::*;

fn roots() -> WinRoots {
    WinRoots {
        local_app_data: Some(r"C:\Users\u\AppData\Local".to_string()),
        app_data: Some(r"C:\Users\u\AppData\Roaming".to_string()),
        program_files: vec![r"C:\Program Files".to_string()],
    }
}

fn kind(exe: &str) -> ClaudeProcKind {
    classify_windows("claude.exe", Some(exe), &roots())
}

const STORE_APP: &str =
    r"C:\Program Files\WindowsApps\Claude_2.26454.0.0_x64__pzs8sxrjxfjjc\app\claude.exe";
const CODE_TAB_VIRTUALIZED: &str = r"C:\Users\u\AppData\Local\Packages\Claude_pzs8sxrjxfjjc\LocalCache\Roaming\Claude\claude-code\2.1.289\e1f0154146bb\claude.exe";
const NATIVE_CLI: &str = r"C:\Users\u\.local\bin\claude.exe";

fn proc_at(pid: u32, exe: Option<&str>, parent: Option<u32>, start_secs: u64) -> ProcFacts {
    ProcFacts {
        pid,
        name: "claude.exe".to_string(),
        exe: exe.map(str::to_string),
        parent,
        start_secs,
    }
}

fn record(pid: u32, entrypoint: Option<&str>, started_at_ms: i64) -> PidRecord {
    PidRecord {
        pid,
        session_id: format!("session-{pid}"),
        cwd: r"C:\Users\u\project".to_string(),
        started_at_ms,
        updated_at_ms: None,
        version: Some("2.1.289".to_string()),
        kind: Some("interactive".to_string()),
        entrypoint: entrypoint.map(str::to_string),
        name: None,
        status: None,
        waiting_for: None,
        messaging_socket_path: None,
        peer_protocol: Some(1),
    }
}

/// This machine's Desktop: a main process, 11 helpers and the Code tab
/// as the main process's child.
fn desktop_with_code_tab() -> Vec<ProcFacts> {
    let mut procs = vec![proc_at(76880, Some(STORE_APP), Some(4000), 1_000)];
    for i in 0..11 {
        procs.push(proc_at(76900 + i, Some(STORE_APP), Some(76880), 1_001));
    }
    procs.push(proc_at(
        153320,
        Some(CODE_TAB_VIRTUALIZED),
        Some(76880),
        1_100,
    ));
    procs
}

// ---- classify_windows ----------------------------------------------

#[test]
fn store_app_under_windowsapps_is_desktop_app() {
    assert_eq!(kind(STORE_APP), ClaudeProcKind::DesktopApp);
    // Capitalised file name, as Get-Process spells it.
    assert_eq!(
        kind(r"C:\Program Files\WindowsApps\Claude_2.26454.0.0_x64__pzs8sxrjxfjjc\app\Claude.exe"),
        ClaudeProcKind::DesktopApp
    );
}

#[test]
fn store_app_moved_to_other_drive_is_desktop_app() {
    assert_eq!(
        kind(r"D:\WindowsApps\Claude_2.26454.0.0_x64__pzs8sxrjxfjjc\app\claude.exe"),
        ClaudeProcKind::DesktopApp
    );
}

#[test]
fn desktop_bundled_claude_code_is_desktop_hosted() {
    assert_eq!(
        kind(r"C:\Users\u\AppData\Roaming\Claude\claude-code\2.1.289\e1f0154146bb\claude.exe"),
        ClaudeProcKind::DesktopHostedCode
    );
}

#[test]
fn desktop_bundled_claude_code_matches_case_insensitively() {
    let mixed = WinRoots {
        local_app_data: Some(r"c:\users\U\appdata\local".to_string()),
        app_data: Some(r"C:\USERS\U\APPDATA\ROAMING".to_string()),
        program_files: vec![r"c:\program files".to_string()],
    };
    assert_eq!(
        classify_windows(
            "CLAUDE.EXE",
            Some(r"C:\Users\u\AppData\Roaming\CLAUDE\Claude-Code\2.1.289\x\Claude.exe"),
            &mixed
        ),
        ClaudeProcKind::DesktopHostedCode
    );
}

#[test]
fn msix_virtualized_claude_code_is_desktop_hosted() {
    assert_eq!(
        kind(CODE_TAB_VIRTUALIZED),
        ClaudeProcKind::DesktopHostedCode
    );
}

#[test]
fn native_cli_in_local_bin_is_cli() {
    assert_eq!(kind(NATIVE_CLI), ClaudeProcKind::Cli);
}

#[test]
fn npm_installed_cli_is_cli_not_desktop_hosted() {
    // Guards against a naive `contains("claude-code")`.
    assert_eq!(
        kind(
            r"C:\Users\u\AppData\Roaming\npm\node_modules\@anthropic-ai\claude-code\bin\claude.exe"
        ),
        ClaudeProcKind::Cli
    );
}

#[test]
fn winget_cli_is_cli() {
    assert_eq!(
        kind(
            r"C:\Users\u\AppData\Local\Microsoft\WinGet\Packages\Anthropic.ClaudeCode_Microsoft.Winget.Source_8wekyb3d8bbwe\claude.exe"
        ),
        ClaudeProcKind::Cli
    );
}

#[test]
fn unreadable_exe_is_unattributed_and_holds_token() {
    let k = classify_windows("claude.exe", None, &roots());
    assert_eq!(k, ClaudeProcKind::Unattributed);
    assert!(
        k.holds_cli_token(),
        "the safety bias: unknown counts as live"
    );
    // An empty path is no better than none.
    assert_eq!(
        classify_windows("claude.exe", Some(""), &roots()),
        ClaudeProcKind::Unattributed
    );
}

#[test]
fn squirrel_desktop_install_stays_exempt() {
    assert_eq!(
        kind(r"C:\Users\u\AppData\Local\Programs\Claude\Claude.exe"),
        ClaudeProcKind::DesktopApp
    );
    assert_eq!(
        kind(r"C:\Program Files\Claude\Claude.exe"),
        ClaudeProcKind::DesktopApp
    );
}

#[test]
fn squirrel_install_root_matches_with_trailing_separator_and_case() {
    let r = WinRoots {
        local_app_data: Some(r"C:\USERS\U\APPDATA\LOCAL\".to_string()),
        app_data: None,
        program_files: vec![r"C:/Program Files/".to_string()],
    };
    assert_eq!(
        classify_windows(
            "claude.exe",
            Some(r"c:\users\u\appdata\local\programs\claude\claude.exe"),
            &r
        ),
        ClaudeProcKind::DesktopApp
    );
    assert_eq!(
        classify_windows(
            "claude.exe",
            Some(r"C:\Program Files\Claude\claude.exe"),
            &r
        ),
        ClaudeProcKind::DesktopApp
    );
}

#[test]
fn file_deeper_than_squirrel_install_dir_is_not_desktop() {
    // Rule 4 is "directly in" the install dir, as it always was.
    assert_eq!(
        kind(r"C:\Users\u\AppData\Local\Programs\Claude\resources\bin\claude.exe"),
        ClaudeProcKind::Cli
    );
}

#[test]
fn verbatim_prefix_is_ignored() {
    assert_eq!(
        kind(
            r"\\?\C:\Program Files\WindowsApps\Claude_2.26454.0.0_x64__pzs8sxrjxfjjc\app\claude.exe"
        ),
        ClaudeProcKind::DesktopApp
    );
    assert_eq!(
        kind(r"\\?\C:\Users\u\AppData\Roaming\Claude\claude-code\2.1.289\x\claude.exe"),
        ClaudeProcKind::DesktopHostedCode
    );
    assert_eq!(
        kind(r"\\?\C:\Users\u\.local\bin\claude.exe"),
        ClaudeProcKind::Cli
    );
}

#[test]
fn roaming_profile_on_unc_share_is_desktop_hosted() {
    let r = WinRoots {
        local_app_data: Some(r"C:\Users\u\AppData\Local".to_string()),
        app_data: Some(r"\\server\profiles\u\AppData\Roaming".to_string()),
        program_files: vec![],
    };
    let exe = r"\\server\profiles\u\AppData\Roaming\Claude\claude-code\2.1.289\x\claude.exe";
    assert_eq!(
        classify_windows("claude.exe", Some(exe), &r),
        ClaudeProcKind::DesktopHostedCode
    );
    // Same file through the verbatim UNC spelling.
    let verbatim =
        r"\\?\UNC\server\profiles\u\AppData\Roaming\Claude\claude-code\2.1.289\x\claude.exe";
    assert_eq!(
        classify_windows("claude.exe", Some(verbatim), &r),
        ClaudeProcKind::DesktopHostedCode
    );
}

#[test]
fn forward_slashes_classify_like_backslashes() {
    assert_eq!(
        kind("C:/Users/u/AppData/Roaming/Claude/claude-code/2.1.289/x/claude.exe"),
        ClaudeProcKind::DesktopHostedCode
    );
}

#[test]
fn unix_shaped_path_is_cli() {
    assert_eq!(kind("/usr/local/bin/claude.exe"), ClaudeProcKind::Cli);
}

#[test]
fn lookalike_package_is_not_desktop() {
    assert_eq!(
        kind(r"C:\Program Files\WindowsApps\ClaudeCompanion_1.0_x64__abc\claude.exe"),
        ClaudeProcKind::Cli
    );
    // Right name, someone else's publisher id.
    assert_eq!(
        kind(r"C:\Program Files\WindowsApps\Claude_1.0.0.0_x64__0000000000000\app\claude.exe"),
        ClaudeProcKind::Cli
    );
    assert_eq!(
        kind(
            r"C:\Users\u\AppData\Local\Packages\Claude_0000000000000\LocalCache\Roaming\Claude\claude-code\1\claude.exe"
        ),
        ClaudeProcKind::Cli
    );
    // The user-writable app-execution-alias folder carries the bare
    // family name, not the package full name.
    assert_eq!(
        kind(r"C:\Users\u\AppData\Local\Microsoft\WindowsApps\Claude_pzs8sxrjxfjjc\claude.exe"),
        ClaudeProcKind::Cli
    );
    // Family-name-shaped folders anywhere else are not the package
    // either, nor are full names missing a version or an architecture,
    // or with a single underscore before the publisher.
    for exe in [
        r"C:\src\proj\WindowsApps\Claude_pzs8sxrjxfjjc\claude.exe",
        r"C:\Program Files\WindowsApps\Claude__x64__pzs8sxrjxfjjc\app\claude.exe",
        r"C:\Program Files\WindowsApps\Claude_2.26454.0.0___pzs8sxrjxfjjc\app\claude.exe",
        r"C:\Program Files\WindowsApps\Claude_2.26454.0.0_x64_pzs8sxrjxfjjc\app\claude.exe",
        r"C:\Program Files\WindowsApps\Claude_beta_x64__pzs8sxrjxfjjc\app\claude.exe",
    ] {
        assert_eq!(kind(exe), ClaudeProcKind::Cli, "{exe}");
    }
}

#[test]
fn store_app_full_name_accepts_other_architectures() {
    assert_eq!(
        kind(r"C:\Program Files\WindowsApps\Claude_3.0.1.0_arm64__pzs8sxrjxfjjc\app\claude.exe"),
        ClaudeProcKind::DesktopApp
    );
}

#[test]
fn prefix_needs_a_separator_boundary() {
    assert_eq!(
        kind(r"C:\Users\u\AppData\Roaming\Claude\claude-codeX\claude.exe"),
        ClaudeProcKind::Cli
    );
    assert_eq!(
        kind(r"C:\Users\u\AppData\Roaming\ClaudeX\claude-code\1\claude.exe"),
        ClaudeProcKind::Cli
    );
}

#[test]
fn unknown_roots_fall_back_to_cli() {
    // Without the roots nothing can be attributed to Desktop's Code
    // tab — the safe direction. The Store app needs no root.
    let none = WinRoots::default();
    assert_eq!(
        classify_windows("claude.exe", Some(CODE_TAB_VIRTUALIZED), &none),
        ClaudeProcKind::Cli
    );
    assert_eq!(
        classify_windows("claude.exe", Some(STORE_APP), &none),
        ClaudeProcKind::DesktopApp
    );
}

#[test]
fn non_claude_image_is_ignored() {
    // Documents the legacy gap: a JS npm install runs as node.exe.
    assert_eq!(
        classify_windows(
            "node.exe",
            Some(r"C:\Program Files\nodejs\node.exe"),
            &roots()
        ),
        ClaudeProcKind::NotClaude
    );
    assert_eq!(
        classify_windows("claude-helper.exe", Some(NATIVE_CLI), &roots()),
        ClaudeProcKind::NotClaude
    );
}

#[test]
fn only_cli_and_unattributed_hold_the_token() {
    use ClaudeProcKind::*;
    assert!(Cli.holds_cli_token());
    assert!(Unattributed.holds_cli_token());
    assert!(!DesktopApp.holds_cli_token());
    assert!(!DesktopHostedCode.holds_cli_token());
    assert!(!NotClaude.holds_cli_token());
}

// ---- live_cli_holders ---------------------------------------------------

#[test]
fn desktop_and_its_code_tab_alone_are_not_live() {
    let procs = desktop_with_code_tab();
    assert_eq!(procs.len(), 13);
    assert!(live_cli_holders(&procs, &roots(), &HashMap::new()).is_empty());
}

#[test]
fn one_cli_among_desktop_processes_is_live() {
    let mut procs = desktop_with_code_tab();
    procs.push(proc_at(147088, Some(NATIVE_CLI), Some(9000), 2_000));
    procs.push(ProcFacts {
        pid: 1,
        name: "explorer.exe".to_string(),
        exe: Some(r"C:\Windows\explorer.exe".to_string()),
        parent: None,
        start_secs: 10,
    });
    assert_eq!(
        live_cli_holders(&procs, &roots(), &HashMap::new()),
        vec![(147088, ClaudeProcKind::Cli)]
    );
}

#[test]
fn unattributed_process_is_live() {
    let procs = vec![proc_at(500, None, None, 2_000)];
    assert_eq!(
        live_cli_holders(&procs, &roots(), &HashMap::new()),
        vec![(500, ClaudeProcKind::Unattributed)]
    );
}

#[test]
fn exemption_with_matching_start_time_releases_a_cli() {
    let procs = vec![
        proc_at(147088, Some(NATIVE_CLI), None, 2_000),
        proc_at(158804, Some(NATIVE_CLI), None, 2_100),
    ];
    let exempt: HashMap<u32, u64> = [(147088, 2_000)].into_iter().collect();
    assert_eq!(
        live_cli_holders(&procs, &roots(), &exempt),
        vec![(158804, ClaudeProcKind::Cli)]
    );
}

#[test]
fn exemption_for_a_recycled_pid_is_ignored() {
    let procs = vec![proc_at(147088, Some(NATIVE_CLI), None, 2_500)];
    let exempt: HashMap<u32, u64> = [(147088, 2_000)].into_iter().collect();
    assert_eq!(live_cli_holders(&procs, &roots(), &exempt).len(), 1);
}

#[test]
fn unknown_start_time_is_never_exempt() {
    let procs = vec![proc_at(147088, Some(NATIVE_CLI), None, 0)];
    let exempt: HashMap<u32, u64> = [(147088, 0)].into_iter().collect();
    assert_eq!(live_cli_holders(&procs, &roots(), &exempt).len(), 1);
}

// ---- registry exemptions ---------------------------------------------

/// Desktop's main process, started well before any session below.
fn desktop_root() -> ProcFacts {
    proc_at(76880, Some(STORE_APP), Some(4000), 1_699_999_000)
}

fn shell(pid: u32, start_secs: u64) -> ProcFacts {
    ProcFacts {
        pid,
        name: "cmd.exe".to_string(),
        exe: Some(r"C:\Windows\System32\cmd.exe".to_string()),
        parent: Some(76880),
        start_secs,
    }
}

#[test]
fn registry_desktop_entrypoint_exempts_unattributed_pid() {
    // Exe unreadable, but CC's own registry says Desktop started it, the
    // process is older than the record, and Desktop is its direct parent.
    let procs = vec![
        desktop_root(),
        proc_at(153320, None, Some(76880), 1_700_000_000),
    ];
    let records = vec![record(153320, Some("claude-desktop"), 1_700_000_005_000)];
    let exempt = desktop_registry_exemptions(&records, &procs, &roots());
    assert_eq!(exempt.get(&153320), Some(&1_700_000_000));
    assert!(live_cli_holders(&procs, &roots(), &exempt).is_empty());
}

#[test]
fn registry_desktop_entrypoint_on_a_shell_child_does_not_exempt() {
    // A CLI started from the Code tab's shell inherits
    // CLAUDE_CODE_ENTRYPOINT=claude-desktop and registers with it, but
    // reads the shared login. Its parent is the shell, not Desktop.
    let procs = vec![
        desktop_root(),
        shell(9100, 1_699_999_500),
        proc_at(147088, Some(NATIVE_CLI), Some(9100), 1_700_000_000),
        proc_at(147089, None, Some(9100), 1_700_000_000),
    ];
    let records = vec![
        record(147088, Some("claude-desktop"), 1_700_000_005_000),
        record(147089, Some("claude-desktop"), 1_700_000_005_000),
    ];
    let exempt = desktop_registry_exemptions(&records, &procs, &roots());
    assert!(exempt.is_empty());
    assert_eq!(
        live_cli_holders(&procs, &roots(), &exempt),
        vec![
            (147088, ClaudeProcKind::Cli),
            (147089, ClaudeProcKind::Unattributed)
        ]
    );
}

#[test]
fn registry_desktop_entrypoint_without_its_parent_in_the_snapshot_does_not_exempt() {
    let procs = vec![proc_at(153320, None, Some(76880), 1_700_000_000)];
    let records = vec![record(153320, Some("claude-desktop"), 1_700_000_005_000)];
    assert!(desktop_registry_exemptions(&records, &procs, &roots()).is_empty());
}

#[test]
fn registry_exemption_ignored_when_parent_pid_was_recycled() {
    // The process's recorded parent pid now belongs to a Desktop process
    // that started after it — so that is not the process that started it.
    let procs = vec![
        proc_at(76880, Some(STORE_APP), None, 1_700_000_050),
        proc_at(153320, None, Some(76880), 1_700_000_000),
    ];
    let records = vec![record(153320, Some("claude-desktop"), 1_700_000_005_000)];
    assert!(desktop_registry_exemptions(&records, &procs, &roots()).is_empty());
}

#[test]
fn registry_exemption_ignored_when_pid_started_later() {
    // The record belongs to a dead session; its pid was reused by a
    // process that started afterwards.
    let procs = vec![
        desktop_root(),
        proc_at(153320, None, Some(76880), 1_700_000_100),
    ];
    let records = vec![record(153320, Some("claude-desktop"), 1_700_000_005_000)];
    let exempt = desktop_registry_exemptions(&records, &procs, &roots());
    assert!(exempt.is_empty());
    assert_eq!(live_cli_holders(&procs, &roots(), &exempt).len(), 1);
}

#[test]
fn missing_registry_record_does_not_exempt() {
    let procs = vec![
        desktop_root(),
        proc_at(153320, None, Some(76880), 1_700_000_000),
    ];
    let exempt = desktop_registry_exemptions(&[], &procs, &roots());
    assert!(exempt.is_empty());
    assert_eq!(live_cli_holders(&procs, &roots(), &exempt).len(), 1);
}

#[test]
fn registry_cli_entrypoint_does_not_exempt() {
    let procs = vec![
        desktop_root(),
        proc_at(147088, Some(NATIVE_CLI), Some(76880), 1_700_000_000),
        proc_at(147089, None, Some(76880), 1_700_000_000),
    ];
    let records = vec![
        record(147088, Some("cli"), 1_700_000_005_000),
        record(147089, None, 1_700_000_005_000),
    ];
    assert!(desktop_registry_exemptions(&records, &procs, &roots()).is_empty());
}

#[test]
fn registry_record_for_unknown_start_time_does_not_exempt() {
    let procs = vec![desktop_root(), proc_at(153320, None, Some(76880), 0)];
    let records = vec![record(153320, Some("claude-desktop"), 1_700_000_005_000)];
    assert!(desktop_registry_exemptions(&records, &procs, &roots()).is_empty());
}

#[test]
fn registry_records_reads_desktop_fixture_against_the_snapshot() {
    let td = tempfile::TempDir::new().unwrap();
    let src = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/session_live/testdata/pid/99010-claude-desktop.json");
    std::fs::copy(&src, td.path().join("99010.json")).unwrap();
    std::fs::copy(&src, td.path().join("99011.json")).unwrap();
    // 99010 is in the snapshot; 99011 is not, so its file is stale.
    let procs = vec![
        proc_at(76880, Some(STORE_APP), None, 1_776_699_000),
        proc_at(99010, None, Some(76880), 1_776_699_999),
    ];
    let records = registry_records(td.path(), &procs);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].pid, 99010);
    let exempt = desktop_registry_exemptions(&records, &procs, &roots());
    assert_eq!(exempt.get(&99010), Some(&1_776_699_999));
    // Read-only: the stale file is left alone.
    assert!(td.path().join("99011.json").exists());
}

// ---- hosted code needs Desktop as its ancestor ------------------------------

#[test]
fn hosted_code_run_from_a_terminal_is_cli() {
    // The bundled binary is a full CC; started by hand it reads the
    // shared login like any CLI.
    let procs = vec![
        shell(9100, 1_000),
        proc_at(153320, Some(CODE_TAB_VIRTUALIZED), Some(9100), 1_100),
    ];
    assert_eq!(
        live_cli_holders(&procs, &roots(), &HashMap::new()),
        vec![(153320, ClaudeProcKind::Cli)]
    );
}

#[test]
fn hosted_code_without_a_parent_in_the_snapshot_is_cli() {
    let procs = vec![proc_at(
        153320,
        Some(CODE_TAB_VIRTUALIZED),
        Some(76880),
        1_100,
    )];
    assert_eq!(
        classify_in_snapshot(&procs, &roots()).get(&153320),
        Some(&ClaudeProcKind::Cli)
    );
}

#[test]
fn hosted_code_spawned_by_the_code_tab_is_desktop_hosted() {
    let mut procs = desktop_with_code_tab();
    procs.push(proc_at(
        153400,
        Some(CODE_TAB_VIRTUALIZED),
        Some(153320),
        1_200,
    ));
    let kinds = classify_in_snapshot(&procs, &roots());
    assert_eq!(kinds.get(&153320), Some(&ClaudeProcKind::DesktopHostedCode));
    assert_eq!(kinds.get(&153400), Some(&ClaudeProcKind::DesktopHostedCode));
    assert!(live_cli_holders(&procs, &roots(), &HashMap::new()).is_empty());
}

#[test]
fn hosted_code_under_a_hand_started_hosted_code_is_cli() {
    let procs = vec![
        shell(9100, 1_000),
        proc_at(153320, Some(CODE_TAB_VIRTUALIZED), Some(9100), 1_100),
        proc_at(153400, Some(CODE_TAB_VIRTUALIZED), Some(153320), 1_200),
    ];
    assert_eq!(live_cli_holders(&procs, &roots(), &HashMap::new()).len(), 2);
}

#[test]
fn hosted_code_whose_parent_pid_was_recycled_is_cli() {
    // Desktop exited; its pid now belongs to a younger Desktop process.
    let procs = vec![
        proc_at(76880, Some(STORE_APP), None, 2_000),
        proc_at(153320, Some(CODE_TAB_VIRTUALIZED), Some(76880), 1_100),
    ];
    assert_eq!(
        classify_in_snapshot(&procs, &roots()).get(&153320),
        Some(&ClaudeProcKind::Cli)
    );
}

#[test]
fn hosted_code_with_unknown_start_times_is_cli() {
    let procs = vec![
        proc_at(76880, Some(STORE_APP), None, 0),
        proc_at(153320, Some(CODE_TAB_VIRTUALIZED), Some(76880), 1_100),
    ];
    assert_eq!(
        classify_in_snapshot(&procs, &roots()).get(&153320),
        Some(&ClaudeProcKind::Cli)
    );
}

#[test]
fn hosted_code_parent_cycle_is_cli() {
    // Two hosted-code processes naming each other as parent (only
    // possible with recycled pids and equal start times) never reach
    // Desktop, and the walk terminates.
    let procs = vec![
        proc_at(500, Some(CODE_TAB_VIRTUALIZED), Some(501), 1_000),
        proc_at(501, Some(CODE_TAB_VIRTUALIZED), Some(500), 1_000),
    ];
    let kinds = classify_in_snapshot(&procs, &roots());
    assert_eq!(kinds.get(&500), Some(&ClaudeProcKind::Cli));
    assert_eq!(kinds.get(&501), Some(&ClaudeProcKind::Cli));
}

#[test]
fn snapshot_classification_leaves_out_non_claude_processes() {
    let procs = vec![shell(9100, 1_000), desktop_root()];
    let kinds = classify_in_snapshot(&procs, &roots());
    assert_eq!(kinds.len(), 1);
    assert_eq!(kinds.get(&76880), Some(&ClaudeProcKind::DesktopApp));
}

#[test]
fn missing_registry_dir_yields_no_records() {
    let td = tempfile::TempDir::new().unwrap();
    let procs = vec![proc_at(99010, None, None, 1)];
    assert!(registry_records(&td.path().join("absent"), &procs).is_empty());
}

// ---- desktop process tree -------------------------------------------------

#[test]
fn desktop_root_is_the_desktop_process_whose_parent_is_not_desktop() {
    let mut procs = desktop_with_code_tab();
    procs.push(proc_at(147088, Some(NATIVE_CLI), Some(9000), 2_000));
    assert_eq!(desktop_app_root_pids(&procs, &roots()), vec![76880]);
}

#[test]
fn cli_and_code_tab_are_never_kill_roots() {
    let procs = vec![
        proc_at(147088, Some(NATIVE_CLI), None, 2_000),
        proc_at(153320, Some(CODE_TAB_VIRTUALIZED), None, 2_000),
    ];
    assert!(desktop_app_root_pids(&procs, &roots()).is_empty());
    assert!(!desktop_app_present(&procs, &roots()));
}

#[test]
fn desktop_presence_is_detected_from_a_helper_alone() {
    let procs = vec![proc_at(76901, Some(STORE_APP), Some(76880), 1_001)];
    assert!(desktop_app_present(&procs, &roots()));
    // Parent gone: the orphaned helper is its own root.
    assert_eq!(desktop_app_root_pids(&procs, &roots()), vec![76901]);
}

// ---- Windows only ----------------------------------------------------------

/// The `sysinfo` glue cannot be fed a synthetic process table, so this
/// is its only automated coverage: on the Windows CI runner, the scan
/// and every classification run to completion.
#[cfg(windows)]
#[test]
fn snapshot_and_classification_run_without_panicking() {
    let procs = snapshot();
    assert!(!procs.is_empty(), "a live system has processes");
    let roots = WinRoots::from_env();
    assert!(roots.local_app_data.is_some());
    let _ = live_cli_holders(&procs, &roots, &HashMap::new());
    let _ = desktop_app_root_pids(&procs, &roots);
    let _ = scan_live_cli_holders();
    let _ = desktop_app_running();
}

/// Prints every `claude*` process on this machine with its path verdict
/// and its snapshot (tree-checked) kind. Run by hand to confirm the
/// classifier against reality:
/// `cargo test -p claudepot-core claude_proc -- --ignored --nocapture`.
#[cfg(windows)]
#[test]
#[ignore]
fn diagnostic_print_every_claude_process() {
    let roots = WinRoots::from_env();
    println!("roots: {roots:?}");
    let procs = snapshot();
    let kinds = classify_in_snapshot(&procs, &roots);
    let mut claude: Vec<&ProcFacts> = procs
        .iter()
        .filter(|p| p.name.to_ascii_lowercase().contains("claude"))
        .collect();
    claude.sort_by_key(|p| p.pid);
    for p in &claude {
        let path_kind = classify_windows(&p.name, p.exe.as_deref(), &roots);
        println!(
            "pid={} parent={:?} start={} name={:?} exe={:?} path_kind={:?} kind={:?}",
            p.pid,
            p.parent,
            p.start_secs,
            p.name,
            p.exe,
            path_kind,
            kinds.get(&p.pid)
        );
    }
    let records = registry_records(
        &crate::session_live::registry::default_sessions_dir(),
        &procs,
    );
    println!(
        "registry exemptions: {:?}",
        desktop_registry_exemptions(&records, &procs, &roots)
    );
    println!("desktop roots: {:?}", desktop_app_root_pids(&procs, &roots));
    println!("live CLI holders: {:?}", scan_live_cli_holders());
}
