use serde::Deserialize;
use serde::Serialize;

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ServiceTier {
    #[default]
    Standard,
    Fast,
}

impl ServiceTier {
    pub(crate) const fn toggled(self) -> Self {
        match self {
            Self::Standard => Self::Fast,
            Self::Fast => Self::Standard,
        }
    }

    pub(crate) const fn request_value(self) -> Option<&'static str> {
        match self {
            Self::Standard => None,
            // Match Codex's canonical request value. The Responses API also accepts `fast`, but
            // reports the selected tier as `priority` in either case.
            Self::Fast => Some("priority"),
        }
    }

    pub(crate) const fn is_fast(self) -> bool {
        matches!(self, Self::Fast)
    }
}
