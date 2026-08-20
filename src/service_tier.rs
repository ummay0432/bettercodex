use anyhow::Result;
use anyhow::anyhow;
use serde::Deserialize;
use serde::Serialize;
use std::path::Path;

const SETTINGS_FILE: &str = "service-tier.json";
const SETTINGS_VERSION: u32 = 1;
const MAX_SETTINGS_BYTES: usize = 4 * 1024;

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

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    version: u32,
    service_tier: ServiceTier,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            service_tier: ServiceTier::default(),
        }
    }
}

pub(crate) fn load_default() -> Result<ServiceTier> {
    let Some(home) = crate::paths::bettercodex_home() else {
        return Ok(ServiceTier::default());
    };
    Ok(read_settings(&home.join(SETTINGS_FILE))?.service_tier)
}

pub(crate) fn save_default(service_tier: ServiceTier) -> Result<()> {
    let home = crate::paths::bettercodex_home()
        .ok_or_else(|| anyhow!("cannot save Fast mode because no bettercodex home is available"))?;
    save_default_to(&home.join(SETTINGS_FILE), service_tier)
}

fn save_default_to(path: &Path, service_tier: ServiceTier) -> Result<()> {
    crate::state_file::update_json(path, MAX_SETTINGS_BYTES, read_settings, |settings| {
        let changed = settings.service_tier != service_tier;
        if changed {
            settings.service_tier = service_tier;
        }
        Ok(crate::state_file::StateChange::from_changed(changed))
    })
}

fn read_settings(path: &Path) -> Result<Settings> {
    let settings: Settings =
        crate::state_file::read_json(path, MAX_SETTINGS_BYTES)?.unwrap_or_default();
    if settings.version != SETTINGS_VERSION {
        return Err(anyhow!(
            "unsupported service-tier settings version {}; expected {SETTINGS_VERSION}",
            settings.version
        ));
    }
    Ok(settings)
}

