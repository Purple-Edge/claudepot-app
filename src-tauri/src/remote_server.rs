//! Hosting the remote panel's HTTP surface inside this process.
//!
//! ## Why in-process and not a daemon
//!
//! `remote::approval` arms Claude Code's `PermissionRequest` hook for
//! exactly as long as a server is up, and `docs/notes/remote-control.md`
//! is explicit that
//! *"that coupling is what makes it acceptable to hand a network client
//! the ability to grant a permission at all."* A launchd/systemd daemon
//! makes "as long as the surface is up" mean "always", which converts a
//! session-scoped capability — approve a tool call, i.e. code execution
//! as this user — into a permanent one. That is the trade the
//! peer-inbound grant already refuses by narrowing temporally.
//!
//! The cost is real and must be said in the UI rather than discovered:
//! **quitting Claudepot stops the remote surface.** `stop_on_exit` is
//! the enforcement; the pane's copy is the disclosure.
//!
//! ## What this module is not
//!
//! It holds no policy. Which addresses may be bound, whether TLS is
//! required, what a password is worth — all of that is
//! `claudepot_core::remote`, and every verb goes through
//! `remote::service` so the CLI and this cannot drift.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use claudepot_core::remote::server::{router, AppState};
use claudepot_core::remote::service::{ApprovalHook, FilePersist};
use claudepot_core::remote::{config, serve, store as device_store};
use tokio::sync::{oneshot, Mutex as AsyncMutex};

/// What a running server is, from the supervisor's side.
struct Running {
    /// Graceful stop. `Some` until taken by `stop`.
    ///
    /// A signal rather than `JoinHandle::abort`, and for the same
    /// reason the CLI selects on SIGTERM: `ApprovalHook`'s `Drop` is
    /// what uninstalls CC's hook, and dropping it on a normal path is
    /// easier to reason about than dropping it mid-cancellation. Abort
    /// stays as the backstop below.
    stop: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
    url: String,
    tls: bool,
}

#[derive(Default)]
struct Inner {
    running: Option<Running>,
    /// Why the last start or the last serve loop failed.
    ///
    /// Kept because a spawned `serve` that dies after a successful
    /// `listen` — a certificate that loads and then does not handshake,
    /// a port stolen in the window between the two — has nowhere else
    /// to report. Without it the pane would show "not serving" with no
    /// reason, which is the shape of failure that gets diagnosed by
    /// reading logs nobody opens.
    last_error: Option<String>,
    /// Warnings from arming the approval hook. Empty is the good case;
    /// a non-empty list means approval-from-the-phone is off while
    /// everything else works, which the user would otherwise discover
    /// by tapping Allow and having nothing happen.
    warnings: Vec<String>,
}

/// How long `stop` waits for the serve task to finish after signalling
/// it before aborting it.
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// Tauri-managed. One server per process.
#[derive(Default)]
pub struct RemoteServerState {
    inner: Arc<AsyncMutex<Inner>>,
}

/// What the renderer needs about the process-local server.
///
/// Deliberately NOT "is the surface up" — that question is answered by
/// `remote::service::status`'s `serving`, which reads the heartbeat and
/// is therefore true of a server this process did not start (a
/// `claudepot remote serve` in a terminal, say). The two differ, and a
/// pane that conflated them would offer a Stop button for a process it
/// cannot stop.
#[derive(Debug, Clone, Default)]
pub struct LocalServer {
    pub running_here: bool,
    pub url: Option<String>,
    pub tls: bool,
    pub last_error: Option<String>,
    pub warnings: Vec<String>,
}

/// Why a start did not happen.
///
/// `Display` is hand-written rather than derived: `thiserror` belongs to
/// `claudepot-core` per `.claude/rules/rust-conventions.md`, and one
/// enum does not justify pulling the dependency into this crate.
#[derive(Debug)]
pub enum StartError {
    Disabled,
    NoPassword,
    ConfigUnreadable(String),
    DevicesUnreadable(String),
    Serve(serve::ServeError),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disabled => write!(f, "the remote surface is disabled — enable it first"),
            Self::NoPassword => write!(f, "no password is set — set one before starting"),
            Self::ConfigUnreadable(e) => write!(f, "could not read the remote configuration: {e}"),
            Self::DevicesUnreadable(e) => {
                write!(f, "could not read the paired-device records: {e}")
            }
            // Transparent: `ServeError` already says whether the address
            // was refused, the certificate was unusable, or the port was
            // taken, and wrapping it would double the sentence.
            Self::Serve(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for StartError {}

impl From<serve::ServeError> for StartError {
    fn from(e: serve::ServeError) -> Self {
        Self::Serve(e)
    }
}

impl RemoteServerState {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn describe(&self) -> LocalServer {
        let inner = self.inner.lock().await;
        let (url, tls) = match &inner.running {
            Some(r) => (Some(r.url.clone()), r.tls),
            None => (None, false),
        };
        LocalServer {
            running_here: inner.running.is_some(),
            url,
            tls,
            last_error: inner.last_error.clone(),
            warnings: inner.warnings.clone(),
        }
    }

    /// Start serving. Returns the URL.
    ///
    /// Starting while already running is **not** an error: it returns
    /// the URL of the server that is already up. A second bind on the
    /// same port would fail with an error about the port rather than
    /// about the double-start, which tells the user the wrong thing.
    pub async fn start(&self) -> Result<String, StartError> {
        let mut inner = self.inner.lock().await;
        if let Some(r) = &inner.running {
            return Ok(r.url.clone());
        }
        let StartInputs {
            server_cfg,
            approvals,
            state,
        } = load_start_inputs()?;

        // Bind once here so a refused address, a missing certificate or
        // a busy port is reported to the caller — a Settings pane — and
        // not only to a log. `listen` is dropped immediately so `serve`
        // can take the same address; racy in principle, and a port
        // stolen in that window surfaces as a plain bind error from the
        // task, which is what `last_error` is for.
        let (listener, info) = serve::listen(&server_cfg).await?;
        drop(listener);

        let url = info.url();
        let serve_loop = async move {
            serve::serve(&server_cfg, router(state))
                .await
                .map_err(|e| e.to_string())
        };
        // Armed inside the task (see `supervise`), so its `Drop` runs
        // when the task ends however it ends.
        let arm = move || {
            let hook = ApprovalHook::arm(approvals);
            let warnings = hook.warnings.clone();
            (Box::new(hook) as HookGuard, warnings)
        };
        supervise(
            &mut inner,
            self.inner.clone(),
            url.clone(),
            info.tls,
            serve_loop,
            arm,
        );
        Ok(url)
    }

    /// Stop serving. `false` when nothing was running here.
    pub async fn stop(&self) -> bool {
        self.stop_within(STOP_TIMEOUT).await
    }

    async fn stop_within(&self, timeout: Duration) -> bool {
        let mut inner = self.inner.lock().await;
        let Some(mut running) = inner.running.take() else {
            return false;
        };
        // Drop the lock before awaiting the task: the task takes this
        // same lock on its way out, so holding it here deadlocks.
        drop(inner);

        if let Some(tx) = running.stop.take() {
            let _ = tx.send(());
        }
        // Bounded: a serve loop that ignores the signal must not hang
        // application quit. The abort is the backstop the graceful
        // signal is preferred over, not a substitute for it.
        if tokio::time::timeout(timeout, &mut running.task)
            .await
            .is_err()
        {
            tracing::warn!("remote surface did not stop in time; aborting the task");
            running.task.abort();
        }
        true
    }
}

/// What `start` needs once the configuration has been read and checked.
struct StartInputs {
    server_cfg: config::ServerConfig,
    approvals: bool,
    state: Arc<AsyncMutex<AppState>>,
}

/// Load and validate the remote configuration and the paired devices,
/// and build the server's shared state.
fn load_start_inputs() -> Result<StartInputs, StartError> {
    let loaded = config::load().map_err(|e| StartError::ConfigUnreadable(e.to_string()))?;
    let cfg = loaded.value;
    if !cfg.server.enabled {
        return Err(StartError::Disabled);
    }
    if cfg.password_hash.is_none() {
        return Err(StartError::NoPassword);
    }
    let devices = device_store::load()
        .map_err(|e| StartError::DevicesUnreadable(e.to_string()))?
        .value;
    let server_cfg = cfg.server.clone();
    // Read before `cfg` moves into the shared state below.
    let approvals = cfg.approvals_enabled;
    let state = Arc::new(AsyncMutex::new(AppState {
        config: cfg,
        devices,
        persist: Box::new(FilePersist),
        idempotency: claudepot_core::remote::idempotency::Idempotency::new(),
        challenges: claudepot_core::remote::passkey::Challenges::new(),
    }));
    Ok(StartInputs {
        server_cfg,
        approvals,
        state,
    })
}

/// Whatever must live exactly as long as the serve loop — CC's approval
/// hook in production. Dropped when the task ends.
type HookGuard = Box<dyn Send>;

/// Spawn the serve task and record it as running.
///
/// Inside the task: `arm` runs first and its guard is held until the
/// loop ends, however it ends — holding it beside the handle instead
/// would leave CC's hook installed for any exit path that forgot to take
/// it out. The loop runs until it fails or `stop` signals it; a failure
/// lands in `last_error`, because a loop that dies after a successful
/// `listen` has nowhere else to report.
fn supervise(
    inner: &mut Inner,
    shared: Arc<AsyncMutex<Inner>>,
    url: String,
    tls: bool,
    serve_loop: impl Future<Output = Result<(), String>> + Send + 'static,
    arm: impl FnOnce() -> (HookGuard, Vec<String>) + Send + 'static,
) {
    let (tx, rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        let (guard, warnings) = arm();
        if !warnings.is_empty() {
            shared.lock().await.warnings = warnings;
        }
        let outcome = tokio::select! {
            result = serve_loop => result.err(),
            _ = rx => None,
        };
        // Stops the heartbeat and takes CC's hook back out.
        drop(guard);

        let mut inner = shared.lock().await;
        inner.running = None;
        if let Some(e) = outcome {
            tracing::error!(error = %e, "remote surface stopped on an error");
            inner.last_error = Some(e);
        }
    });
    inner.running = Some(Running {
        stop: Some(tx),
        task,
        url,
        tls,
    });
    inner.last_error = None;
    inner.warnings.clear();
}

/// Stop the server when the application exits.
///
/// Quitting Claudepot takes the remote surface down with it — that is
/// the trade in-process hosting makes, and it is enforced here rather
/// than left to the OS reaping the process, because CC's
/// `PermissionRequest` hook is an entry in the user's `settings.json`
/// that outlives us if nobody removes it. The heartbeat makes a
/// survivor harmless; leaving litter we could have removed is not a
/// plan.
pub fn stop_on_exit(state: &RemoteServerState) {
    stop_on_exit_in(state, &claudepot_core::remote::approval::store::dir());
}

/// [`stop_on_exit`] with the approvals directory injected, so the exit
/// path can be tested without touching the user's data dir.
fn stop_on_exit_in(state: &RemoteServerState, approvals_dir: &std::path::Path) {
    let inner = state.inner.clone();
    // A blocking wait on the async lock is wrong here — this runs on
    // the exit path where the runtime may already be winding down. Try
    // the lock, and fall back to the hook's own runtime gate: a missed
    // uninstall reads as "not serving" within `SERVING_FRESH` anyway.
    if let Ok(mut guard) = inner.try_lock() {
        if let Some(mut running) = guard.running.take() {
            if let Some(tx) = running.stop.take() {
                let _ = tx.send(());
            }
            running.task.abort();
        }
    }
    // Belt and braces: clear the heartbeat directly, so a phone that
    // polls during shutdown sees the surface go away rather than
    // waiting out the freshness window.
    claudepot_core::remote::approval::store::stop_serving(approvals_dir);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// A hook guard that records being dropped.
    struct DropFlag(Arc<AtomicBool>);
    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    async fn run(
        state: &RemoteServerState,
        serve_loop: impl Future<Output = Result<(), String>> + Send + 'static,
        warnings: Vec<String>,
    ) -> Arc<AtomicBool> {
        let dropped = Arc::new(AtomicBool::new(false));
        let flag = dropped.clone();
        let mut inner = state.inner.lock().await;
        supervise(
            &mut inner,
            state.inner.clone(),
            "https://127.0.0.1:7443".into(),
            true,
            serve_loop,
            move || (Box::new(DropFlag(flag)) as HookGuard, warnings),
        );
        dropped
    }

    async fn settle() {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn a_running_server_is_described_and_a_second_start_returns_its_url() {
        let state = RemoteServerState::new();
        run(&state, std::future::pending(), Vec::new()).await;
        let d = state.describe().await;
        assert!(d.running_here);
        assert_eq!(d.url.as_deref(), Some("https://127.0.0.1:7443"));
        assert!(d.tls);
        // No config is read on this path: the running server wins.
        assert_eq!(state.start().await.unwrap(), "https://127.0.0.1:7443");
    }

    #[tokio::test]
    async fn a_serve_loop_that_dies_records_why_and_drops_the_hook() {
        let state = RemoteServerState::new();
        let dropped = run(
            &state,
            async { Err("handshake failed".to_string()) },
            Vec::new(),
        )
        .await;
        settle().await;
        let d = state.describe().await;
        assert!(!d.running_here);
        assert_eq!(d.last_error.as_deref(), Some("handshake failed"));
        assert!(dropped.load(Ordering::SeqCst), "the hook must come out");
    }

    #[tokio::test]
    async fn hook_warnings_reach_the_description() {
        let state = RemoteServerState::new();
        run(
            &state,
            std::future::pending(),
            vec!["hook not armed".into()],
        )
        .await;
        settle().await;
        assert_eq!(
            state.describe().await.warnings,
            vec!["hook not armed".to_string()]
        );
    }

    #[tokio::test]
    async fn stop_signals_the_loop_and_the_hook_comes_out() {
        let state = RemoteServerState::new();
        let dropped = run(&state, std::future::pending(), Vec::new()).await;
        assert!(state.stop().await);
        assert!(dropped.load(Ordering::SeqCst));
        let d = state.describe().await;
        assert!(!d.running_here);
        assert_eq!(d.last_error, None, "a requested stop is not an error");
        assert!(!state.stop().await, "nothing left to stop");
    }

    #[tokio::test]
    async fn exit_takes_the_server_down_and_clears_the_heartbeat() {
        use claudepot_core::remote::approval::store;
        let state = RemoteServerState::new();
        let dropped = run(&state, std::future::pending(), Vec::new()).await;
        let dir = tempfile::TempDir::new().unwrap();
        store::mark_serving(dir.path(), 1).unwrap();
        assert!(store::is_serving(dir.path(), 1));
        // Let the task start and arm its hook, as it has long before any
        // real quit.
        settle().await;

        stop_on_exit_in(&state, dir.path());

        assert!(!state.describe().await.running_here);
        assert!(!store::is_serving(dir.path(), 1), "the heartbeat must go");
        settle().await;
        assert!(
            dropped.load(Ordering::SeqCst),
            "the aborted task drops the hook"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_loop_that_ignores_the_signal_is_aborted_after_the_timeout() {
        let state = RemoteServerState::new();
        // Blocks its worker without yielding, so the stop signal cannot
        // be polled until it returns.
        run(
            &state,
            async {
                std::thread::sleep(Duration::from_millis(800));
                Ok(())
            },
            Vec::new(),
        )
        .await;
        settle().await;
        let start = std::time::Instant::now();
        assert!(state.stop_within(Duration::from_millis(100)).await);
        assert!(
            start.elapsed() < Duration::from_millis(700),
            "{:?}",
            start.elapsed()
        );
        assert!(!state.describe().await.running_here);
    }
}
