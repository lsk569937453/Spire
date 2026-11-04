use crate::app_error;
use crate::control_plane::certificate_api::LetsEncryptActions;
use crate::utils::fs_utils::get_domain_path;
use crate::vojo::acme_client::LetsEntrypt;
use crate::vojo::app_config::{AcmeConfig, AppConfig, ServiceType};
use crate::vojo::app_error::AppError;
use std::fs::File;
use std::io::BufReader;
use std::sync::Arc;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc2822;
use tokio::task::JoinHandle;
use tokio::time::{Duration, interval};
#[derive(Debug)]
pub struct CertificateManager {
    config: Arc<AppConfig>,
    renewal_task_handles: Vec<JoinHandle<()>>,
}

impl CertificateManager {
    pub fn new(config: Arc<AppConfig>) -> Self {
        Self {
            config,
            renewal_task_handles: Vec::new(),
        }
    }

    pub fn start_renewal_task(&mut self) {
        info!("Starting single-threaded certificate renewal service...");

        let mut domains_to_check = Vec::new();
        for service in self.config.api_service_config.values() {
            if !(service.server_type == ServiceType::Https
                || service.server_type == ServiceType::Http2Tls)
            {
                continue;
            }

            for domain_config in &service.domain_config {
                info!("Discovered domain [{domain_config}], adding to the renewal check queue.");
                domains_to_check.push(domain_config.clone());
            }
        }

        if domains_to_check.is_empty() {
            info!("No domains configured for renewal.");
            return;
        }
        let global_acme_config = self.config.acme.clone();

        let handle = tokio::spawn(async move {
            let mut timer = interval(Duration::from_secs(30));

            loop {
                timer.tick().await;
                info!("Starting a new certificate renewal check cycle...");

                for domain_conf in &domains_to_check {
                    let domain_name = domain_conf;
                    info!("Performing renewal check for domain: [{domain_name}]");

                    if Self::needs_renewal(domain_name).await {
                        info!("Certificate for [{domain_name}] needs renewal, attempting...");

                        match Self::renew_certificate(domain_name, &global_acme_config).await {
                            Ok(_) => {
                                info!("Successfully renewed certificate for [{domain_name}].");
                            }
                            Err(e) => {
                                error!("Error renewing certificate for [{domain_name}]: {e:?}");
                            }
                        }
                    } else {
                        info!("Certificate for [{domain_name}] does not need renewal yet.");
                    }
                }
                info!("This certificate renewal check cycle is complete.");
            }
        });

        self.renewal_task_handles.push(handle);
    }
    async fn needs_renewal(domain_config: &str) -> bool {
        let domain_name = domain_config;
        if domain_name.is_empty() {
            return true;
        }
        let mut cert_path = match get_domain_path(domain_name) {
            Ok(s) => s,
            Err(_) => return true,
        };
        cert_path = cert_path.join("cert.pem");

        info!("Checking certificate validity: {cert_path:?}");

        match File::open(&cert_path) {
            Ok(file) => {
                let mut reader = BufReader::new(file);
                match rustls_pemfile::read_one(&mut reader) {
                    Ok(Some(item)) => {
                        if let rustls_pemfile::Item::X509Certificate(cert_der) = item {
                            match x509_parser::parse_x509_certificate(cert_der.as_ref()) {
                                Ok((_, x509_cert)) => {
                                    let expiration_datetime =
                                        x509_cert.validity().not_after.to_datetime();

                                    let now = OffsetDateTime::now_utc();

                                    if now > expiration_datetime {
                                        error!(
                                            "Certificate [{}] expired on {:?}.",
                                            cert_path.display(),
                                            expiration_datetime.format(&Rfc2822)
                                        );
                                        true
                                    } else {
                                        let remaining_duration = expiration_datetime - now;
                                        let remaining_days = remaining_duration.whole_days();

                                        const EXPIRATION_THRESHOLD_DAYS: i64 = 30;

                                        if remaining_days < EXPIRATION_THRESHOLD_DAYS {
                                            warn!(
                                                "Certificate [{}] will expire in {} days (Expiration date: {:?})",
                                                cert_path.display(),
                                                remaining_days,
                                                expiration_datetime.format(&Rfc2822)
                                            );
                                            true
                                        } else {
                                            info!(
                                                "Certificate [{}] is valid for {} more days (Expiration date: {:?})",
                                                cert_path.display(),
                                                remaining_days,
                                                expiration_datetime.format(&Rfc2822)
                                            );
                                            false
                                        }
                                    }
                                }
                                Err(e) => {
                                    error!(
                                        "Failed to parse X.509 certificate from [{}]: {}",
                                        cert_path.display(),
                                        e
                                    );
                                    true
                                }
                            }
                        } else {
                            error!(
                                "Item found in [{}] is not a certificate",
                                cert_path.display()
                            );
                            true
                        }
                    }
                    Ok(None) => {
                        error!("No PEM item found in [{}]", cert_path.display());
                        true
                    }
                    Err(e) => {
                        error!(
                            "Failed to read certificate from [{}]: {}",
                            cert_path.display(),
                            e
                        );
                        true
                    }
                }
            }
            Err(e) => {
                error!(
                    "Failed to read certificate from [{}]: {}",
                    cert_path.display(),
                    e
                );

                true
            }
        }
    }

    async fn renew_certificate(domain_config: &String, acme: &AcmeConfig) -> Result<(), AppError> {
        let domain_name = domain_config;
        if domain_name.is_empty() {
            return Err(app_error!("Renewal failed: domain name is empty."));
        }
        let lets_entrypt = LetsEntrypt {
            mail_name: "EMAIL".to_string(),
            domain_name: domain_name.clone(),
        };
        info!("Simulating renewal process for domain: [{domain_name}]");

        let base_path = get_domain_path(domain_name)?;

        tokio::fs::create_dir_all(&base_path).await.map_err(|e| {
            app_error!(
                "Failed to create certificate storage directory {:?}: {}",
                base_path,
                e
            )
        })?;

        let cert_path = base_path.join("cert.pem");
        let key_path = base_path.join("key.pem");

        info!(
            " - Performing ACME challenge and requesting certificate for domain [{domain_name}]..."
        );
        let (key_pem, cert_pem) = lets_entrypt.obtain_certificate(acme).await?;
        info!(" - Successfully obtained certificate and private key.");

        info!(" - Saving new certificate to: {cert_path:?}");
        tokio::fs::write(&cert_path, cert_pem).await.map_err(|e| {
            app_error!("Failed to write certificate file to {:?}: {}", cert_path, e)
        })?;

        info!(" - Saving new private key to: {key_path:?}");
        tokio::fs::write(&key_path, key_pem)
            .await
            .map_err(|e| app_error!("Failed to write private key file to {:?}: {}", key_path, e))?;

        info!(
            "Successfully wrote certificate and private key for domain [{domain_name}] to the local filesystem."
        );

        Ok(())
    }
}
