use rustls::crypto::ring::sign::any_supported_type;
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug)]
pub struct SniCertResolver {
    certs: HashMap<String, Arc<sign::CertifiedKey>>,
    default_cert: Option<Arc<sign::CertifiedKey>>,
}

impl SniCertResolver {
    pub fn new() -> Self {
        Self {
            certs: HashMap::new(),
            default_cert: None,
        }
    }

    /// Registers a certificate for `domain`. The first registered domain
    /// should be marked as default; it is served when the client sends no
    /// SNI or an unknown name.
    pub fn add_certified_key(
        &mut self,
        domain: &str,
        certified_key: sign::CertifiedKey,
        is_default: bool,
    ) {
        let certified_key = Arc::new(certified_key);
        self.certs.insert(domain.to_lowercase(), certified_key.clone());

        if is_default {
            self.default_cert = Some(certified_key);
        }
    }

    /// SNI lookup split out of `resolve` so it can be unit-tested without a
    /// real `ClientHello`.
    pub fn resolve_by_name(&self, name: &str) -> Option<Arc<sign::CertifiedKey>> {
        if let Some(cert) = self.certs.get(&name.to_lowercase()) {
            info!("SNI match for: {name}, providing specific certificate.");
            return Some(Arc::clone(cert));
        }

        warn!("No SNI match for: {name}, providing default certificate.");
        self.default_cert.as_ref().map(Arc::clone)
    }
}

impl ResolvesServerCert for SniCertResolver {
    fn resolve(&self, client_hello: ClientHello<'_>) -> Option<Arc<sign::CertifiedKey>> {
        client_hello
            .server_name()
            .map(|sni_name| {
                debug!("Resolving certificate for SNI: {sni_name}");
                self.resolve_by_name(sni_name)
            })
            .unwrap_or_else(|| {
                warn!("No SNI in client hello, providing default certificate.");
                self.default_cert.as_ref().map(Arc::clone)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::crypto::ring;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

    fn self_signed_certified_key(
        domain: &str,
    ) -> (sign::CertifiedKey, Vec<CertificateDer<'static>>) {
        let _ = ring::default_provider().install_default();

        let mut params = rcgen::CertificateParams::new(vec![domain.to_string()]).unwrap();
        params.distinguished_name = rcgen::DistinguishedName::new();
        let key_pair = rcgen::KeyPair::generate().unwrap();
        let cert = params.self_signed(&key_pair).unwrap();

        let certs = vec![cert.der().clone()];
        let key = PrivateKeyDer::from(PrivatePkcs8KeyDer::from(key_pair.serialize_der()));
        let signing_key = any_supported_type(&key).unwrap();
        (
            sign::CertifiedKey::new(certs.clone(), signing_key),
            certs,
        )
    }

    #[test]
    fn resolves_by_sni_name() {
        let mut resolver = SniCertResolver::new();
        let (www_key, www_certs) = self_signed_certified_key("www.example.com");
        let (apex_key, apex_certs) = self_signed_certified_key("example.com");
        resolver.add_certified_key("www.example.com", www_key, true);
        resolver.add_certified_key("example.com", apex_key, false);

        let www = resolver.resolve_by_name("www.example.com").unwrap();
        let apex = resolver.resolve_by_name("example.com").unwrap();
        assert_eq!(www.end_entity_cert().unwrap(), &www_certs[0]);
        assert_eq!(apex.end_entity_cert().unwrap(), &apex_certs[0]);

        // Unknown names fall back to the default (first) cert.
        let fallback = resolver.resolve_by_name("other.example.com").unwrap();
        assert_eq!(fallback.end_entity_cert().unwrap(), &www_certs[0]);
    }

    #[test]
    fn sni_matching_is_case_insensitive() {
        let mut resolver = SniCertResolver::new();
        let (certified_key, certs) = self_signed_certified_key("example.com");
        resolver.add_certified_key("example.com", certified_key, true);

        let resolved = resolver.resolve_by_name("EXAMPLE.COM").unwrap();
        assert_eq!(resolved.end_entity_cert().unwrap(), &certs[0]);
    }

    #[test]
    fn no_default_cert_returns_none_for_unknown_names() {
        let resolver = SniCertResolver::new();
        assert!(resolver.resolve_by_name("example.com").is_none());
    }
}
