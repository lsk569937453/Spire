use super::app_error::AppError;
use crate::control_plane::lets_encrypt::LetsEncryptActions;
use axum::extract::State;
use axum::{extract::Path, http::StatusCode, routing::any, Router};
use hyper_util::client::legacy::Client as HyperClient;
use hyper_util::rt::TokioExecutor;
use instant_acme::LetsEncrypt;
use instant_acme::NewAccount;
use instant_acme::{
    Account, AuthorizationStatus, ChallengeType, Identifier, NewOrder, OrderStatus,
};
use rcgen::{CertificateParams, DistinguishedName, KeyPair};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
#[derive(Debug, Clone, Deserialize, Serialize, Default)]

pub struct LetsEntrypt {
    pub mail_name: String,
    pub domain_name: String,
}
impl LetsEncryptActions for LetsEntrypt {
    async fn start_request2(&self) -> Result<String, AppError> {
        let account = local_account(self.mail_name.clone()).await?;
        info!("account created");
        let domain_name = self.domain_name.clone();
        let domain = domain_name.as_str();
        let mut order = account
            .new_order(&NewOrder {
                identifiers: &[Identifier::Dns(domain.to_string())],
            })
            .await?;
        let authorizations = order.authorizations().await?;

        let authorization = authorizations
            .first()
            .ok_or(AppError("there should be one authorization".to_string()))?;

        if !matches!(authorization.status, AuthorizationStatus::Pending) {
            Err(AppError("order should be pending".to_string()))?;
        }
        let challenge = authorization
            .challenges
            .iter()
            .find(|c| c.r#type == ChallengeType::Http01)
            .ok_or_else(|| AppError("no http01 challenge found".to_string()))?;

        let challenges = HashMap::from([(
            challenge.token.clone(),
            order.key_authorization(challenge).as_str().to_string(),
        )]);
        info!("challenges: {:?}", challenges);
        let acme_router = acme_router(challenges);
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

        let listener = tokio::net::TcpListener::bind("0.0.0.0:80").await?;
        let server_handle = tokio::task::spawn(async move {
            axum::serve(listener, acme_router)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await
                .unwrap()
        });
        info!("Serving ACME handler at: 0.0.0.0:80");
        let result = async {
            order.set_challenge_ready(&challenge.url).await?;
            let mut tries = 1u8;
            let mut delay = Duration::from_millis(250);
            loop {
                tokio::time::sleep(delay).await;
                let state = order.refresh().await?;
                if let OrderStatus::Ready | OrderStatus::Invalid = state.status {
                    info!("order state: {:#?}", state);
                    break;
                }

                delay *= 2;
                tries += 1;
                if tries < 15 {
                    info!("order is not ready, waiting {delay:?},{:?}{}", state, tries);
                } else {
                    error!(
                        "timed out before order reached ready state: {state:#?},{}",
                        tries,
                    );
                    Err(AppError(
                        "timed out before order reached ready state".to_string(),
                    ))?;
                }
            }

            let state = order.state();
            if state.status != OrderStatus::Ready {
                Err(AppError(format!(
                    "unexpected order status: {:?}",
                    state.status
                )))?;
            }

            info!("challenge completed,{:?}", state);

            let mut params = CertificateParams::new(vec![domain.to_owned()])?;
            params.distinguished_name = DistinguishedName::new();
            let private_key = KeyPair::generate()?;
            let signing_request = params.serialize_request(&private_key)?;

            order.finalize(signing_request.der()).await?;

            let mut cert_chain_pem: Option<String> = None;
            let mut retries = 5;
            while cert_chain_pem.is_none() && retries > 0 {
                cert_chain_pem = order.certificate().await?;
                retries -= 1;
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            cert_chain_pem.ok_or_else(|| AppError("certificate timeout".to_string()))
        }
        .await;
        let _ = shutdown_tx.send(());
        server_handle.await.ok();

        result
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
    info!("received HTTP-01 ACME challenge,{}", token);

    if let Some(key_auth) = challenges.get(&token) {
        Ok({
            info!("responding to ACME challenge,{}", key_auth);
            key_auth.clone()
        })
    } else {
        tracing::warn!(%token, "didn't find acme challenge");
        Err(StatusCode::NOT_FOUND)
    }
}

/// Set up a simple acme server to respond to http01 challenges.
pub fn acme_router(challenges: HashMap<String, String>) -> Router {
    Router::new()
        .route("/.well-known/acme-challenge/{*rest}", any(http01_challenge))
        .with_state(challenges)
}
use rustls::crypto::ring;
use rustls::RootCertStore;
async fn local_account(mail_name: String) -> Result<Account, AppError> {
    info!("installing ring");
    let _ = ring::default_provider().install_default();
    info!("installing ring done");
    let root_store = RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.into(),
    };
    let roots = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    info!("creating test account1");
    let https = HyperClient::builder(TokioExecutor::new()).build(
        hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(roots)
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .build(),
    );
    info!("creating test account2");
    let (account, _) = Account::create_with_http(
        &NewAccount {
            contact: &[],
            terms_of_service_agreed: true,
            only_return_existing: false,
        },
        LetsEncrypt::Staging.url().to_owned().as_str(),
        None,
        Box::new(https.clone()),
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
        use std::usize;

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
                        .uri(format!("/.well-known/acme-challenge/{}", token))
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

        let result = le_request.start_request2().await;

        assert!(
            result.is_err(),
            "Certificate request failed: {:?}",
            result.err()
        );
    }
}
