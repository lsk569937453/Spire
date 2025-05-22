use super::app_error::AppError;
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
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
#[derive(Debug, Clone, Deserialize, Serialize, Default)]

pub struct LetsEntrypt {
    pub mail_name: String,
    pub domain_name: String,
    #[serde(skip_serializing, skip_deserializing)]
    pub token_map: Arc<Mutex<HashMap<String, String>>>,
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

pub async fn dyn_reply(
    axum::extract::Path(token): axum::extract::Path<String>,
    State(token_map_shared): State<Arc<Mutex<HashMap<String, String>>>>,
) -> Result<impl axum::response::IntoResponse, Infallible> {
    info!("The server has received the token,the token is {}", token);
    let token_map = token_map_shared.lock().await;
    if !token_map.contains_key(&token) {
        error!("Can not find the token:{} from memory.", token);
        return Ok((axum::http::StatusCode::BAD_REQUEST, String::from("")));
    } else {
        // let cloned_map = token_map.clone();
        let proof_option = token_map.get(&token);
        if let Some(proof) = proof_option {
            info!(
                "The server response the proof successfully,token:{},proof:{}",
                token,
                proof.clone()
            );
            return Ok((axum::http::StatusCode::OK, proof.clone().to_string()));
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
            token_map: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn start_request2(&self) -> Result<String, AppError> {
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
