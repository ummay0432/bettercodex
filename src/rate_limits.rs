//! Codex account rate-limit snapshots carried by Responses streams and the
//! ChatGPT usage endpoint.
//!
//! Ported from OpenAI Codex `codex-rs/codex-api/src/rate_limits.rs` at
//! 279b93242cfef379e65da97e87e44b83c5934fd7, together with the retained
//! `backend-client` usage fetch used by Codex's `/status` prefetch.

use crate::auth::ChatGptAccount;
use crate::auth::SharedAuth;
use crate::http_client::bounded_error_body;
use anyhow::Context;
use anyhow::Result;
use reqwest::header::HeaderMap;
use reqwest::header::HeaderValue;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeSet;

const MAX_ERROR_BODY_BYTES: usize = 8_000;
const MAX_ERROR_BODY_CHARS: usize = 2_000;

#[derive(Clone)]
pub(crate) struct RateLimitClient {
    client: reqwest::Client,
    auth: SharedAuth,
    status_url: String,
}

impl RateLimitClient {
    pub(crate) fn new(client: reqwest::Client, auth: SharedAuth, base_url: &str) -> Self {
        Self {
            client,
            auth,
            status_url: rate_limit_status_url(base_url),
        }
    }

    pub(crate) fn account(&self) -> Result<ChatGptAccount> {
        self.auth.account()
    }

    pub(crate) async fn fetch(&self) -> Result<Vec<RateLimitSnapshot>> {
        let mut auth = self.auth.refreshed_snapshot(&self.client).await?;
        let mut refreshed_after_unauthorized = false;
        loop {
            let mut request = self
                .client
                .get(&self.status_url)
                .header("accept", HeaderValue::from_static("application/json"))
                .header("authorization", auth.authorization.clone());
            if let Some(account_id) = &auth.account_id {
                request = request.header("chatgpt-account-id", account_id.clone());
            }
            let response = request
                .send()
                .await
                .with_context(|| format!("GET {} failed", self.status_url))?;
            let status = response.status();
            if status.is_success() {
                let payload = response
                    .json::<RateLimitStatusPayload>()
                    .await
                    .with_context(|| {
                        format!("failed to decode rate limits from {}", self.status_url)
                    })?;
                return Ok(rate_limit_snapshots_from_payload(payload));
            }
            if status == reqwest::StatusCode::UNAUTHORIZED && !refreshed_after_unauthorized {
                auth = self.auth.force_refreshed_snapshot(&self.client).await?;
                refreshed_after_unauthorized = true;
                continue;
            }
            let body =
                bounded_error_body(response, MAX_ERROR_BODY_BYTES, MAX_ERROR_BODY_CHARS).await;
            anyhow::bail!("GET {} failed with {status}: {body}", self.status_url);
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RateLimitSnapshot {
    pub(crate) limit_id: String,
    pub(crate) limit_name: Option<String>,
    pub(crate) primary: Option<RateLimitWindow>,
    pub(crate) secondary: Option<RateLimitWindow>,
    pub(crate) credits: Option<CreditsSnapshot>,
    pub(crate) captured_at: i64,
}

impl RateLimitSnapshot {
    fn has_data(&self) -> bool {
        self.primary.is_some() || self.secondary.is_some() || self.credits.is_some()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RateLimitWindow {
    pub(crate) used_percent: f64,
    pub(crate) window_minutes: Option<i64>,
    pub(crate) resets_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CreditsSnapshot {
    pub(crate) has_credits: bool,
    pub(crate) unlimited: bool,
    pub(crate) balance: Option<String>,
}

#[derive(Deserialize)]
struct RateLimitStatusPayload {
    rate_limit: Option<RateLimitStatusDetails>,
    credits: Option<CreditStatusDetails>,
    additional_rate_limits: Option<Vec<AdditionalRateLimitDetails>>,
}

#[derive(Deserialize)]
struct AdditionalRateLimitDetails {
    limit_name: String,
    metered_feature: String,
    rate_limit: Option<RateLimitStatusDetails>,
}

#[derive(Deserialize)]
struct RateLimitStatusDetails {
    primary_window: Option<RateLimitWindowSnapshot>,
    secondary_window: Option<RateLimitWindowSnapshot>,
}

#[derive(Deserialize)]
struct RateLimitWindowSnapshot {
    used_percent: f64,
    limit_window_seconds: i64,
    reset_at: i64,
}

#[derive(Deserialize)]
struct CreditStatusDetails {
    has_credits: bool,
    unlimited: bool,
    balance: Option<String>,
}

fn rate_limit_status_url(base_url: &str) -> String {
    let base_url = base_url.trim_end_matches('/');
    if let Some((origin, _)) = base_url.split_once("/backend-api") {
        format!("{origin}/backend-api/wham/usage")
    } else {
        format!("{base_url}/api/codex/usage")
    }
}

fn rate_limit_snapshots_from_payload(payload: RateLimitStatusPayload) -> Vec<RateLimitSnapshot> {
    let captured_at = chrono::Utc::now().timestamp();
    let mut snapshots = vec![rate_limit_snapshot(
        "codex".to_string(),
        None,
        payload.rate_limit,
        payload.credits.map(|credits| CreditsSnapshot {
            has_credits: credits.has_credits,
            unlimited: credits.unlimited,
            balance: credits.balance,
        }),
        captured_at,
    )];
    snapshots.extend(
        payload
            .additional_rate_limits
            .unwrap_or_default()
            .into_iter()
            .map(|additional| {
                rate_limit_snapshot(
                    normalize_limit_id(&additional.metered_feature),
                    Some(additional.limit_name),
                    additional.rate_limit,
                    None,
                    captured_at,
                )
            }),
    );
    snapshots
}

fn rate_limit_snapshot(
    limit_id: String,
    limit_name: Option<String>,
    details: Option<RateLimitStatusDetails>,
    credits: Option<CreditsSnapshot>,
    captured_at: i64,
) -> RateLimitSnapshot {
    let (primary, secondary) = details.map_or((None, None), |details| {
        (
            details.primary_window.map(map_status_window),
            details.secondary_window.map(map_status_window),
        )
    });
    RateLimitSnapshot {
        limit_id,
        limit_name,
        primary,
        secondary,
        credits,
        captured_at,
    }
}

fn map_status_window(window: RateLimitWindowSnapshot) -> RateLimitWindow {
    RateLimitWindow {
        used_percent: window.used_percent,
        window_minutes: (window.limit_window_seconds > 0)
            .then(|| window.limit_window_seconds.saturating_add(59) / 60),
        resets_at: Some(window.reset_at),
    }
}

/// Parses every rate-limit header family present on one Responses connection.
pub(crate) fn parse_all_rate_limits(headers: &HeaderMap) -> Vec<RateLimitSnapshot> {
    let captured_at = chrono::Utc::now().timestamp();
    let mut snapshots = Vec::new();
    if let Some(snapshot) = parse_rate_limit_for_limit(headers, None, captured_at)
        && snapshot.has_data()
    {
        snapshots.push(snapshot);
    }

    let mut limit_ids = BTreeSet::new();
    for name in headers.keys() {
        let header_name = name.as_str().to_ascii_lowercase();
        if let Some(limit_id) = header_name_to_limit_id(&header_name)
            && limit_id != "codex"
        {
            limit_ids.insert(limit_id);
        }
    }
    snapshots.extend(limit_ids.into_iter().filter_map(|limit_id| {
        let snapshot = parse_rate_limit_for_limit(headers, Some(&limit_id), captured_at)?;
        snapshot.has_data().then_some(snapshot)
    }));
    snapshots
}

fn parse_rate_limit_for_limit(
    headers: &HeaderMap,
    limit_id: Option<&str>,
    captured_at: i64,
) -> Option<RateLimitSnapshot> {
    let normalized_limit = limit_id
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("codex")
        .to_ascii_lowercase()
        .replace('_', "-");
    let prefix = format!("x-{normalized_limit}");
    let primary = parse_rate_limit_window(
        headers,
        &format!("{prefix}-primary-used-percent"),
        &format!("{prefix}-primary-window-minutes"),
        &format!("{prefix}-primary-reset-at"),
    );
    let secondary = parse_rate_limit_window(
        headers,
        &format!("{prefix}-secondary-used-percent"),
        &format!("{prefix}-secondary-window-minutes"),
        &format!("{prefix}-secondary-reset-at"),
    );
    let limit_name = parse_header_str(headers, &format!("{prefix}-limit-name"))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string);

    Some(RateLimitSnapshot {
        limit_id: normalize_limit_id(&normalized_limit),
        limit_name,
        primary,
        secondary,
        credits: parse_credits_snapshot(headers),
        captured_at,
    })
}

#[derive(Deserialize)]
struct RateLimitEventWindow {
    used_percent: f64,
    window_minutes: Option<i64>,
    reset_at: Option<i64>,
}

#[derive(Deserialize)]
struct RateLimitEventDetails {
    primary: Option<RateLimitEventWindow>,
    secondary: Option<RateLimitEventWindow>,
}

#[derive(Deserialize)]
struct RateLimitEventCredits {
    has_credits: bool,
    unlimited: bool,
    balance: Option<String>,
}

#[derive(Deserialize)]
struct RateLimitEvent {
    #[serde(rename = "type")]
    kind: String,
    rate_limits: Option<RateLimitEventDetails>,
    credits: Option<RateLimitEventCredits>,
    metered_limit_name: Option<String>,
    limit_name: Option<String>,
}

/// Parses a `codex.rate_limits` stream event emitted after the initial headers.
pub(crate) fn parse_rate_limit_event(event: &Value) -> Option<RateLimitSnapshot> {
    let event: RateLimitEvent = serde_json::from_value(event.clone()).ok()?;
    if event.kind != "codex.rate_limits" {
        return None;
    }
    let (primary, secondary) = event.rate_limits.as_ref().map_or((None, None), |limits| {
        (
            limits.primary.as_ref().map(map_event_window),
            limits.secondary.as_ref().map(map_event_window),
        )
    });
    let credits = event.credits.map(|credits| CreditsSnapshot {
        has_credits: credits.has_credits,
        unlimited: credits.unlimited,
        balance: credits.balance,
    });
    let limit_id = event
        .metered_limit_name
        .or(event.limit_name)
        .map(|name| normalize_limit_id(&name))
        .unwrap_or_else(|| "codex".to_string());
    let snapshot = RateLimitSnapshot {
        limit_id,
        limit_name: None,
        primary,
        secondary,
        credits,
        captured_at: chrono::Utc::now().timestamp(),
    };
    snapshot.has_data().then_some(snapshot)
}

fn map_event_window(window: &RateLimitEventWindow) -> RateLimitWindow {
    RateLimitWindow {
        used_percent: window.used_percent,
        window_minutes: window.window_minutes,
        resets_at: window.reset_at,
    }
}

fn parse_rate_limit_window(
    headers: &HeaderMap,
    used_percent_header: &str,
    window_minutes_header: &str,
    resets_at_header: &str,
) -> Option<RateLimitWindow> {
    let used_percent = parse_header_str(headers, used_percent_header)?
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())?;
    let window_minutes = parse_header_str(headers, window_minutes_header)
        .and_then(|value| value.parse::<i64>().ok());
    let resets_at =
        parse_header_str(headers, resets_at_header).and_then(|value| value.parse::<i64>().ok());
    (used_percent != 0.0
        || window_minutes.is_some_and(|minutes| minutes != 0)
        || resets_at.is_some())
    .then_some(RateLimitWindow {
        used_percent,
        window_minutes,
        resets_at,
    })
}

fn parse_credits_snapshot(headers: &HeaderMap) -> Option<CreditsSnapshot> {
    let has_credits = parse_header_bool(headers, "x-codex-credits-has-credits")?;
    let unlimited = parse_header_bool(headers, "x-codex-credits-unlimited")?;
    let balance = parse_header_str(headers, "x-codex-credits-balance")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    Some(CreditsSnapshot {
        has_credits,
        unlimited,
        balance,
    })
}

fn parse_header_bool(headers: &HeaderMap, name: &str) -> Option<bool> {
    let raw = parse_header_str(headers, name)?;
    if raw.eq_ignore_ascii_case("true") || raw == "1" {
        Some(true)
    } else if raw.eq_ignore_ascii_case("false") || raw == "0" {
        Some(false)
    } else {
        None
    }
}

fn parse_header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok()
}

fn header_name_to_limit_id(header_name: &str) -> Option<String> {
    let prefix = header_name.strip_suffix("-primary-used-percent")?;
    Some(normalize_limit_id(prefix.strip_prefix("x-")?))
}

fn normalize_limit_id(name: &str) -> String {
    name.trim().to_ascii_lowercase().replace('-', "_")
}
