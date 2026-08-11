use crate::http_client::bounded_error_body;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::Value;
use std::borrow::Cow;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::RwLock;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use tokio::sync::Semaphore;
use uuid::Uuid;

const ACCESS_TOKEN_ENV: &str = "CODEX_ACCESS_TOKEN";
const ACCOUNT_ID_ENV: &str = "CHATGPT_ACCOUNT_ID";
const REFRESH_URL: &str = "https://auth.openai.com/oauth/token";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const REFRESH_WINDOW: Duration = Duration::from_secs(5 * 60);
const MAX_AUTH_FILE_BYTES: usize = 1024 * 1024;
const MAX_REFRESH_ERROR_BODY_BYTES: usize = 8_000;
const MAX_REFRESH_ERROR_BODY_CHARS: usize = 2_000;

#[derive(Clone)]
pub(crate) struct Auth {
    access_token: String,
    refresh_token: Option<String>,
    account_id: Option<String>,
    account: ChatGptAccount,
    expires_at: Option<u64>,
    refresh_url: Cow<'static, str>,
    storage: Option<StoredAuth>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ChatGptAccount {
    pub(crate) email: Option<String>,
    pub(crate) plan: Option<String>,
}

#[derive(Clone)]
pub(crate) struct SharedAuth {
    inner: Arc<SharedAuthInner>,
}

struct SharedAuthInner {
    auth: RwLock<Auth>,
    refresh_lock: Semaphore,
}

pub(crate) struct AuthSnapshot {
    pub(crate) authorization: reqwest::header::HeaderValue,
    pub(crate) account_id: Option<reqwest::header::HeaderValue>,
}

#[derive(Clone)]
struct StoredAuth {
    path: PathBuf,
    document: Value,
}

#[derive(Deserialize)]
struct RefreshResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    id_token: Option<String>,
}

impl Auth {
    #[cfg(test)]
    pub(crate) fn for_test(access_token: impl Into<String>) -> Self {
        Self {
            access_token: access_token.into(),
            refresh_token: None,
            account_id: Some("test-account".to_string()),
            account: ChatGptAccount::default(),
            expires_at: None,
            refresh_url: Cow::Borrowed(REFRESH_URL),
            storage: None,
        }
    }

    pub(crate) fn load() -> Result<Self> {
        if let Ok(access_token) = std::env::var(ACCESS_TOKEN_ENV)
            && !access_token.trim().is_empty()
        {
            let account_id = std::env::var(ACCOUNT_ID_ENV)
                .ok()
                .filter(|value| !value.trim().is_empty())
                .or_else(|| account_id_from_jwt(&access_token));
            let account = chatgpt_account_from_tokens(None, &access_token);
            return Ok(Self {
                expires_at: expiration_from_jwt(&access_token),
                access_token,
                refresh_token: None,
                account_id,
                account,
                refresh_url: Cow::Borrowed(REFRESH_URL),
                storage: None,
            });
        }

        Self::load_from_file(auth_file_path()?)
    }

    fn load_from_file(path: PathBuf) -> Result<Self> {
        let document = read_auth_document(&path).with_context(|| {
            format!(
                "failed to load ChatGPT credentials at {}; run `bcodex login`",
                path.display()
            )
        })?;
        let tokens = document
            .get("tokens")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow!("{} does not contain ChatGPT tokens", path.display()))?;
        let access_token = nonempty_string(tokens.get("access_token"))
            .ok_or_else(|| anyhow!("{} does not contain a ChatGPT access token", path.display()))?
            .to_string();
        let refresh_token = nonempty_string(tokens.get("refresh_token")).map(str::to_string);
        let id_token = nonempty_string(tokens.get("id_token"));
        let account_id = nonempty_string(tokens.get("account_id"))
            .map(str::to_string)
            .or_else(|| id_token.and_then(account_id_from_jwt))
            .or_else(|| account_id_from_jwt(&access_token));
        let account = chatgpt_account_from_tokens(id_token, &access_token);

        Ok(Self {
            expires_at: expiration_from_jwt(&access_token),
            access_token,
            refresh_token,
            account_id,
            account,
            refresh_url: Cow::Borrowed(REFRESH_URL),
            storage: Some(StoredAuth { path, document }),
        })
    }

    fn refresh_needed(&self) -> Result<bool> {
        let now = unix_timestamp()?;
        Ok(self
            .expires_at
            .is_some_and(|expires_at| expires_at <= now + REFRESH_WINDOW.as_secs()))
    }

    pub(crate) async fn refresh_if_needed(&mut self, client: &reqwest::Client) -> Result<()> {
        if self.refresh_needed()? {
            self.refresh(client).await?;
        }
        Ok(())
    }

    pub(crate) async fn force_refresh(&mut self, client: &reqwest::Client) -> Result<()> {
        self.refresh(client).await
    }

    async fn refresh(&mut self, client: &reqwest::Client) -> Result<()> {
        if self.reload_from_storage_if_changed()? {
            return Ok(());
        }
        let refresh_token = self.refresh_token.clone().ok_or_else(|| {
            anyhow!(
                "the ChatGPT access token cannot be refreshed; run `bcodex login` and try again"
            )
        })?;
        let response = client
            .post(self.refresh_url.as_ref())
            .json(&serde_json::json!({
                "client_id": CLIENT_ID,
                "grant_type": "refresh_token",
                "refresh_token": refresh_token,
            }))
            .send()
            .await
            .context("failed to refresh ChatGPT credentials")?;
        let status = response.status();
        if status != StatusCode::OK {
            let body = bounded_error_body(
                response,
                MAX_REFRESH_ERROR_BODY_BYTES,
                MAX_REFRESH_ERROR_BODY_CHARS,
            )
            .await;
            return Err(anyhow!(
                "ChatGPT credential refresh failed with {status}: {body}; run `bcodex login`"
            ));
        }
        let refreshed: RefreshResponse = response
            .json()
            .await
            .context("failed to decode refreshed ChatGPT credentials")?;
        let access_token = refreshed
            .access_token
            .filter(|token| !token.trim().is_empty())
            .ok_or_else(|| anyhow!("ChatGPT credential refresh returned no access token"))?;
        let refresh_token = refreshed
            .refresh_token
            .filter(|token| !token.trim().is_empty())
            .or_else(|| self.refresh_token.clone());

        self.expires_at = expiration_from_jwt(&access_token);
        self.account_id = refreshed
            .id_token
            .as_deref()
            .and_then(account_id_from_jwt)
            .or_else(|| account_id_from_jwt(&access_token))
            .or_else(|| self.account_id.clone());
        self.account = merge_chatgpt_account(
            chatgpt_account_from_tokens(refreshed.id_token.as_deref(), &access_token),
            &self.account,
        );
        self.access_token = access_token;
        self.refresh_token = refresh_token;
        self.persist(refreshed.id_token.as_deref())?;
        Ok(())
    }

    fn reload_from_storage_if_changed(&mut self) -> Result<bool> {
        let Some(path) = self.storage.as_ref().map(|storage| storage.path.clone()) else {
            return Ok(false);
        };
        let expected_account_id = self.account_id.as_deref().ok_or_else(|| {
            anyhow!(
                "cannot safely refresh ChatGPT credentials without an account ID; run `bcodex login`"
            )
        })?;
        let mut reloaded = Self::load_from_file(path)?;
        if reloaded.account_id.as_deref() != Some(expected_account_id) {
            return Err(anyhow!(
                "stored ChatGPT credentials changed accounts; restart bettercodex or run `bcodex login`"
            ));
        }
        let changed = self.storage.as_ref().map(|storage| &storage.document)
            != reloaded.storage.as_ref().map(|storage| &storage.document);
        reloaded.refresh_url = self.refresh_url.clone();
        *self = reloaded;
        Ok(changed)
    }

    fn persist(&mut self, id_token: Option<&str>) -> Result<()> {
        let Some(storage) = self.storage.as_mut() else {
            return Ok(());
        };
        let tokens = storage
            .document
            .get_mut("tokens")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| anyhow!("stored ChatGPT credentials lost their token object"))?;
        tokens.insert(
            "access_token".to_string(),
            Value::String(self.access_token.clone()),
        );
        if let Some(refresh_token) = &self.refresh_token {
            tokens.insert(
                "refresh_token".to_string(),
                Value::String(refresh_token.clone()),
            );
        }
        if let Some(id_token) = id_token {
            tokens.insert("id_token".to_string(), Value::String(id_token.to_string()));
        }
        if let Some(account_id) = &self.account_id {
            tokens.insert("account_id".to_string(), Value::String(account_id.clone()));
        }
        storage.document["last_refresh"] = Value::String(rfc3339_now()?);
        write_private_json(&storage.path, &storage.document)
    }

    fn snapshot(&self) -> Result<AuthSnapshot> {
        let authorization =
            reqwest::header::HeaderValue::from_str(&format!("Bearer {}", self.access_token))
                .context("ChatGPT access token is not a valid HTTP header")?;
        let account_id = self
            .account_id
            .as_deref()
            .map(reqwest::header::HeaderValue::from_str)
            .transpose()
            .context("ChatGPT account ID is not a valid HTTP header")?;
        Ok(AuthSnapshot {
            authorization,
            account_id,
        })
    }

    pub(crate) fn account(&self) -> ChatGptAccount {
        self.account.clone()
    }
}

fn read_auth_document(path: &Path) -> Result<Value> {
    let mut file = File::open(path)
        .with_context(|| format!("failed to read ChatGPT credentials at {}", path.display()))?;
    if file.metadata()?.len() > u64::try_from(MAX_AUTH_FILE_BYTES).unwrap_or(u64::MAX) {
        return Err(anyhow!(
            "ChatGPT credentials at {} exceed the {} MiB limit",
            path.display(),
            MAX_AUTH_FILE_BYTES / (1024 * 1024)
        ));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(u64::try_from(MAX_AUTH_FILE_BYTES.saturating_add(1)).unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read ChatGPT credentials at {}", path.display()))?;
    if bytes.len() > MAX_AUTH_FILE_BYTES {
        return Err(anyhow!(
            "ChatGPT credentials at {} exceed the {} MiB limit",
            path.display(),
            MAX_AUTH_FILE_BYTES / (1024 * 1024)
        ));
    }
    serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse ChatGPT credentials at {}", path.display()))
}

impl SharedAuth {
    pub(crate) fn new(auth: Auth) -> Self {
        Self {
            inner: Arc::new(SharedAuthInner {
                auth: RwLock::new(auth),
                refresh_lock: Semaphore::new(1),
            }),
        }
    }

    pub(crate) async fn refreshed_snapshot(
        &self,
        client: &reqwest::Client,
    ) -> Result<AuthSnapshot> {
        self.refresh_snapshot(client, false).await
    }

    pub(crate) async fn force_refreshed_snapshot(
        &self,
        client: &reqwest::Client,
    ) -> Result<AuthSnapshot> {
        self.refresh_snapshot(client, true).await
    }

    pub(crate) fn account(&self) -> Result<ChatGptAccount> {
        self.inner
            .auth
            .read()
            .map(|auth| auth.account())
            .map_err(|_| anyhow!("ChatGPT credential cache lock was poisoned"))
    }

    async fn refresh_snapshot(
        &self,
        client: &reqwest::Client,
        force: bool,
    ) -> Result<AuthSnapshot> {
        let _refresh_guard = self
            .inner
            .refresh_lock
            .acquire()
            .await
            .map_err(|_| anyhow!("ChatGPT credential refresh coordinator closed"))?;
        let mut auth = {
            let auth = self
                .inner
                .auth
                .read()
                .map_err(|_| anyhow!("ChatGPT credential cache lock was poisoned"))?;
            if !force && !auth.refresh_needed()? {
                return auth.snapshot();
            }
            auth.clone()
        };
        if force {
            auth.force_refresh(client).await?;
        } else {
            auth.refresh_if_needed(client).await?;
        }
        let snapshot = auth.snapshot();
        *self
            .inner
            .auth
            .write()
            .map_err(|_| anyhow!("ChatGPT credential cache lock was poisoned"))? = auth;
        snapshot
    }
}

pub(crate) fn auth_file_path() -> Result<PathBuf> {
    Ok(crate::paths::codex_home()
        .ok_or_else(|| anyhow!("cannot locate Codex credentials: no user home is available"))?
        .join("auth.json"))
}

pub(crate) fn save_login_tokens(
    api_key: Option<String>,
    id_token: String,
    access_token: String,
    refresh_token: String,
) -> Result<()> {
    let account_id = account_id_from_jwt(&id_token);
    let document = serde_json::json!({
        "auth_mode": "chatgpt",
        "OPENAI_API_KEY": api_key,
        "tokens": {
            "id_token": id_token,
            "access_token": access_token,
            "refresh_token": refresh_token,
            "account_id": account_id,
        },
        "last_refresh": rfc3339_now()?,
    });
    write_private_json(&auth_file_path()?, &document)
}

fn nonempty_string(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn jwt_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let decoded = URL_SAFE_NO_PAD.decode(payload).ok()?;
    serde_json::from_slice(&decoded).ok()
}

fn expiration_from_jwt(token: &str) -> Option<u64> {
    jwt_claims(token)?.get("exp")?.as_u64()
}

fn account_id_from_jwt(token: &str) -> Option<String> {
    let claims = jwt_claims(token)?;
    claims
        .pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id")
        .and_then(Value::as_str)
        .or_else(|| claims.get("chatgpt_account_id").and_then(Value::as_str))
        .map(str::to_string)
}

fn chatgpt_account_from_tokens(id_token: Option<&str>, access_token: &str) -> ChatGptAccount {
    let id_claims = id_token.and_then(jwt_claims);
    let access_claims = jwt_claims(access_token);
    let email = id_claims
        .as_ref()
        .and_then(account_email_from_claims)
        .or_else(|| access_claims.as_ref().and_then(account_email_from_claims));
    let plan = id_claims
        .as_ref()
        .and_then(account_plan_from_claims)
        .or_else(|| access_claims.as_ref().and_then(account_plan_from_claims));
    ChatGptAccount { email, plan }
}

fn account_email_from_claims(claims: &Value) -> Option<String> {
    claims
        .get("email")
        .and_then(Value::as_str)
        .or_else(|| {
            claims
                .get("https://api.openai.com/profile")
                .and_then(|profile| profile.get("email"))
                .and_then(Value::as_str)
        })
        .map(str::trim)
        .filter(|email| !email.is_empty())
        .map(str::to_string)
}

fn account_plan_from_claims(claims: &Value) -> Option<String> {
    let plan = claims
        .get("https://api.openai.com/auth")
        .and_then(|auth| auth.get("chatgpt_plan_type"))
        .and_then(Value::as_str)?;
    Some(
        match plan.to_ascii_lowercase().as_str() {
            "free" => "Free",
            "go" => "Go",
            "plus" => "Plus",
            "pro" => "Pro",
            "prolite" => "Pro Lite",
            "team" | "self_serve_business_prolite" | "self_serve_business_usage_based" => {
                "Business"
            }
            "enterprise_cbp_automation" => "Enterprise (Automation)",
            "business" | "ent26" | "enterprise_cbp_usage_based" | "enterprise" | "hc" => {
                "Enterprise"
            }
            "edu" | "education" => "Edu",
            _ => "Unknown",
        }
        .to_string(),
    )
}

fn merge_chatgpt_account(current: ChatGptAccount, previous: &ChatGptAccount) -> ChatGptAccount {
    ChatGptAccount {
        email: current.email.or_else(|| previous.email.clone()),
        plan: current.plan.or_else(|| previous.plan.clone()),
    }
}

fn unix_timestamp() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_secs())
}

fn rfc3339_now() -> Result<String> {
    let seconds = unix_timestamp()? as i64;
    let days = seconds.div_euclid(86_400);
    let seconds_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_date_from_unix_days(days);
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    Ok(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z"
    ))
}

fn civil_date_from_unix_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month, day)
}

fn write_private_json(path: &Path, document: &Value) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("credential path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create credential directory {}", parent.display()))?;
    let temp = parent.join(format!(
        ".auth.json.tmp-{}-{}",
        std::process::id(),
        Uuid::new_v4()
    ));
    let bytes = serde_json::to_vec_pretty(document)?;
    let result = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        crate::platform_fs::configure_private_file(&mut options);
        let mut file = options.open(&temp).with_context(|| {
            format!(
                "failed to open temporary credential file {}",
                temp.display()
            )
        })?;
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        crate::platform_fs::replace_file(&temp, path).with_context(|| {
            format!(
                "failed to replace credential file {} with {}",
                path.display(),
                temp.display()
            )
        })?;
        crate::platform_fs::sync_directory(parent)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
#[path = "auth_tests.rs"]
mod tests;
