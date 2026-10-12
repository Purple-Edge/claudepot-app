use crate::error::OAuthError;
use crate::oauth::{beta_header, http_client};
use serde::Deserialize;

const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const DEFAULT_SCOPES: &str =
    "user:file_upload user:inference user:mcp_servers user:profile user:sessions:claude_code";

#[derive(Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: u64,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub token_type: Option<String>,
    /// Seconds until the login itself ends. CC stores it as
    /// `refreshTokenExpiresAt`; see [`crate::blob::OAuthCredentials`].
    #[serde(default)]
    pub refresh_token_expires_in: Option<u64>,
}

/// Manual `Debug` so a `tracing::debug!(?resp)`, error-context
/// capture, or test-failure dump can never print the raw
/// `sk-ant-oat01-`/`sk-ant-ort01-` bodies. Same redaction shape as
/// `blob::OAuthCredentials` (`<redacted len=N>`), per
/// `.claude/rules/rust-conventions.md`.
impl std::fmt::Debug for TokenResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        struct Redacted(usize);
        impl std::fmt::Debug for Redacted {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "<redacted len={}>", self.0)
            }
        }
        f.debug_struct("TokenResponse")
            .field("access_token", &Redacted(self.access_token.len()))
            .field("refresh_token", &Redacted(self.refresh_token.len()))
            .field("expires_in", &self.expires_in)
            .field("scope", &self.scope)
            .field("token_type", &self.token_type)
            .field("refresh_token_expires_in", &self.refresh_token_expires_in)
            .finish()
    }
}

/// Exchange a refresh token for a new access token + rotated refresh token.
pub async fn refresh(refresh_token: &str) -> Result<TokenResponse, OAuthError> {
    let client = http_client()?;
    let resp = client
        .post(TOKEN_URL)
        .header("Content-Type", "application/json")
        .header("anthropic-beta", beta_header::get_or_default())
        .json(&serde_json::json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
            "client_id": CLIENT_ID,
            "scope": DEFAULT_SCOPES,
        }))
        .send()
        .await?;

    let status = resp.status();
    if status == 429 {
        // Now clamped to 300s like every other OAuth call site. This one
        // was uncapped, so a token endpoint returning a huge
        // `Retry-After` could dictate an unbounded wait — the exact DoS
        // the sibling call sites' comments warned about.
        return Err(OAuthError::RateLimited {
            retry_after_secs: crate::oauth::retry_after_secs(resp.headers()),
        });
    }
    if status == 400 || status == 401 {
        // 400/401 = the refresh_token itself was refused (revoked,
        // expired, or never valid). This is terminal — re-login needed.
        let _ = resp.text().await;
        return Err(OAuthError::RefreshFailed(format!(
            "token endpoint rejected refresh_token with {status}"
        )));
    }
    if !status.is_success() {
        // 5xx or unexpected non-2xx: transport / server-side trouble.
        // Not a credential problem — treat as transient so callers can
        // preserve verified_email history and retry later.
        let _ = resp.text().await;
        return Err(OAuthError::ServerError(format!(
            "token endpoint returned {status}"
        )));
    }

    let token_resp: TokenResponse = resp.json().await?;
    Ok(token_resp)
}

/// Apply a refresh to a stored blob: replace the token fields and keep
/// everything else — subscription metadata, the login's expiry, and every
/// key CC writes that this crate does not name (`mcpOAuth`).
///
/// The one place a token response becomes a blob. Building a fresh blob
/// instead is how a refresh used to delete `refreshTokenExpiresAt` and the
/// MCP logins from every account Claudepot refreshed.
pub fn apply_refresh(
    original: &crate::blob::CredentialBlob,
    resp: &TokenResponse,
) -> crate::blob::CredentialBlob {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let mut blob = original.clone();
    let oauth = &mut blob.claude_ai_oauth;
    oauth.access_token = resp.access_token.clone();
    oauth.refresh_token = resp.refresh_token.clone();
    oauth.expires_at = now_ms + (resp.expires_in as i64 * 1000);
    if let Some(scope) = resp.scope.as_deref() {
        oauth.scopes = scope
            .split(' ')
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
    }
    // A response that does not restate the login's expiry has not moved it.
    if let Some(secs) = resp.refresh_token_expires_in {
        oauth.refresh_token_expires_at = Some(now_ms + (secs as i64 * 1000));
    }
    blob
}

/// Build a credential blob JSON string from a token response. With an
/// original blob this is [`apply_refresh`]; without one (a first login)
/// it starts from an empty blob with default scopes.
pub fn build_blob(resp: &TokenResponse, original: Option<&crate::blob::CredentialBlob>) -> String {
    let base = match original {
        Some(blob) => blob.clone(),
        None => crate::blob::CredentialBlob {
            claude_ai_oauth: crate::blob::OAuthCredentials {
                access_token: String::new(),
                refresh_token: String::new(),
                expires_at: 0,
                scopes: DEFAULT_SCOPES.split(' ').map(String::from).collect(),
                subscription_type: Some(String::new()),
                rate_limit_tier: Some(String::new()),
                refresh_token_expires_at: None,
                extra: serde_json::Map::new(),
            },
            extra: serde_json::Map::new(),
        },
    };
    let blob = apply_refresh(&base, resp);
    // Serializing a struct of strings, integers and JSON values cannot fail.
    serde_json::to_string(&blob).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blob::CredentialBlob;

    fn make_token_response() -> TokenResponse {
        TokenResponse {
            access_token: "sk-ant-oat01-new".into(),
            refresh_token: "sk-ant-ort01-new".into(),
            expires_in: 3600,
            scope: Some("user:inference".into()),
            token_type: Some("Bearer".into()),
            refresh_token_expires_in: None,
        }
    }

    #[test]
    fn test_build_blob_preserves_subscription_metadata() {
        let original_json = crate::testing::sample_blob_json(0);
        let original = CredentialBlob::from_json(&original_json).unwrap();
        assert_eq!(
            original.claude_ai_oauth.subscription_type.as_deref(),
            Some("pro")
        );
        assert_eq!(
            original.claude_ai_oauth.rate_limit_tier.as_deref(),
            Some("default_claude_pro")
        );

        let resp = make_token_response();
        let result_json = build_blob(&resp, Some(&original));
        let result = CredentialBlob::from_json(&result_json).unwrap();

        assert_eq!(result.claude_ai_oauth.access_token, "sk-ant-oat01-new");
        assert_eq!(
            result.claude_ai_oauth.subscription_type.as_deref(),
            Some("pro")
        );
        assert_eq!(
            result.claude_ai_oauth.rate_limit_tier.as_deref(),
            Some("default_claude_pro")
        );
    }

    #[test]
    fn test_build_blob_without_original_uses_empty_defaults() {
        let resp = make_token_response();
        let result_json = build_blob(&resp, None);
        let result = CredentialBlob::from_json(&result_json).unwrap();

        assert_eq!(result.claude_ai_oauth.access_token, "sk-ant-oat01-new");
        assert_eq!(
            result.claude_ai_oauth.subscription_type.as_deref(),
            Some("")
        );
        assert_eq!(result.claude_ai_oauth.rate_limit_tier.as_deref(), Some(""));
    }

    /// Mirrors `blob.rs`'s `test_blob_debug_redacts_tokens` — the
    /// Debug impl is what `tracing` and panic messages interpolate.
    #[test]
    fn test_token_response_debug_redacts_tokens() {
        let resp = TokenResponse {
            access_token: "sk-ant-oat01-SECRETLEAKVALUE".into(),
            refresh_token: "sk-ant-ort01-OTHERLEAK".into(),
            expires_in: 3600,
            scope: Some("user:inference".into()),
            token_type: Some("Bearer".into()),
            refresh_token_expires_in: None,
        };
        let dbg = format!("{resp:?}");
        assert!(
            !dbg.contains("SECRETLEAK"),
            "Debug must not include the raw access token; got: {dbg}"
        );
        assert!(
            !dbg.contains("OTHERLEAK"),
            "Debug must not include the raw refresh token; got: {dbg}"
        );
        // Length is preserved for diagnosing truncated responses.
        assert!(dbg.contains("len=28"), "got: {dbg}");
        assert!(dbg.contains("len=22"), "got: {dbg}");
        // Non-secret fields stay visible.
        assert!(dbg.contains("expires_in"));
        // Pretty formatter routes through the same impl.
        let pretty = format!("{resp:#?}");
        assert!(!pretty.contains("SECRETLEAK"));
    }

    #[test]
    fn build_blob_records_the_login_expiry_the_server_returns() {
        let mut resp = make_token_response();
        resp.refresh_token_expires_in = Some(864_000);
        let result = CredentialBlob::from_json(&build_blob(&resp, None)).unwrap();

        let now_ms = chrono::Utc::now().timestamp_millis();
        let at = result.claude_ai_oauth.refresh_token_expires_at.unwrap();
        assert!(at > now_ms + 863_000_000 && at < now_ms + 865_000_000);
    }

    /// The login's expiry is fixed when the user signs in; a refresh that
    /// does not restate it has not changed it.
    #[test]
    fn build_blob_keeps_the_original_login_expiry_and_extras_when_not_restated() {
        let original = CredentialBlob::from_json(
            r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-old","refreshToken":"sk-ant-ort01-old","expiresAt":1,"refreshTokenExpiresAt":1792000000000,"scopes":["user:inference","user:plugins"],"subscriptionType":"max","rateLimitTier":"t"},"mcpOAuth":{"server|1":{"serverName":"s"}}}"#,
        )
        .unwrap();
        let mut resp = make_token_response();
        resp.scope = None;

        let out: serde_json::Value =
            serde_json::from_str(&build_blob(&resp, Some(&original))).unwrap();
        let oauth = &out["claudeAiOauth"];
        assert_eq!(oauth["accessToken"], "sk-ant-oat01-new");
        assert_eq!(oauth["refreshToken"], "sk-ant-ort01-new");
        assert_eq!(oauth["refreshTokenExpiresAt"], 1792000000000_i64);
        assert_eq!(
            oauth["scopes"],
            serde_json::json!(["user:inference", "user:plugins"])
        );
        assert_eq!(out["mcpOAuth"]["server|1"]["serverName"], "s");
    }

    #[test]
    fn test_build_blob_computes_future_expiry() {
        let resp = make_token_response();
        let result_json = build_blob(&resp, None);
        let result = CredentialBlob::from_json(&result_json).unwrap();

        let now_ms = chrono::Utc::now().timestamp_millis();
        assert!(result.claude_ai_oauth.expires_at > now_ms + 3_500_000);
        assert!(result.claude_ai_oauth.expires_at < now_ms + 3_700_000);
    }
}
