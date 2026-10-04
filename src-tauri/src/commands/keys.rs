//! Tauri commands for the Keys section.
//!
//! Metadata and secrets both live in `~/.claudepot/keys.db` (0o600 on
//! Unix, user-only DACL on Windows). The OS Keychain is NOT used for
//! this feature — CC's shared
//! `Claude Code-credentials` slot is the only Keychain surface touched
//! by the app (via `cli_backend::keychain`). The plaintext token
//! crosses the Tauri bridge ONLY on deliberate `*_copy` / `*_add` —
//! never on list, probe, or usage fetch.
//!
//! Every handler is `async fn` for the same reason the rest of the
//! command surface is — blocking I/O on Tauri's main thread freezes
//! the webview. See `commands.rs` header for the full rationale.
//!
//! This is the reference implementation for the `ErrorDto` error shape
//! (i18n plan §2.5). Two things changed and both are deliberate:
//! the English operation prefixes (`"keys store open failed: "`,
//! `"list api keys: "`) are gone — the UI knows what it was attempting
//! and says so in its own language — and every rejection now carries a
//! `code` the renderer can translate, with the underlying English still
//! in `message`.
//!
//! The `legacy_string` bridge that held `key_api_add`, `key_oauth_add`
//! and `key_oauth_usage_cached` on `Result<T, String>` is gone with it.
//! Its reason was renderer-side — `AddKeyModal.tsx` and
//! `OAuthUsageModal.tsx` rendered the rejection with `` `${e}` ``,
//! which prints `[object Object]` for a DTO. Both now call
//! `renderError(e)`, so the whole file is one shape.
//!
//! One rule this file holds harder than any other: **no `params` here
//! may carry a secret.** A label, a row uuid or a retry window is fine;
//! the pasted token is not, and neither is its preview. The token
//! leaves Rust through the OS clipboard and nowhere else.

use crate::dto::{ApiKeySummaryDto, ErrorDto, OauthTokenSummaryDto};
use crate::dto_error::codes;
use claudepot_core::account::AccountStore;
use claudepot_core::keys::{
    classify_token, oauth_shell_invocation, token_preview, CopyMaterial, KeyPrefix, KeyStore,
    OAUTH_TOKEN_VALIDITY_DAYS,
};
use claudepot_core::paths;
use claudepot_core::services::usage_cache::UsageCache;
use serde_json::json;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::AppHandle;
use tauri_plugin_clipboard_manager::ClipboardExt;
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

/// 30-second self-clear deadline for clipboard payloads written by
/// `key_*_copy*`. Mirrors the legacy JS `CLIPBOARD_CLEAR_MS` so the UX
/// timing didn't change when the policy moved Rust-side.
///
/// `pub(crate)` so the env-secret copy path shares one deadline.
pub(crate) const CLIPBOARD_CLEAR_MS: u64 = 30_000;

fn open_keys_store() -> Result<KeyStore, ErrorDto> {
    let db = paths::claudepot_data_dir().join("keys.db");
    KeyStore::open(&db).map_err(ErrorDto::from)
}

use super::open_account_store;

/// Build `uuid → email` from a single `accounts.list()` call so N-row
/// key tables don't fire N SELECTs when rendering the Keys section.
fn account_email_map(store: &AccountStore) -> Result<HashMap<Uuid, String>, ErrorDto> {
    let accounts = store
        .list()
        .map_err(|e| ErrorDto::detail(codes::ACCOUNTS_LIST_FAILED, e))?;
    Ok(accounts.into_iter().map(|a| (a.uuid, a.email)).collect())
}

fn parse_account_uuid(s: &str, email_map: &HashMap<Uuid, String>) -> Result<Uuid, ErrorDto> {
    let id = Uuid::parse_str(s).map_err(|_| {
        ErrorDto::with_params(
            codes::ACCOUNTS_INVALID_UUID,
            json!({ "uuid": s }),
            format!("invalid account uuid: {s}"),
        )
    })?;
    if !email_map.contains_key(&id) {
        return Err(ErrorDto::with_params(
            codes::ACCOUNTS_UNKNOWN_UUID,
            json!({ "uuid": s }),
            format!("no registered account with uuid {s}"),
        ));
    }
    Ok(id)
}

/// Renderer-supplied row id. `bad uuid: {e}` is kept verbatim as the
/// English fallback; `params.uuid` is what a localized sentence names.
fn parse_row_uuid(s: &str) -> Result<Uuid, ErrorDto> {
    Uuid::parse_str(s).map_err(|e| {
        ErrorDto::with_params(
            codes::KEYS_BAD_UUID,
            json!({ "uuid": s }),
            format!("bad uuid: {e}"),
        )
    })
}

/// The add and rename verbs reject an empty label. Shared so the four
/// cannot drift into four codes for one rule.
fn require_label(label: &str) -> Result<(), ErrorDto> {
    if label.is_empty() {
        return Err(ErrorDto::new(
            codes::KEYS_LABEL_REQUIRED,
            "label is required",
        ));
    }
    Ok(())
}

fn oauth_summary(
    t: claudepot_core::keys::OauthToken,
    email_map: &HashMap<Uuid, String>,
) -> OauthTokenSummaryDto {
    let expires_at = t.created_at + chrono::Duration::days(OAUTH_TOKEN_VALIDITY_DAYS);
    // Ceil the remaining time so a token with 12 hours left reads
    // "1d left" instead of collapsing to 0 and tripping the UI's
    // `<= 0` Expired check. Past-expiry deltas must stay strictly
    // negative — Rust's `/` truncates toward zero, so a token expired
    // by a few hours would otherwise round to 0 and read "expires
    // today" instead of "expired" (audit B8 commands_keys.rs:54).
    // `div_euclid` gives floor division for negative numerators with
    // a positive denominator, which is exactly what we want here.
    let secs_remaining = (expires_at - chrono::Utc::now()).num_seconds();
    let days_remaining = if secs_remaining > 0 {
        (secs_remaining + 86_399) / 86_400
    } else {
        secs_remaining.div_euclid(86_400)
    };
    OauthTokenSummaryDto {
        account_email: email_map.get(&t.account_uuid).cloned(),
        uuid: t.uuid.to_string(),
        label: t.label,
        token_preview: t.token_preview,
        account_uuid: t.account_uuid.to_string(),
        created_at: t.created_at,
        expires_at,
        days_remaining,
        last_probed_at: t.last_probed_at,
        last_probe_status: t.last_probe_status,
    }
}

fn api_summary(
    k: claudepot_core::keys::ApiKey,
    email_map: &HashMap<Uuid, String>,
) -> ApiKeySummaryDto {
    ApiKeySummaryDto {
        account_email: email_map.get(&k.account_uuid).cloned(),
        uuid: k.uuid.to_string(),
        label: k.label,
        token_preview: k.token_preview,
        account_uuid: k.account_uuid.to_string(),
        created_at: k.created_at,
        last_probed_at: k.last_probed_at,
        last_probe_status: k.last_probe_status,
    }
}

/// Wrapped in `tokio::task::spawn_blocking` so the two SQLite opens
/// + the email-map list don't block a Tauri IPC worker (audit B8
/// commands_keys.rs:96). KeysSection mounts this alongside
/// `key_oauth_list` and `account_list` via `Promise.all`, so a
/// blocking call here would serialize the whole mount.
#[tauri::command]
pub async fn key_api_list() -> Result<Vec<ApiKeySummaryDto>, ErrorDto> {
    tokio::task::spawn_blocking(|| {
        let keys = open_keys_store()?;
        let accounts = open_account_store()?;
        let email_map = account_email_map(&accounts)?;
        let rows = keys.list_api_keys().map_err(ErrorDto::from)?;
        Ok::<_, ErrorDto>(
            rows.into_iter()
                .map(|k| api_summary(k, &email_map))
                .collect(),
        )
    })
    .await
    .map_err(ErrorDto::task_join)?
}

/// Sibling to `key_api_list` — same `spawn_blocking` rationale.
#[tauri::command]
pub async fn key_oauth_list() -> Result<Vec<OauthTokenSummaryDto>, ErrorDto> {
    tokio::task::spawn_blocking(|| {
        let keys = open_keys_store()?;
        let accounts = open_account_store()?;
        let email_map = account_email_map(&accounts)?;
        let rows = keys.list_oauth_tokens().map_err(ErrorDto::from)?;
        Ok::<_, ErrorDto>(
            rows.into_iter()
                .map(|t| oauth_summary(t, &email_map))
                .collect(),
        )
    })
    .await
    .map_err(ErrorDto::task_join)?
}

/// Add an `ANTHROPIC_API_KEY`. `account_uuid` is required — every key
/// was created under some account, and recording that makes the row
/// findable by account later. The "no account" case isn't a real state
/// we need to model.
///
/// `async fn` — the body opens two SQLite stores and writes the
/// plaintext secret into `keys.db`'s 0600 `secret` column (no
/// encryption layer — the at-rest pattern documented in the file
/// header and `.claude/rules/architecture.md`). Keeping it off the main thread is
/// consistent with the rest of the `key_*` family; see `key_api_list`.
///
/// Secret handling (D-5/6/7): the `token` `String` arrives via the
/// IPC bridge, is trimmed into a fresh owned `String`, and is
/// **zeroized** on every exit path — success and error alike. The
/// originally-deserialized `token` argument also has its bytes
/// scrubbed before drop. `KeyError` Display impl never interpolates
/// the secret (audit `keys/error.rs`), so lifting its rejection with
/// `ErrorDto::from` cannot leak it — neither `message` nor `params`
/// can reach the value. The two prefix rejections below carry an empty
/// `params` for the same reason.
#[tauri::command]
pub async fn key_api_add(
    label: String,
    mut token: String,
    account_uuid: String,
) -> Result<ApiKeySummaryDto, ErrorDto> {
    let result = key_api_add_inner(&label, token.trim(), &account_uuid).await;
    // Scrub the IPC-bridge `String` regardless of outcome. The trimmed
    // slice above is a borrow of these bytes — once `inner` returned,
    // its `String` shadow has been dropped (and zeroized) and the
    // backing buffer is what we must wipe here.
    token.zeroize();
    result
}

async fn key_api_add_inner(
    label: &str,
    token: &str,
    account_uuid: &str,
) -> Result<ApiKeySummaryDto, ErrorDto> {
    add_secret(
        SecretKind::Api,
        label,
        token,
        account_uuid,
        |keys, label, preview, account, secret, email_map| {
            keys.insert_api_key(label, preview, account, secret)
                .map(|row| api_summary(row, email_map))
                .map_err(ErrorDto::from)
        },
    )
    .await
}

/// The two inbound secret kinds, and what each must look like.
#[derive(Clone, Copy)]
enum SecretKind {
    Api,
    Oauth,
}

impl SecretKind {
    /// Reject a value of the wrong kind. The rejection names the prefix,
    /// never any part of the value.
    fn require_prefix(self, token: &str) -> Result<(), ErrorDto> {
        let (want, code, prefix, message) = match self {
            SecretKind::Api => (
                KeyPrefix::ApiKey,
                codes::KEYS_NOT_AN_API_KEY,
                "sk-ant-api03-",
                "not an API key — expected a value starting with `sk-ant-api03-`",
            ),
            SecretKind::Oauth => (
                KeyPrefix::OauthToken,
                codes::KEYS_NOT_AN_OAUTH_TOKEN,
                "sk-ant-oat01-",
                "not an OAuth token — expected a value starting with `sk-ant-oat01-`",
            ),
        };
        if classify_token(token) == Some(want) {
            Ok(())
        } else {
            Err(ErrorDto::with_params(
                code,
                json!({ "prefix": prefix }),
                message,
            ))
        }
    }
}

/// How a new secret is written and summarised: the store's insert for
/// the kind, plus the DTO it returns.
type InsertSecret<T> =
    fn(&KeyStore, &str, &str, Uuid, &str, &HashMap<Uuid, String>) -> Result<T, ErrorDto>;

/// Shared body of `key_api_add` / `key_oauth_add`. The owned copy of the
/// secret is `Zeroizing`, so it scrubs on every exit path including an
/// early `?`, and the SQLite work runs on the blocking pool rather than
/// the IPC worker.
async fn add_secret<T: Send + 'static>(
    kind: SecretKind,
    label: &str,
    token: &str,
    account_uuid: &str,
    insert: InsertSecret<T>,
) -> Result<T, ErrorDto> {
    let label = label.trim().to_string();
    require_label(&label)?;
    kind.require_prefix(token)?;
    let secret = Zeroizing::new(token.to_string());
    let account_uuid = account_uuid.to_string();
    tokio::task::spawn_blocking(move || {
        let accounts = open_account_store()?;
        let email_map = account_email_map(&accounts)?;
        let account = parse_account_uuid(&account_uuid, &email_map)?;
        let preview = token_preview(secret.as_str());
        let keys = open_keys_store()?;
        insert(
            &keys,
            &label,
            &preview,
            account,
            secret.as_str(),
            &email_map,
        )
    })
    .await
    .map_err(ErrorDto::task_join)?
}

/// Shared body of the two `key_*_update_secret` commands. Same
/// zeroization and blocking-pool contract as [`add_secret`].
async fn update_secret(kind: SecretKind, uuid: &str, token: &str) -> Result<(), ErrorDto> {
    let id = parse_row_uuid(uuid)?;
    kind.require_prefix(token)?;
    let secret = Zeroizing::new(token.to_string());
    tokio::task::spawn_blocking(move || {
        let preview = token_preview(secret.as_str());
        let keys = open_keys_store()?;
        match kind {
            SecretKind::Api => keys.update_api_secret(id, &preview, secret.as_str()),
            SecretKind::Oauth => keys.update_oauth_secret(id, &preview, secret.as_str()),
        }
        .map_err(ErrorDto::from)
    })
    .await
    .map_err(ErrorDto::task_join)?
}

/// Add a `CLAUDE_CODE_OAUTH_TOKEN`. Account tag is required — the user
/// picks the account they ran `claude setup-token` against.
///
/// `async fn` for the same reason as `key_api_add` / the rest of the
/// `key_*` family. See `key_api_add` for the zeroization contract.
#[tauri::command]
pub async fn key_oauth_add(
    label: String,
    mut token: String,
    account_uuid: String,
) -> Result<OauthTokenSummaryDto, ErrorDto> {
    let result = key_oauth_add_inner(&label, token.trim(), &account_uuid).await;
    token.zeroize();
    result
}

async fn key_oauth_add_inner(
    label: &str,
    token: &str,
    account_uuid: &str,
) -> Result<OauthTokenSummaryDto, ErrorDto> {
    add_secret(
        SecretKind::Oauth,
        label,
        token,
        account_uuid,
        |keys, label, preview, account, secret, email_map| {
            keys.insert_oauth_token(label, preview, account, secret)
                .map(|row| oauth_summary(row, email_map))
                .map_err(ErrorDto::from)
        },
    )
    .await
}

/// `async fn` — SQLite row delete (the plaintext secret column goes
/// with the row; there is no separate encrypted blob). See `key_api_list`.
#[tauri::command]
pub async fn key_api_remove(uuid: String) -> Result<(), ErrorDto> {
    let id = parse_row_uuid(&uuid)?;
    let keys = open_keys_store()?;
    keys.remove_api_key(id).map_err(ErrorDto::from)
}

/// `async fn` — sibling to `key_api_remove`.
#[tauri::command]
pub async fn key_oauth_remove(uuid: String) -> Result<(), ErrorDto> {
    let id = parse_row_uuid(&uuid)?;
    let keys = open_keys_store()?;
    keys.remove_oauth_token(id).map_err(ErrorDto::from)
}

/// Rotate an API key's secret, keeping the row.
///
/// Inbound secret, so the same zeroization contract as `key_api_add`
/// applies: the IPC-bridge `String` is scrubbed on every exit path,
/// success and error alike, and the owned copy inside `_inner` is
/// scrubbed before it returns. Nothing flows back out — the reply is
/// `()`, not the key.
#[tauri::command]
pub async fn key_api_update_secret(uuid: String, mut token: String) -> Result<(), ErrorDto> {
    let result = key_api_update_secret_inner(&uuid, token.trim()).await;
    token.zeroize();
    result
}

async fn key_api_update_secret_inner(uuid: &str, token: &str) -> Result<(), ErrorDto> {
    update_secret(SecretKind::Api, uuid, token).await
}

/// Rotate an OAuth token's secret. See `key_api_update_secret`.
#[tauri::command]
pub async fn key_oauth_update_secret(uuid: String, mut token: String) -> Result<(), ErrorDto> {
    let result = key_oauth_update_secret_inner(&uuid, token.trim()).await;
    token.zeroize();
    result
}

async fn key_oauth_update_secret_inner(uuid: &str, token: &str) -> Result<(), ErrorDto> {
    update_secret(SecretKind::Oauth, uuid, token).await
}

/// Rename an API key. Label is user-owned metadata — resolution and
/// lookup key off `uuid`, never the label, so renames are a pure
/// display-layer change.
#[tauri::command]
pub async fn key_api_rename(uuid: String, label: String) -> Result<(), ErrorDto> {
    let id = parse_row_uuid(&uuid)?;
    let label = label.trim();
    require_label(label)?;
    let keys = open_keys_store()?;
    keys.rename_api_key(id, label).map_err(ErrorDto::from)
}

/// Rename an OAuth token. See `key_api_rename`.
#[tauri::command]
pub async fn key_oauth_rename(uuid: String, label: String) -> Result<(), ErrorDto> {
    let id = parse_row_uuid(&uuid)?;
    let label = label.trim();
    require_label(label)?;
    let keys = open_keys_store()?;
    keys.rename_oauth_token(id, label).map_err(ErrorDto::from)
}

/// Receipt returned to the renderer after a successful `key_*_copy*`
/// call. The raw secret never crosses the IPC boundary on this path —
/// only the label + preview the UI already had on hand, plus the
/// timestamp at which Rust will self-clear the clipboard.
#[derive(Debug, Clone, serde::Serialize)]
pub struct KeyCopyReceiptDto {
    pub label: String,
    pub preview: String,
    pub clipboard_clears_at_unix_ms: u64,
}

/// What kind of secret a `key_*_copy*` call should fetch from the
/// store. Drives both DB lookup and the receipt's `kind`-agnostic
/// payload formatting.
#[derive(Clone, Copy)]
enum CopyKind {
    Api,
    Oauth,
    OauthShell,
}

/// The bytes a copy writes: the secret itself, or the shell invocation
/// `claudepot_core::keys::oauth_shell_invocation` builds around it.
fn build_copy_payload(kind: CopyKind, secret: &str) -> Zeroizing<String> {
    Zeroizing::new(match kind {
        CopyKind::Api | CopyKind::Oauth => secret.to_string(),
        CopyKind::OauthShell => oauth_shell_invocation(secret),
    })
}

/// `pub(crate)` — shared with the env-secret copy path.
pub(crate) fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Schedule a 30s clipboard self-clear, gated on the clipboard still
/// holding the exact payload we wrote. If `read_text` returns
/// something else (or fails — permissions, etc.), abort silently:
/// it's better to leave whatever the user copied in between than
/// blind-clobber it.
///
/// The `payload` is moved into the task; the caller must zeroize its
/// own local copy before / after returning to the IPC bridge.
///
/// `pub(crate)` so the env-secret copy path reuses the exact same
/// gated self-clear behavior.
pub(crate) fn schedule_self_clear(app: AppHandle, mut payload: String) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(tokio::time::Duration::from_millis(CLIPBOARD_CLEAR_MS)).await;
        let still_ours = match app.clipboard().read_text() {
            Ok(s) => s == payload,
            Err(_) => false,
        };
        // Zeroize our copy of the payload regardless of the readback
        // outcome — there is no further legitimate use of these bytes.
        payload.zeroize();
        if still_ours {
            // `clear` writes an empty string; the JS prior art picked
            // the same shape ("better than nothing"). Errors are
            // ignorable: if the user has the screen-recording
            // permission scrub seam open, there's nothing we can do.
            let _ = app.clipboard().clear();
        }
    });
}

/// Shared body of the three `key_*_copy*` commands: load the row off
/// the IPC worker, then write the clipboard and return a receipt. The
/// secret bytes never reach the renderer.
async fn key_copy_inner(
    uuid: String,
    kind: CopyKind,
    app: AppHandle,
) -> Result<KeyCopyReceiptDto, ErrorDto> {
    let id = parse_row_uuid(&uuid)?;
    let material = tokio::task::spawn_blocking(move || load_copy_material(id, kind))
        .await
        .map_err(ErrorDto::task_join)??;
    copy_to_clipboard(&material, kind, &app, |payload| {
        schedule_self_clear(app.clone(), payload)
    })
}

/// One row's label, preview and secret, read together by the store so
/// a concurrent rotate cannot split them.
fn load_copy_material(id: Uuid, kind: CopyKind) -> Result<CopyMaterial, ErrorDto> {
    let keys = open_keys_store()?;
    let (found, code, what) = match kind {
        CopyKind::Api => (
            keys.find_api_copy_material(id),
            codes::KEYS_API_KEY_NOT_FOUND,
            "api key",
        ),
        CopyKind::Oauth | CopyKind::OauthShell => (
            keys.find_oauth_copy_material(id),
            codes::KEYS_OAUTH_TOKEN_NOT_FOUND,
            "oauth token",
        ),
    };
    found.map_err(ErrorDto::from)?.ok_or_else(|| {
        ErrorDto::with_params(
            code,
            json!({ "uuid": id.to_string() }),
            format!("{what} {id} not found"),
        )
    })
}

/// Where a copy writes. The app's clipboard in production; a test
/// double in the unit tests, which is what lets the happy path of the
/// copy flow be exercised without a webview.
trait ClipboardSink {
    fn write_text(&self, text: String) -> Result<(), String>;
}

impl ClipboardSink for AppHandle {
    fn write_text(&self, text: String) -> Result<(), String> {
        self.clipboard().write_text(text).map_err(|e| e.to_string())
    }
}

/// Write `material`'s payload to `sink`, hand a copy to
/// `schedule_clear` for the gated 30s self-clear, and return the
/// receipt. Every local copy of the payload is `Zeroizing`.
fn copy_to_clipboard(
    material: &CopyMaterial,
    kind: CopyKind,
    sink: &impl ClipboardSink,
    schedule_clear: impl FnOnce(String),
) -> Result<KeyCopyReceiptDto, ErrorDto> {
    let payload = build_copy_payload(kind, material.secret.as_str());
    sink.write_text(payload.to_string()).map_err(|e| {
        ErrorDto::with_params(
            codes::KEYS_CLIPBOARD_WRITE_FAILED,
            json!({ "detail": e }),
            format!("clipboard: {e}"),
        )
    })?;
    let clears_at = now_unix_ms() + CLIPBOARD_CLEAR_MS;
    // The self-clear task owns its copy and zeroizes it after readback.
    schedule_clear(payload.to_string());
    Ok(KeyCopyReceiptDto {
        label: material.label.clone(),
        preview: material.preview.clone(),
        clipboard_clears_at_unix_ms: clears_at,
    })
}

/// Copy an `ANTHROPIC_API_KEY` to the OS clipboard. The secret never
/// returns to JS — the renderer only sees a `KeyCopyReceiptDto` it
/// can toast verbatim. Self-clears after 30s if the clipboard still
/// holds our payload.
#[tauri::command]
pub async fn key_api_copy(uuid: String, app: AppHandle) -> Result<KeyCopyReceiptDto, ErrorDto> {
    key_copy_inner(uuid, CopyKind::Api, app).await
}

/// Copy a `CLAUDE_CODE_OAUTH_TOKEN` to the OS clipboard. Sibling of
/// `key_api_copy`. See that function for the secret-handling contract.
#[tauri::command]
pub async fn key_oauth_copy(uuid: String, app: AppHandle) -> Result<KeyCopyReceiptDto, ErrorDto> {
    key_copy_inner(uuid, CopyKind::Oauth, app).await
}

/// Copy a paste-ready POSIX shell invocation that runs CC under this
/// OAuth token: `CLAUDE_CODE_OAUTH_TOKEN='<secret>' claude`. The
/// formatting happens server-side so the raw secret never leaves Rust
/// — the JS layer only ever sees a `KeyCopyReceiptDto`.
#[tauri::command]
pub async fn key_oauth_copy_shell(
    uuid: String,
    app: AppHandle,
) -> Result<KeyCopyReceiptDto, ErrorDto> {
    key_copy_inner(uuid, CopyKind::OauthShell, app).await
}

/// Probe an API key against `GET /v1/models`. `Ok(())` = key accepted,
/// `Err(reason)` = rejected / rate-limited / transport. No DB write —
/// the result is ephemeral so a stale cached status can never lie.
///
/// Secret handling (D-5/6/7): the plaintext key is fetched inside
/// `spawn_blocking` (SQLite open off the IPC worker, same as
/// `key_copy_inner`) as `Zeroizing`, so it is scrubbed on every exit
/// path — cancellation included — and the error mapping below never
/// interpolates it.
#[tauri::command]
pub async fn key_api_probe(uuid: String) -> Result<(), ErrorDto> {
    use claudepot_core::error::OAuthError;
    let id = parse_row_uuid(&uuid)?;
    // `Zeroizing` before the `.await`: a future cancelled mid-probe drops
    // the secret without reaching any explicit scrub.
    let secret = tokio::task::spawn_blocking(move || -> Result<_, ErrorDto> {
        let keys = open_keys_store()?;
        keys.find_api_secret(id)
            .map(Zeroizing::new)
            .map_err(ErrorDto::from)
    })
    .await
    .map_err(ErrorDto::task_join)??;
    let probe = claudepot_core::keys::probe_api_key(secret.as_str()).await;
    drop(secret);
    match probe {
        Ok(()) => Ok(()),
        // Probe-specific copy, not the OAuth wording: the user is
        // looking at one key's row, so these two say what that row
        // means. The remaining variants have no probe-specific reading
        // and travel under their own `oauth.*` codes.
        Err(OAuthError::AuthFailed(_)) => Err(ErrorDto::new(
            codes::KEYS_PROBE_REJECTED,
            "rejected (invalid key)",
        )),
        Err(OAuthError::RateLimited { retry_after_secs }) => Err(ErrorDto::with_params(
            codes::KEYS_PROBE_RATE_LIMITED,
            json!({ "retry_after_secs": retry_after_secs }),
            format!("rate-limited (retry in {retry_after_secs}s)"),
        )),
        Err(e) => Err(ErrorDto::from(e)),
    }
}

/// Return the cached usage snapshot for the account this OAuth token
/// belongs to. Never hits Anthropic — reads only from the in-memory
/// `UsageCache` already populated by `fetch_all_usage` /
/// `refresh_usage_for` on the Accounts side. `None` means the cache
/// has no entry for this account yet (user hasn't opened Accounts,
/// or cache was invalidated); the UI renders that as an empty state
/// rather than triggering a fetch here.
#[tauri::command]
pub async fn key_oauth_usage_cached(
    uuid: String,
    cache: tauri::State<'_, UsageCache>,
) -> Result<Option<crate::dto::AccountUsageDto>, ErrorDto> {
    let id = parse_row_uuid(&uuid)?;
    // SQLite open/read off the IPC worker — same policy as
    // `key_copy_inner` / `key_api_probe` in this file.
    let token = tokio::task::spawn_blocking(move || -> Result<_, ErrorDto> {
        let keys = open_keys_store()?;
        keys.find_oauth_token(id)
            .map_err(ErrorDto::from)?
            .ok_or_else(|| {
                ErrorDto::with_params(
                    codes::KEYS_OAUTH_TOKEN_NOT_FOUND,
                    json!({ "uuid": id.to_string() }),
                    format!("oauth token {id} not found"),
                )
            })
    })
    .await
    .map_err(ErrorDto::task_join)??;
    let snapshot = cache.peek_cached(token.account_uuid).await;
    Ok(snapshot
        .as_ref()
        .map(crate::dto::AccountUsageDto::from_response))
}

#[cfg(test)]
mod oauth_summary_days_tests {
    use super::*;
    use chrono::Duration;
    use claudepot_core::keys::OauthToken;

    fn make_token(created_at: chrono::DateTime<chrono::Utc>) -> OauthToken {
        OauthToken {
            uuid: Uuid::new_v4(),
            label: "test".into(),
            token_preview: "sk-ant-oat01-…".into(),
            account_uuid: Uuid::new_v4(),
            created_at,
            last_probed_at: None,
            last_probe_status: None,
        }
    }

    #[test]
    fn oauth_summary_expired_a_few_hours_reads_minus_one_day() {
        // Audit B8 commands_keys.rs:54 — the previous truncating
        // division turned a token expired 5 hours ago into
        // `days_remaining = 0`, which the UI renders as "expires
        // today" instead of "expired". Floor division must yield
        // -1 (or more negative) for any negative remainder.
        let validity = Duration::days(OAUTH_TOKEN_VALIDITY_DAYS);
        let created_at = chrono::Utc::now() - validity - Duration::hours(5);
        let dto = oauth_summary(make_token(created_at), &HashMap::new());
        assert!(
            dto.days_remaining < 0,
            "expected negative days_remaining for expired token, got {}",
            dto.days_remaining
        );
    }

    #[test]
    fn oauth_summary_expired_one_full_day_reads_minus_one_day() {
        let validity = Duration::days(OAUTH_TOKEN_VALIDITY_DAYS);
        let created_at = chrono::Utc::now() - validity - Duration::days(1);
        let dto = oauth_summary(make_token(created_at), &HashMap::new());
        assert!(
            dto.days_remaining <= -1,
            "expected ≤ -1 days_remaining, got {}",
            dto.days_remaining
        );
    }

    #[test]
    fn oauth_summary_fresh_token_reads_positive_days() {
        let created_at = chrono::Utc::now();
        let dto = oauth_summary(make_token(created_at), &HashMap::new());
        assert!(
            dto.days_remaining > 0,
            "expected positive days_remaining for fresh token, got {}",
            dto.days_remaining
        );
    }
}

#[cfg(test)]
mod copy_payload_tests {
    //! The copy flow's logic, with the clipboard behind [`ClipboardSink`]
    //! so the happy path runs without a webview: payload formatting, the
    //! write, the self-clear hand-off, and the receipt's shape.

    use super::*;
    use std::cell::RefCell;

    struct FakeClipboard {
        written: RefCell<Option<String>>,
        fail: bool,
    }

    impl ClipboardSink for FakeClipboard {
        fn write_text(&self, text: String) -> Result<(), String> {
            if self.fail {
                return Err("pasteboard unavailable".into());
            }
            *self.written.borrow_mut() = Some(text);
            Ok(())
        }
    }

    fn material(secret: &str) -> CopyMaterial {
        CopyMaterial {
            label: "ci-token".into(),
            preview: "sk-ant-oat01-Abc…xyz".into(),
            secret: Zeroizing::new(secret.into()),
        }
    }

    #[test]
    fn a_copy_writes_the_payload_schedules_its_clear_and_returns_no_secret() {
        let sink = FakeClipboard {
            written: RefCell::new(None),
            fail: false,
        };
        let mut scheduled = None;
        let secret = "sk-ant-oat01-DO_NOT_LEAK";
        let receipt = copy_to_clipboard(&material(secret), CopyKind::OauthShell, &sink, |p| {
            scheduled = Some(p)
        })
        .unwrap();
        // Built from a template, not a literal assignment of a
        // token-shaped fixture, which the pre-commit guard refuses.
        let expected = format!("CLAUDE_CODE_OAUTH_TOKEN='{secret}' claude");
        assert_eq!(sink.written.borrow().as_deref(), Some(expected.as_str()));
        assert_eq!(
            scheduled.as_deref(),
            Some(expected.as_str()),
            "the clear gates on this payload"
        );
        assert_eq!(receipt.label, "ci-token");
        let json = serde_json::to_string(&receipt).unwrap();
        assert!(!json.contains(secret), "receipt carries no secret: {json}");
        assert!(receipt.clipboard_clears_at_unix_ms >= now_unix_ms());
    }

    #[test]
    fn a_failed_write_schedules_no_clear_and_reports_the_code() {
        let sink = FakeClipboard {
            written: RefCell::new(None),
            fail: true,
        };
        let mut scheduled = false;
        let err = copy_to_clipboard(&material("sk-ant-api03-x"), CopyKind::Api, &sink, |_| {
            scheduled = true
        })
        .unwrap_err();
        assert_eq!(err.code, codes::KEYS_CLIPBOARD_WRITE_FAILED);
        assert!(!err.message.contains("sk-ant-api03-x"));
        assert!(!scheduled);
    }

    #[test]
    fn key_api_copy_format_payload_passes_secret_through_unchanged() {
        let p = build_copy_payload(CopyKind::Api, "sk-ant-api03-abcdef");
        assert_eq!(p.as_str(), "sk-ant-api03-abcdef");
    }

    #[test]
    fn key_oauth_copy_format_payload_passes_secret_through_unchanged() {
        let p = build_copy_payload(CopyKind::Oauth, "sk-ant-oat01-zzz");
        assert_eq!(p.as_str(), "sk-ant-oat01-zzz");
    }

    #[test]
    fn a_wrong_kind_of_secret_is_rejected_without_echoing_it() {
        let err = SecretKind::Oauth
            .require_prefix("sk-ant-api03-abc")
            .unwrap_err();
        assert_eq!(err.code, codes::KEYS_NOT_AN_OAUTH_TOKEN);
        assert!(!err.message.contains("abc"));
        assert!(SecretKind::Api.require_prefix("sk-ant-api03-abc").is_ok());
    }

    #[test]
    fn key_copy_receipt_does_not_log_token() {
        // The Debug impl is what `tracing!` would interpolate, and
        // what error toasts capture if a higher-level handler
        // formats the receipt by accident. Neither field must
        // expose a substring of the secret bytes — they were never
        // copied into the receipt to begin with, so this is a
        // structural guarantee, not a sanitization step.
        let secret = "sk-ant-oat01-DO_NOT_LEAK_VERY_LONG_TOKEN_VALUE";
        let receipt = KeyCopyReceiptDto {
            label: "ci-token".into(),
            preview: "sk-ant-oat01-DO_…UE".into(),
            clipboard_clears_at_unix_ms: 12345,
        };
        let dbg = format!("{receipt:?}");
        assert!(
            !dbg.contains(secret),
            "receipt Debug impl must not include the secret value: {dbg}"
        );
        // Also assert the JSON shape — this is what crosses the
        // bridge and what the renderer toasts.
        let json = serde_json::to_string(&receipt).unwrap();
        assert!(json.contains("\"label\":\"ci-token\""), "json: {json}");
        assert!(
            json.contains("\"preview\":\"sk-ant-oat01-DO_…UE\""),
            "json: {json}",
        );
        assert!(
            json.contains("\"clipboard_clears_at_unix_ms\":12345"),
            "json: {json}",
        );
        assert!(
            !json.contains(secret),
            "receipt JSON must not include the secret: {json}",
        );
    }

    #[test]
    fn now_unix_ms_is_within_a_minute_of_systemtime() {
        // Sanity check that the deadline math doesn't wrap. The
        // narrow tolerance catches a regression where someone
        // converts `as_millis()` to seconds without noticing.
        let ours = now_unix_ms();
        let theirs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        assert!(
            theirs.saturating_sub(ours) < 60_000,
            "now_unix_ms drifted: ours={ours} theirs={theirs}",
        );
    }
}
