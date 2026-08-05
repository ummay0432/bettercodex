use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::Value;
use std::borrow::Cow;
use std::fs::OpenOptions;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use tokio::sync::Mutex;

const ACCESS_TOKEN_ENV: &str = "CODEX_ACCESS_TOKEN";
const ACCOUNT_ID_ENV: &str = "CHATGPT_ACCOUNT_ID";
const REFRESH_URL: &str = "https://auth.openai.com/oauth/token";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const REFRESH_WINDOW: Duration = Duration::from_secs(5 * 60);

pub(crate) struct Auth {
    access_token: String,
    refresh_token: Option<String>,
    account_id: Option<String>,
    expires_at: Option<u64>,
    refresh_url: Cow<'static, str>,
    storage: Option<StoredAuth>,
}

#[derive(Clone)]
pub(crate) struct SharedAuth {
    inner: Arc<Mutex<Auth>>,
}

pub(crate) struct AuthSnapshot {
    pub(crate) authorization: reqwest::header::HeaderValue,
    pub(crate) account_id: Option<reqwest::header::HeaderValue>,
}

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
            expires_at: None,
            refresh_url: Cow::Borrowed(REFRESH_URL),
            storage: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn refreshable_for_test(
        access_token: impl Into<String>,
        refresh_token: impl Into<String>,
        refresh_url: impl Into<String>,
    ) -> Self {
        Self {
            access_token: access_token.into(),
            refresh_token: Some(refresh_token.into()),
            account_id: Some("test-account".to_string()),
            expires_at: None,
            refresh_url: Cow::Owned(refresh_url.into()),
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
            return Ok(Self {
                expires_at: expiration_from_jwt(&access_token),
                access_token,
                refresh_token: None,
                account_id,
                refresh_url: Cow::Borrowed(REFRESH_URL),
                storage: None,
            });
        }

        let path = auth_file_path()?;
        let contents = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read ChatGPT credentials at {}", path.display()))?;
        let document: Value = serde_json::from_str(&contents).with_context(|| {
            format!("failed to parse ChatGPT credentials at {}", path.display())
        })?;
        let tokens = document
            .get("tokens")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow!("{} does not contain ChatGPT tokens", path.display()))?;
        let access_token = nonempty_string(tokens.get("access_token"))
            .ok_or_else(|| anyhow!("{} does not contain a ChatGPT access token", path.display()))?
            .to_string();
        let refresh_token = nonempty_string(tokens.get("refresh_token")).map(str::to_string);
        let account_id = nonempty_string(tokens.get("account_id"))
            .map(str::to_string)
            .or_else(|| nonempty_string(tokens.get("id_token")).and_then(account_id_from_jwt))
            .or_else(|| account_id_from_jwt(&access_token));

        Ok(Self {
            expires_at: expiration_from_jwt(&access_token),
            access_token,
            refresh_token,
            account_id,
            refresh_url: Cow::Borrowed(REFRESH_URL),
            storage: Some(StoredAuth { path, document }),
        })
    }

    pub(crate) async fn refresh_if_needed(&mut self, client: &reqwest::Client) -> Result<()> {
        let now = unix_timestamp()?;
        if self
            .expires_at
            .is_some_and(|expires_at| expires_at <= now + REFRESH_WINDOW.as_secs())
        {
            self.refresh(client).await?;
        }
        Ok(())
    }

    pub(crate) async fn force_refresh(&mut self, client: &reqwest::Client) -> Result<()> {
        self.refresh(client).await
    }

    async fn refresh(&mut self, client: &reqwest::Client) -> Result<()> {
        let refresh_token = self.refresh_token.clone().ok_or_else(|| {
            anyhow!("the ChatGPT access token cannot be refreshed; run `codex login` and try again")
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
            let body = bounded_error_body(response).await;
            return Err(anyhow!(
                "ChatGPT credential refresh failed with {status}: {body}; run `codex login`"
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
        self.access_token = access_token;
        self.refresh_token = refresh_token;
        self.persist(refreshed.id_token.as_deref())?;
        Ok(())
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
}

impl SharedAuth {
    pub(crate) fn new(auth: Auth) -> Self {
        Self {
            inner: Arc::new(Mutex::new(auth)),
        }
    }

    pub(crate) async fn refreshed_snapshot(
        &self,
        client: &reqwest::Client,
    ) -> Result<AuthSnapshot> {
        let mut auth = self.inner.lock().await;
        auth.refresh_if_needed(client).await?;
        auth.snapshot()
    }

    pub(crate) async fn force_refreshed_snapshot(
        &self,
        client: &reqwest::Client,
    ) -> Result<AuthSnapshot> {
        let mut auth = self.inner.lock().await;
        auth.force_refresh(client).await?;
        auth.snapshot()
    }
}

fn auth_file_path() -> Result<PathBuf> {
    let codex_home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
        .ok_or_else(|| anyhow!("cannot locate Codex credentials: HOME is not set"))?;
    Ok(codex_home.join("auth.json"))
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
    let temp = parent.join(format!(".auth.json.tmp-{}", std::process::id()));
    let bytes = serde_json::to_vec_pretty(document)?;
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&temp).with_context(|| {
        format!(
            "failed to open temporary credential file {}",
            temp.display()
        )
    })?;
    file.write_all(&bytes)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    std::fs::rename(&temp, path).with_context(|| {
        format!(
            "failed to replace credential file {} with {}",
            path.display(),
            temp.display()
        )
    })?;
    Ok(())
}

async fn bounded_error_body(response: reqwest::Response) -> String {
    response
        .text()
        .await
        .unwrap_or_else(|_| "unreadable response".to_string())
        .chars()
        .take(2_000)
        .collect()
}

#[cfg(test)]
#[path = "auth_tests.rs"]
mod tests;
