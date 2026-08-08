//! Focused HTTP behavior retained from OpenAI Codex commit
//! 1669c2403f793d0230065397dfc25f52b844244e.
//!
//! bettercodex needs Codex's retry curve, Cloudflare-only ChatGPT cookie jar,
//! and custom enterprise CA handling. The route-aware proxy, telemetry,
//! request abstraction, and provider systems are deliberately not retained.

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use rand::Rng;
use reqwest::cookie::CookieStore;
use reqwest::cookie::Jar;
use reqwest::header::HeaderValue;
use rustls::pki_types::pem;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::pem::SectionKind;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Once;
use std::time::Duration;

const CODEX_CA_CERT_ENV: &str = "CODEX_CA_CERTIFICATE";
const SSL_CERT_FILE_ENV: &str = "SSL_CERT_FILE";
const CA_CERT_HINT: &str = "ensure it points to a PEM file containing one or more CERTIFICATE blocks, or unset it to use system roots";
const REQUIRED_SIGNATURE_SCHEME: rustls::SignatureScheme =
    rustls::SignatureScheme::ECDSA_NISTP521_SHA512;
type PemSection = (SectionKind, Vec<u8>);

static SHARED_CHATGPT_CLOUDFLARE_COOKIE_STORE: LazyLock<Arc<ChatGptCloudflareCookieStore>> =
    LazyLock::new(|| Arc::new(ChatGptCloudflareCookieStore::default()));

pub(crate) fn backoff(base: Duration, attempt: u64) -> Duration {
    if attempt == 0 {
        return base;
    }
    let exponent = 2_u64.saturating_pow(attempt as u32 - 1);
    let raw_millis = (base.as_millis() as u64).saturating_mul(exponent);
    let jitter: f64 = rand::rng().random_range(0.9..1.1);
    Duration::from_millis((raw_millis as f64 * jitter) as u64)
}

pub(crate) async fn bounded_error_body(
    mut response: reqwest::Response,
    max_bytes: usize,
    max_chars: usize,
) -> String {
    let mut body = Vec::with_capacity(max_bytes);
    while body.len() < max_bytes {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Err(_) if body.is_empty() => return "unreadable response".to_string(),
            Ok(None) | Err(_) => break,
        };
        let remaining = max_bytes.saturating_sub(body.len());
        body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
    }
    String::from_utf8_lossy(&body)
        .chars()
        .take(max_chars)
        .collect()
}

/// Installs Codex's process-wide AWS-LC rustls provider.
///
/// AWS-LC retains ECDSA P-521/SHA-512 support needed by some enterprise TLS
/// proxies. A provider installed earlier by an embedding host is preserved.
pub(crate) fn ensure_rustls_crypto_provider() {
    static RUSTLS_PROVIDER_INIT: Once = Once::new();
    RUSTLS_PROVIDER_INIT.call_once(|| {
        if rustls::crypto::aws_lc_rs::default_provider()
            .install_default()
            .is_err()
        {
            return;
        }

        let Some(provider) = rustls::crypto::CryptoProvider::get_default() else {
            panic!("aws-lc-rs rustls crypto provider should be installed");
        };
        assert!(
            provider
                .signature_verification_algorithms
                .supported_schemes()
                .contains(&REQUIRED_SIGNATURE_SCHEME),
            "installed rustls crypto provider must support {REQUIRED_SIGNATURE_SCHEME:?}"
        );
    });
}

pub(crate) fn with_chatgpt_cloudflare_cookie_store(
    builder: reqwest::ClientBuilder,
) -> reqwest::ClientBuilder {
    builder.cookie_provider(Arc::clone(&SHARED_CHATGPT_CLOUDFLARE_COOKIE_STORE))
}

/// Builds a Reqwest client with Codex's custom-CA precedence and parsing policy.
pub(crate) fn build_client(builder: reqwest::ClientBuilder) -> Result<reqwest::Client> {
    // Reqwest is built without a bundled provider, so this is required even
    // when the system root set is used unchanged.
    ensure_rustls_crypto_provider();
    let Some(bundle) = ConfiguredCaBundle::from_environment() else {
        return builder
            .build()
            .context("failed to build HTTP client with system roots");
    };

    let certificates = bundle.load_certificates()?;
    let mut builder = builder.use_rustls_tls();
    for (index, certificate) in certificates.iter().enumerate() {
        let certificate = reqwest::Certificate::from_der(certificate).with_context(|| {
            format!(
                "failed to parse certificate #{} from {} selected by {}; {CA_CERT_HINT}",
                index + 1,
                bundle.path.display(),
                bundle.source_env
            )
        })?;
        builder = builder.add_root_certificate(certificate);
    }
    builder.build().with_context(|| {
        format!(
            "failed to build HTTP client using CA bundle from {} ({})",
            bundle.source_env,
            bundle.path.display()
        )
    })
}

#[derive(Debug)]
struct ConfiguredCaBundle {
    source_env: &'static str,
    path: PathBuf,
}

impl ConfiguredCaBundle {
    fn from_environment() -> Option<Self> {
        nonempty_path(CODEX_CA_CERT_ENV)
            .map(|path| Self {
                source_env: CODEX_CA_CERT_ENV,
                path,
            })
            .or_else(|| {
                nonempty_path(SSL_CERT_FILE_ENV).map(|path| Self {
                    source_env: SSL_CERT_FILE_ENV,
                    path,
                })
            })
    }

    fn load_certificates(&self) -> Result<Vec<Vec<u8>>> {
        let pem_data = std::fs::read(&self.path).with_context(|| {
            format!(
                "failed to read CA certificate file {} selected by {}; {CA_CERT_HINT}",
                self.path.display(),
                self.source_env
            )
        })?;
        let normalized = NormalizedPem::from_pem_data(&pem_data);
        let mut certificates = Vec::new();
        for section in normalized.sections() {
            let (kind, der) = section.with_context(|| {
                format!(
                    "failed to parse PEM file {} selected by {}; {CA_CERT_HINT}",
                    self.path.display(),
                    self.source_env
                )
            })?;
            if kind == SectionKind::Certificate {
                let certificate = normalized.certificate_der(&der).ok_or_else(|| {
                    anyhow!(
                        "failed to extract certificate data from {} selected by {}; {CA_CERT_HINT}",
                        self.path.display(),
                        self.source_env
                    )
                })?;
                certificates.push(certificate.to_vec());
            }
        }
        if certificates.is_empty() {
            return Err(anyhow!(
                "no certificates found in PEM file {} selected by {}; {CA_CERT_HINT}",
                self.path.display(),
                self.source_env
            ));
        }
        Ok(certificates)
    }
}

fn nonempty_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

enum NormalizedPem {
    Standard(String),
    TrustedCertificate(String),
}

impl NormalizedPem {
    fn from_pem_data(pem_data: &[u8]) -> Self {
        let pem = String::from_utf8_lossy(pem_data);
        if pem.contains("TRUSTED CERTIFICATE") {
            Self::TrustedCertificate(
                pem.replace("BEGIN TRUSTED CERTIFICATE", "BEGIN CERTIFICATE")
                    .replace("END TRUSTED CERTIFICATE", "END CERTIFICATE"),
            )
        } else {
            Self::Standard(pem.into_owned())
        }
    }

    fn contents(&self) -> &str {
        match self {
            Self::Standard(contents) | Self::TrustedCertificate(contents) => contents,
        }
    }

    fn sections(&self) -> impl Iterator<Item = std::result::Result<PemSection, pem::Error>> + '_ {
        PemSection::pem_slice_iter(self.contents().as_bytes())
    }

    fn certificate_der<'a>(&self, der: &'a [u8]) -> Option<&'a [u8]> {
        match self {
            Self::Standard(_) => Some(der),
            Self::TrustedCertificate(_) => first_der_item(der),
        }
    }
}

fn first_der_item(der: &[u8]) -> Option<&[u8]> {
    der_item_length(der).map(|length| &der[..length])
}

fn der_item_length(der: &[u8]) -> Option<usize> {
    let &length_octet = der.get(1)?;
    if length_octet & 0x80 == 0 {
        return Some(2 + usize::from(length_octet)).filter(|length| *length <= der.len());
    }

    let length_octets = usize::from(length_octet & 0x7f);
    if length_octets == 0 {
        return None;
    }
    let length_end = 2_usize.checked_add(length_octets)?;
    let mut content_length = 0_usize;
    for &byte in der.get(2..length_end)? {
        content_length = content_length
            .checked_mul(256)?
            .checked_add(usize::from(byte))?;
    }
    length_end
        .checked_add(content_length)
        .filter(|length| *length <= der.len())
}

#[derive(Debug, Default)]
struct ChatGptCloudflareCookieStore {
    jar: Jar,
}

impl CookieStore for ChatGptCloudflareCookieStore {
    fn set_cookies(
        &self,
        cookie_headers: &mut dyn Iterator<Item = &HeaderValue>,
        url: &reqwest::Url,
    ) {
        if !is_chatgpt_cookie_url(url) {
            return;
        }
        let mut cloudflare_headers =
            cookie_headers.filter(|header| is_allowed_cloudflare_set_cookie_header(header));
        self.jar.set_cookies(&mut cloudflare_headers, url);
    }

    fn cookies(&self, url: &reqwest::Url) -> Option<HeaderValue> {
        is_chatgpt_cookie_url(url)
            .then(|| self.jar.cookies(url).and_then(only_cloudflare_cookies))
            .flatten()
    }
}

fn is_chatgpt_cookie_url(url: &reqwest::Url) -> bool {
    url.scheme() == "https" && url.host_str().is_some_and(is_allowed_chatgpt_host)
}

fn is_allowed_chatgpt_host(host: &str) -> bool {
    const EXACT_HOSTS: &[&str] = &["chatgpt.com", "chat.openai.com", "chatgpt-staging.com"];
    const SUBDOMAIN_SUFFIXES: &[&str] = &[".chatgpt.com", ".chatgpt-staging.com"];
    EXACT_HOSTS.contains(&host)
        || SUBDOMAIN_SUFFIXES
            .iter()
            .any(|suffix| host.ends_with(suffix))
}

fn is_allowed_cloudflare_set_cookie_header(header: &HeaderValue) -> bool {
    header
        .to_str()
        .ok()
        .and_then(|header| header.split_once('=').map(|(name, _)| name.trim()))
        .is_some_and(is_allowed_cloudflare_cookie_name)
}

fn only_cloudflare_cookies(header: HeaderValue) -> Option<HeaderValue> {
    let cookies = header
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|cookie| {
            let cookie = cookie.trim();
            let name = cookie.split_once('=')?.0.trim();
            is_allowed_cloudflare_cookie_name(name).then_some(cookie)
        })
        .collect::<Vec<_>>()
        .join("; ");
    (!cookies.is_empty())
        .then(|| HeaderValue::from_str(&cookies).ok())
        .flatten()
}

fn is_allowed_cloudflare_cookie_name(name: &str) -> bool {
    matches!(
        name,
        "__cf_bm"
            | "__cflb"
            | "__cfruid"
            | "__cfseq"
            | "__cfwaitingroom"
            | "_cfuvid"
            | "cf_clearance"
            | "cf_ob_info"
            | "cf_use_ob"
    ) || name.starts_with("cf_chl_")
}

#[cfg(test)]
mod tests {
    use super::ChatGptCloudflareCookieStore;
    use super::der_item_length;
    use reqwest::cookie::CookieStore;
    use reqwest::header::HeaderValue;

    #[test]
    fn cloudflare_store_excludes_account_cookies() {
        let store = ChatGptCloudflareCookieStore::default();
        let url = reqwest::Url::parse("https://chatgpt.com/backend-api/codex/responses").unwrap();
        let cloudflare = HeaderValue::from_static("_cfuvid=visitor; Path=/; Secure");
        let account = HeaderValue::from_static("chatgpt_session=secret; Path=/; Secure");
        store.set_cookies(&mut [&cloudflare, &account].into_iter(), &url);
        assert_eq!(
            store
                .cookies(&url)
                .and_then(|value| value.to_str().ok().map(str::to_string)),
            Some("_cfuvid=visitor".to_string())
        );
    }

    #[test]
    fn der_length_ignores_trailing_trusted_certificate_metadata() {
        assert_eq!(der_item_length(&[0x30, 0x03, 1, 2, 3, 9, 9]), Some(5));
    }
}
