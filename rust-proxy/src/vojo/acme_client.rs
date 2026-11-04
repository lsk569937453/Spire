use super::app_error::AppError;
use crate::app_error;
use crate::control_plane::certificate_api::LetsEncryptActions;
use crate::vojo::app_config::AcmeConfig;
use axum::extract::State;
use axum::{Router, extract::Path, http::StatusCode, routing::any};
use base64::Engine;
use base64::engine::general_purpose;
use instant_acme::RetryPolicy;
use instant_acme::{
    Account, AuthorizationStatus, ChallengeType, Identifier, NewOrder, OrderStatus,
};
use instant_acme::{Authorizations, ExternalAccountKey, ZeroSsl};
use instant_acme::{LetsEncrypt, NewAccount};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::oneshot;
#[derive(Debug, Clone, Deserialize, Serialize, Default)]

pub struct LetsEntrypt {
    pub mail_name: String,
    pub domain_name: String,
}
impl LetsEntrypt {
    async fn spawn_challenge_server(
        &self,
        authorizations: &mut Authorizations<'_>,
    ) -> Result<(oneshot::Sender<()>, tokio::task::JoinHandle<()>), AppError> {
        let mut challenges = HashMap::new();
        while let Some(authz_result) = authorizations.next().await {
            let mut authz = authz_result?;
            if authz.status != AuthorizationStatus::Pending {
                info!(
                    "Skipping authorization for identifier '{}' with status: {:?}",
                    authz.identifier(),
                    authz.status
                );
                continue;
            }

            info!(
                "Processing pending authorization for identifier: '{}'",
                authz.identifier()
            );

            let mut challenge = authz.challenge(ChallengeType::Http01).ok_or_else(|| {
                AppError("No http01 challenge found for this authorization".to_string())
            })?;

            let key_auth = challenge.key_authorization().as_str().to_string();
            let token = key_auth
                .split('.')
                .next()
                .ok_or_else(|| AppError("Could not split token from key_auth string".to_string()))?
                .to_string();
            info!("token is {token},key_auth is {key_auth}");
            challenges.insert(token.clone(), key_auth);
            info!("Setting challenge ready for token: {token}");
            challenge.set_ready().await?;
        }

        if challenges.is_empty() {
            "No pending authorizations found to challenge.".to_string();
        }

        info!("Preparing challenges: {:?}", challenges.keys());
        let acme_router = acme_router(challenges);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        let listener = tokio::net::TcpListener::bind("0.0.0.0:80").await?;

        let server_handle = tokio::task::spawn(async move {
            let res = axum::serve(listener, acme_router)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                    info!("Gracefully shutting down ACME challenge server.");
                })
                .await;
            if let Err(e) = res {
                error!("error is {}", e);
            }
        });

        Ok((shutdown_tx, server_handle))
    }
}
impl LetsEncryptActions for LetsEntrypt {
    async fn obtain_certificate(&self, acme: &AcmeConfig) -> Result<(String, String), AppError> {
        let account = local_account(acme).await?;
        info!("Account created successfully.");
        let identifiers = [Identifier::Dns(self.domain_name.clone())];
        let mut order = account.new_order(&NewOrder::new(&identifiers)).await?;
        let mut authorizations = order.authorizations();
        let (shutdown_tx, server_handle) = self.spawn_challenge_server(&mut authorizations).await?;
        info!("ACME challenge server is running at 0.0.0.0:80.");
        tokio::time::sleep(Duration::from_secs(1)).await;
        let status = order
            .poll_ready(
                &RetryPolicy::default()
                    .backoff(1.0)
                    .initial_delay(Duration::from_secs(1))
                    .timeout(Duration::from_secs(60)),
            )
            .await?;
        if status != OrderStatus::Ready {
            let _ = shutdown_tx.send(());
            server_handle.await.ok();
            return Err(app_error!(
                "Order status is not 'Ready', but '{:?}'",
                status
            ));
        }

        info!("Order is ready, proceeding to finalization.");
        let private_key_pem = order.finalize().await?;
        info!("Order finalized. Polling for the certificate.");
        let cert_chain_pem = order.poll_certificate(&RetryPolicy::default()).await?;
        info!("Certificate obtained successfully. Shutting down challenge server.");
        let _ = shutdown_tx.send(());
        server_handle.await.ok();

        info!("private key:\n{private_key_pem}");
        Ok((private_key_pem, cert_chain_pem))
    }
}
impl LetsEntrypt {
    pub fn _new(mail_name: String, domain_name: String) -> Self {
        LetsEntrypt {
            mail_name,
            domain_name,
        }
    }
}
pub async fn http01_challenge(
    State(challenges): State<HashMap<String, String>>,
    Path(token): Path<String>,
) -> Result<String, StatusCode> {
    info!("received HTTP-01 ACME challenge,{token}");

    if let Some(key_auth) = challenges.get(&token) {
        Ok({
            info!("responding to ACME challenge,{key_auth}");
            key_auth.clone()
        })
    } else {
        tracing::warn!(%token, "didn't find acme challenge");
        Err(StatusCode::NOT_FOUND)
    }
}

pub fn acme_router(challenges: HashMap<String, String>) -> Router {
    Router::new()
        .route("/.well-known/acme-challenge/{*rest}", any(http01_challenge))
        .with_state(challenges)
}
use rustls::crypto::ring;
async fn local_account(acme: &AcmeConfig) -> Result<Account, AppError> {
    info!("installing ring");
    let _ = ring::default_provider().install_default();
    info!("installing ring done");

    info!("creating test account");

    let (dir, external_account) = match acme {
        AcmeConfig::LetsEncrypt => (LetsEncrypt::Production.url().to_owned(), None),
        AcmeConfig::ZeroSsl {
            eab_kid,
            eab_hmac_key,
        } => {
            let hmac_key_bytes = general_purpose::URL_SAFE_NO_PAD
                .decode(eab_hmac_key)
                .map_err(|e| {
                    app_error!(
                        "Invalid ZeroSSL HMAC key for key_id '{}': not valid base64url. Error: {}",
                        eab_kid,
                        e
                    )
                })?;

            let eab_key = ExternalAccountKey::new(eab_kid.clone(), &hmac_key_bytes);
            (ZeroSsl::Production.url().to_owned(), Some(eab_key))
        }
    };
    info!("ACME Directory URL: {dir:?}",);

    let account_builder = Account::builder()?;
    let (account, _) = account_builder
        .create(
            &NewAccount {
                contact: &[],
                terms_of_service_agreed: true,
                only_return_existing: false,
            },
            dir,
            external_account.as_ref(),
        )
        .await?;
    Ok(account)
}
#[cfg(test)]
mod tests {
    use super::*;
    use http::Request;
    use tower::ServiceExt; // for `oneshot`

    #[cfg(test)]
    mod unit_tests {

        use super::*;

        #[tokio::test]
        async fn http01_challenge_handler_logic() {
            let token = "test-token-123".to_string();
            let key_auth = "key-auth-abc".to_string();
            let mut challenges = HashMap::new();
            challenges.insert(token.clone(), key_auth.clone());

            let state = State(challenges);

            let path_found = Path(token);
            let response = http01_challenge(state.clone(), path_found).await;
            assert_eq!(response, Ok(key_auth));

            let path_not_found = Path("unknown-token".to_string());
            let response_not_found = http01_challenge(state, path_not_found).await;
            assert_eq!(response_not_found, Err(StatusCode::NOT_FOUND));
        }
        use axum::body::to_bytes;
        #[tokio::test]
        async fn acme_router_works() {
            let token = "another-token-456".to_string();
            let key_auth = "another-key-auth-def".to_string();
            let challenges = HashMap::from([(token.clone(), key_auth.clone())]);

            let app = acme_router(challenges);

            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/.well-known/acme-challenge/{token}"))
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::OK);
            let body = response.into_body();
            let body = to_bytes(body, usize::MAX).await.unwrap();
            assert_eq!(&body[..], key_auth.as_bytes());

            let response_not_found = app
                .oneshot(
                    Request::builder()
                        .uri("/.well-known/acme-challenge/wrong-token")
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();

            assert_eq!(response_not_found.status(), StatusCode::NOT_FOUND);
        }
    }

    #[tokio::test]
    async fn full_certificate_request_flow() {
        let test_domain = "your-test-domain.com".to_string();
        let test_email = "test@example.com".to_string();

        let le_request = LetsEntrypt {
            mail_name: test_email,
            domain_name: test_domain,
        };

        let result = le_request
            .obtain_certificate(&AcmeConfig::LetsEncrypt)
            .await;

        assert!(
            result.is_err(),
            "Certificate request failed: {:?}",
            result.err()
        );
    }
}
