use rustls_native_certs::CertificateResult;

// `rustls_native_certs::load_native_certs()` first consults SSL_CERT_FILE and SSL_CERT_DIR. Load
// platform roots directly so a configured custom CA layers onto the native trust store instead of
// replacing it. This is the supported-target subset of current upstream Codex's network-proxy
// native-root loader.
#[cfg(target_os = "linux")]
pub(super) fn load_platform_native_certs() -> CertificateResult {
    let mut result =
        rustls_native_certs::load_certs_from_paths(platform_cert_file().as_deref(), None);
    for cert_dir in platform_cert_dirs() {
        extend_certificate_result(
            &mut result,
            rustls_native_certs::load_certs_from_paths(None, Some(&cert_dir)),
        );
    }
    result
        .certs
        .sort_unstable_by(|left, right| left.as_ref().cmp(right.as_ref()));
    result.certs.dedup();
    result
}

#[cfg(target_os = "macos")]
pub(super) fn load_platform_native_certs() -> CertificateResult {
    use rustls::pki_types::CertificateDer;
    use rustls_native_certs::Error;
    use rustls_native_certs::ErrorKind;
    use security_framework::trust_settings::Domain;
    use security_framework::trust_settings::TrustSettings;
    use security_framework::trust_settings::TrustSettingsForCertificate;
    use std::collections::BTreeMap;

    let mut result = CertificateResult::default();
    let mut all_certs = BTreeMap::new();
    for domain in &[Domain::User, Domain::Admin, Domain::System] {
        let trust_settings = TrustSettings::new(*domain);
        let certificates = match trust_settings.iter() {
            Ok(certificates) => certificates,
            Err(error) => {
                result.errors.push(Error {
                    context: match domain {
                        Domain::User => "failed to load user trust settings",
                        Domain::Admin => "failed to load admin trust settings",
                        Domain::System => "failed to load system trust settings",
                    },
                    kind: ErrorKind::Os(error.into()),
                });
                continue;
            }
        };

        for certificate in certificates {
            let der = certificate.to_der();
            let trusted = match trust_settings.tls_trust_settings_for_certificate(&certificate) {
                Ok(trusted) => trusted.unwrap_or(TrustSettingsForCertificate::TrustRoot),
                Err(error) => {
                    result.errors.push(Error {
                        context: "certificate not trusted",
                        kind: ErrorKind::Os(error.into()),
                    });
                    continue;
                }
            };
            all_certs.entry(der).or_insert(trusted);
        }
    }

    for (der, trusted) in all_certs {
        use TrustSettingsForCertificate::TrustAsRoot;
        use TrustSettingsForCertificate::TrustRoot;

        if matches!(trusted, TrustRoot | TrustAsRoot) {
            result.certs.push(CertificateDer::from(der));
        }
    }
    result
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn load_platform_native_certs() -> CertificateResult {
    rustls_native_certs::load_native_certs()
}

#[cfg(target_os = "linux")]
fn extend_certificate_result(result: &mut CertificateResult, extra: CertificateResult) {
    result.certs.extend(extra.certs);
    result.errors.extend(extra.errors);
}

#[cfg(target_os = "linux")]
fn platform_cert_file() -> Option<std::path::PathBuf> {
    PLATFORM_CERTIFICATE_FILE_NAMES
        .iter()
        .map(std::path::Path::new)
        .find(|path| path.exists())
        .map(std::path::Path::to_path_buf)
}

#[cfg(target_os = "linux")]
fn platform_cert_dirs() -> impl Iterator<Item = std::path::PathBuf> {
    PLATFORM_CERTIFICATE_DIRS
        .iter()
        .map(std::path::Path::new)
        .filter(|path| path.exists())
        .map(std::path::Path::to_path_buf)
}

#[cfg(target_os = "linux")]
const PLATFORM_CERTIFICATE_DIRS: &[&str] = &[
    "/etc/ssl/certs",
    "/etc/pki/tls/certs",
    "/etc/security/certificates",
];

#[cfg(target_os = "linux")]
const PLATFORM_CERTIFICATE_FILE_NAMES: &[&str] = &[
    "/etc/ssl/certs/ca-certificates.crt",
    "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem",
    "/etc/pki/tls/certs/ca-bundle.crt",
    "/etc/ssl/ca-bundle.pem",
    "/etc/pki/tls/cacert.pem",
    "/etc/ssl/cert.pem",
    "/opt/etc/ssl/certs/ca-certificates.crt",
    "/etc/ssl/certs/cacert.pem",
];

