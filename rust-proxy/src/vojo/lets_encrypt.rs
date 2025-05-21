use super::app_error::AppError;

use axum::extract::State;
use axum::{body::Bytes, extract::Path, http::StatusCode, routing::any, Router};
use http_body_util::Full;
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::{connect::HttpConnector, Client};
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
    }
    Ok((axum::http::StatusCode::OK, String::from("")))
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
        let account = local_account().await?;

        info!("account created");
        let domain_name = self.domain_name.clone();

        let domain = domain_name.as_str();

        let mut order = account
            .new_order(&NewOrder {
                identifiers: &[Identifier::Dns(domain.to_string())],
            })
            .await
            .map_err(|e| AppError("failed to order certificate".to_string()))?;

        let authorizations = order
            .authorizations()
            .await
            .map_err(|e| AppError("failed to retrieve order authorizations".to_string()))?;

        // There should only ever be 1 authorization as we only provided 1 domain above.
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

        let listener = tokio::net::TcpListener::bind("0.0.0.0:5002").await.unwrap();

        // Start the Axum server as a background task, so it's running while we complete the challenge
        // in the next steps.
        tokio::task::spawn(async move { axum::serve(listener, acme_router).await.unwrap() });

        info!("Serving ACME handler at: 0.0.0.0:5002");

        order
            .set_challenge_ready(&challenge.url)
            .await
            .map_err(|e| AppError("failed to notify server that challenge is ready".to_string()))?;

        // We now need to wait until the order reaches an end-state. We refresh the order in a loop,
        // with exponential backoff, until the order is either ready or invalid (for example if our
        // challenge server responded with the wrong key authorization).
        let mut tries = 1u8;
        let mut delay = Duration::from_millis(250);
        loop {
            tokio::time::sleep(delay).await;
            let state = order.refresh().await.unwrap();
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

        // Create a CSR for our domain.
        let mut params =
            CertificateParams::new(vec![domain.to_owned()]).map_err(|e| AppError(e.to_string()))?;
        params.distinguished_name = DistinguishedName::new();
        let private_key = KeyPair::generate().map_err(|e| AppError(e.to_string()))?;
        let signing_request = params
            .serialize_request(&private_key)
            .map_err(|e| AppError(e.to_string()))?;

        // DER encode the CSR and use it to request the certificate.
        order
            .finalize(signing_request.der())
            .await
            .map_err(|e| AppError("failed to finalize order".to_string()))?;

        // Poll for certificate, do this for a few rounds.
        let mut cert_chain_pem: Option<String> = None;
        let mut retries = 5;
        while cert_chain_pem.is_none() && retries > 0 {
            cert_chain_pem = order
                .certificate()
                .await
                .map_err(|e| AppError("failed to get the certificate for order".to_string()))?;
            retries -= 1;
            tokio::time::sleep(Duration::from_secs(1)).await;
        }

        if let Some(chain) = cert_chain_pem {
            info!("certificate chain:\n\n{}", chain);
            info!("private key:\n\n{}", private_key.serialize_pem());
            Ok(chain)
        } else {
            Err(AppError(
                "failed to get certificate for order before timeout".to_string(),
            ))
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

async fn local_account() -> Result<Account, AppError> {
    let http_client = client_with_custom_ca_cert()?;
    let account = create_account(
        http_client.clone(),
        "fake@email.com",
        Some("https://localhost:14000/dir".to_string()),
    )
    .await
    .map_err(|e| AppError("failed to create account.".to_string()))?;

    Ok(account)
}

/// Only used for local run with Pebble.
/// See <https://github.com/letsencrypt/pebble?tab=readme-ov-file#avoiding-client-https-errors> for
/// why we need to add the pebble cert to the client root certificates.
fn client_with_custom_ca_cert(
) -> Result<Box<Client<HttpsConnector<HttpConnector>, Full<Bytes>>>, AppError> {
    use hyper_util::rt::TokioExecutor;
    use rustls::{crypto::ring, RootCertStore};

    let _ = ring::default_provider()
        .install_default()
        .map_err(|e| AppError(format!("{:?}", e)));

    let f = std::fs::File::open("pebble.minica.pem").map_err(|e| AppError(e.to_string()))?;
    let mut ca = std::io::BufReader::new(f);
    let certs = rustls_pemfile::certs(&mut ca)
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();

    let mut roots = RootCertStore::empty();
    roots.add_parsable_certificates(certs);
    // TLS client config using the custom CA store for lookups
    let tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();

    // Prepare the HTTPS connector
    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(tls)
        .https_or_http()
        .enable_http1()
        .build();

    let client = Client::builder(TokioExecutor::new()).build(https);

    Ok(Box::new(client))
}

/// Create a new ACME account that can be restored by using the deserialization
/// of the returned JSON into a [`instant_acme::Account`]
async fn create_account(
    http_client: Box<Client<HttpsConnector<HttpConnector>, Full<Bytes>>>,
    email: &str,
    acme_server: Option<String>,
) -> Result<Account, AppError> {
    use instant_acme::{LetsEncrypt, NewAccount};
    let acme_server = acme_server.unwrap_or_else(|| LetsEncrypt::Production.url().to_string());

    let account: NewAccount = NewAccount {
        contact: &[&format!("mailto:{email}")],
        terms_of_service_agreed: true,
        only_return_existing: false,
    };

    // We only a custom Http client with a specific TLS setup when using Pebble
    let account = Account::create_with_http(&account, &acme_server, None, http_client)
        .await
        .map_err(|e| AppError("failed to create account with custom http client.".to_string()))?
        .0;

    Ok(account)
}
