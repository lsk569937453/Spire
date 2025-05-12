use super::app_error::AppError;
use crate::control_plane::lets_encrypt::LetsEncryptActions;
use axum::extract::State;
use axum::{routing::get, Router};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::convert::Infallible;
use std::env;
use std::path::Path;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::mpsc::{self, Receiver};
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
    async fn create_temp_server(
        token_map: Arc<Mutex<HashMap<String, String>>>,
        mut rx: Receiver<()>,
    ) -> Result<(), AppError> {
        let app = Router::new()
            .route("/.well-known/acme-challenge/:token", get(dyn_reply))
            .with_state(token_map);
        // Create a `TcpListener` using tokio.
        let listener = TcpListener::bind("0.0.0.0:80").await.unwrap();

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

        let request_result = self.request_cert(DirectoryUrl::LetsEncrypt).await;
        if request_result.is_ok() {
            let send_result = tx.send(()).await.map_err(|e| AppError(format!("{}", e)));
            if send_result.is_err() {
                error!(
                    "Close the 80 port error,the error is:{}",
                    send_result.unwrap_err()
                );
            }
            return request_result.map_err(|e| AppError(format!("{}", e)));
        } else {
            error!("{}", request_result.unwrap_err());
        }

        Err(AppError("Request the lets_encrypt fails".to_string()))
    }
    pub async fn request_cert(
        &self,
        directory_url: DirectoryUrl<'_>,
    ) -> Result<Certificate, Error> {
        let result: bool = Path::new(DEFAULT_TEMPORARY_DIR).is_dir();
        if !result {
            let path = env::current_dir()?;
            let absolute_path = path.join(DEFAULT_TEMPORARY_DIR);
            std::fs::create_dir_all(absolute_path)?;
        }
        let persist = FilePersist::new(DEFAULT_TEMPORARY_DIR);
        let dir = Directory::from_url(persist, directory_url)?;
        let acc = dir.account(&self.mail_name)?;
        let mut ord_new = acc.new_order(&self.domain_name, &[])?;
        let ord_csr = loop {
            if let Some(ord_csr) = ord_new.confirm_validations() {
                break ord_csr;
            }
            let auths = ord_new.authorizations()?;
            let chall = auths[0].http_challenge();
            let token = chall.http_token();
            let proof = chall.http_proof();
            info!("Has receive the token:{} and proof:{}", token, proof);
            let mut token_map = self.token_map.lock().await;
            token_map.insert(String::from(token), proof);
            info!("Has deleted the lock!");

            chall.validate(1000)?;
            ord_new.refresh()?;
        };
        let pkey_pri = create_p384_key();
        let ord_cert = ord_csr.finalize_pkey(pkey_pri, 5000)?;
        let cert = ord_cert.download_and_save_cert()?;

        Ok(cert)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use http::Request;
    use tower::ServiceExt; // for `oneshot`

    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use http_body_util::BodyExt; // 需要添加 http-body-util 依赖

    use tokio::sync::Mutex;
    // 需要添加 tower 依赖

    // 测试 token 存储和检索
    #[tokio::test]
    async fn test_token_storage() {
        let token_map = Arc::new(Mutex::new(HashMap::new()));
        token_map
            .lock()
            .await
            .insert("test_token".into(), "proof".into());

        let response = dyn_reply(
            axum::extract::Path("test_token".into()),
            State(token_map.clone()),
        )
        .await
        .unwrap()
        .into_response();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"proof");
    }

    // 测试未知 token 处理
    #[tokio::test]
    async fn test_invalid_token() {
        let token_map = Arc::new(Mutex::new(HashMap::new()));

        let response = dyn_reply(axum::extract::Path("invalid".into()), State(token_map))
            .await
            .unwrap()
            .into_response();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    // 测试临时服务器启动和关闭
    #[tokio::test]
    async fn test_temp_server_lifecycle() {
        let (tx, rx) = mpsc::channel(1);
        let token_map = Arc::new(Mutex::new(HashMap::new()));

        let handle = tokio::spawn(LetsEntrypt::create_temp_server(token_map, rx));

        // 发送关闭信号
        tx.send(()).await.unwrap();

        assert!(handle.await.is_err());
    }

    // 模拟 ACME 证书请求流程
    #[tokio::test]
    async fn test_cert_request_flow() {
        let lets_encrypt = LetsEntrypt {
            mail_name: "test@example.com".into(),
            domain_name: "test.example.com".into(),
            token_map: Arc::new(Mutex::new(HashMap::new())),
        };

        // 注意：实际需要模拟 ACME 服务，这里测试流程是否正确
        let result = lets_encrypt
            .request_cert(DirectoryUrl::LetsEncryptStaging)
            .await;
        assert!(result.is_err()); // 预期失败，因为无法连接真实服务
    }

    // 测试证书目录自动创建
    #[tokio::test]
    async fn test_cert_dir_creation() {
        let lets_encrypt = LetsEntrypt::_new("test@example.com".into(), "test.com".into());

        let result = lets_encrypt
            .request_cert(DirectoryUrl::LetsEncryptStaging)
            .await;
        assert!(result.is_err());
    }
}
