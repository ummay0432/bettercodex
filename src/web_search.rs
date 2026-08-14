//! Hosted Responses web-search items and citation annotations.
//!
//! Search execution belongs to the Responses API. This module only projects the durable wire
//! items into the small typed forms used by events, saved transcripts, and terminal rendering.

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct WebSearchCall {
    pub(crate) id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) action: Option<WebSearchAction>,
}

impl WebSearchCall {
    pub(crate) fn from_response_item(item: &Value) -> Option<Self> {
        if item.get("type").and_then(Value::as_str) != Some("web_search_call") {
            return None;
        }
        let id = item
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let status = item
            .get("status")
            .and_then(Value::as_str)
            .map(str::to_string);
        let action = item
            .get("action")
            .and_then(|action| serde_json::from_value(action.clone()).ok());
        Some(Self { id, status, action })
    }

    pub(crate) fn detail(&self) -> String {
        self.action
            .as_ref()
            .map(WebSearchAction::detail)
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum WebSearchAction {
    Search {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        query: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        queries: Option<Vec<String>>,
    },
    OpenPage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
    FindInPage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pattern: Option<String>,
    },
    #[serde(other)]
    Other,
}

impl WebSearchAction {
    pub(crate) fn detail(&self) -> String {
        match self {
            Self::Search { query, queries } => query
                .as_deref()
                .filter(|query| !query.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    let first = queries
                        .as_ref()
                        .and_then(|queries| queries.first())
                        .cloned()
                        .unwrap_or_default();
                    if queries.as_ref().is_some_and(|queries| queries.len() > 1)
                        && !first.is_empty()
                    {
                        format!("{first} ...")
                    } else {
                        first
                    }
                }),
            Self::OpenPage { url } => url.clone().unwrap_or_default(),
            Self::FindInPage { url, pattern } => match (pattern, url) {
                (Some(pattern), Some(url)) => format!("'{pattern}' in {url}"),
                (Some(pattern), None) => format!("'{pattern}'"),
                (None, Some(url)) => url.clone(),
                (None, None) => String::new(),
            },
            Self::Other => String::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct UrlCitation {
    pub(crate) start_index: usize,
    pub(crate) end_index: usize,
    pub(crate) url: String,
    pub(crate) title: String,
}

impl UrlCitation {
    pub(crate) fn validated_url(&self) -> Option<String> {
        let url = self
            .url
            .chars()
            .filter(|ch| !ch.is_control())
            .collect::<String>();
        let parsed = url::Url::parse(&url).ok()?;
        if matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some() {
            Some(url)
        } else {
            None
        }
    }

    pub(crate) fn from_annotation(annotation: &Value, index_offset: usize) -> Option<Self> {
        if annotation.get("type").and_then(Value::as_str) != Some("url_citation") {
            return None;
        }
        // Responses currently emits flat fields. Accept the nested Chat-style representation as
        // well so saved sessions remain useful if the backend converges those wire forms.
        let citation = annotation.get("url_citation").unwrap_or(annotation);
        let start_index = citation
            .get("start_index")
            .and_then(Value::as_u64)
            .and_then(|index| usize::try_from(index).ok())?
            .saturating_add(index_offset);
        let end_index = citation
            .get("end_index")
            .and_then(Value::as_u64)
            .and_then(|index| usize::try_from(index).ok())?
            .saturating_add(index_offset);
        let url = citation.get("url").and_then(Value::as_str)?.to_string();
        if start_index > end_index || url.is_empty() {
            return None;
        }
        let title = citation
            .get("title")
            .and_then(Value::as_str)
            .filter(|title| !title.trim().is_empty())
            .unwrap_or(&url)
            .to_string();
        Some(Self {
            start_index,
            end_index,
            url,
            title,
        })
    }
}
