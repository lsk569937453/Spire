use crate::vojo::lets_encrypt::LetsEntrypt;
use crate::vojo::{app_error::AppError, base_response::BaseResponse};
use crate::SharedConfig;
use axum::extract::State;
use axum::response::IntoResponse;
use mockall::automock;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
struct LetsEncryptResponse {
    key_perm: String,
    certificate_perm: String,
}
#[automock]
pub trait LetsEncryptActions: Send + Sync {
    async fn start_request2(&self) -> Result<String, AppError>;
}
pub async fn lets_encrypt_certificate_logic<LEO: LetsEncryptActions>(
    lets_encrypt_object: LEO,
) -> Result<impl IntoResponse, Infallible> {
    let request_result = lets_encrypt_object.start_request2().await;

    match request_result {
        Ok(certificate) => {
            let response = LetsEncryptResponse {
                key_perm: certificate.clone(),
                certificate_perm: certificate,
            };
            let data = BaseResponse {
                response_code: 0,
                response_object: response,
            };
            match serde_json::to_string(&data) {
                Ok(json_str) => Ok((axum::http::StatusCode::OK, json_str)),
                Err(err) => {
                    error!("Error serializing response: {:?}", err); // Log the error for debugging
                    Ok((
                        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to serialize response".to_string(), // Provide a generic error message to the client
                    ))
                }
            }
        }
        Err(err) => Ok((
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            err.to_string(),
        )),
    }
}
pub async fn lets_encrypt_certificate(
    State(_): State<SharedConfig>,

    axum::extract::Json(lets_encrypt_object): axum::extract::Json<LetsEntrypt>,
) -> Result<impl axum::response::IntoResponse, Infallible> {
    lets_encrypt_certificate_logic(lets_encrypt_object).await
}
#[cfg(test)]
mod tests {
    use super::*;

    use axum::body::to_bytes;
    use axum::response::IntoResponse;

    #[tokio::test]
    async fn test_lets_encrypt_certificate_success() {
        let mut mock_le_actions = MockLetsEncryptActions::new();
        mock_le_actions
            .expect_start_request2()
            .times(1)
            .returning(|| Ok("mock_certificate_content".to_string()));
        let response = lets_encrypt_certificate_logic(mock_le_actions)
            .await
            .unwrap();

        let res = response.into_response();

        let (parts, body_data) = res.into_parts();
        let body = to_bytes(body_data, usize::MAX).await.unwrap();
        assert_eq!(parts.status, axum::http::StatusCode::OK);
        let response = serde_json::from_slice::<BaseResponse<LetsEncryptResponse>>(&body);
        assert!(response.is_ok());
    }
    #[tokio::test]
    async fn test_lets_encrypt_certificat_error() {
        let mut mock_le_actions = MockLetsEncryptActions::new();
        mock_le_actions
            .expect_start_request2()
            .times(1)
            .returning(|| Err(AppError("mock_certificate_content".to_string())));
        let response = lets_encrypt_certificate_logic(mock_le_actions)
            .await
            .unwrap();

        let res = response.into_response();

        let (parts, body_data) = res.into_parts();
        let body = to_bytes(body_data, usize::MAX).await.unwrap();
        assert_eq!(parts.status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
        let response = serde_json::from_slice::<String>(&body);
        assert!(response.is_err());
    }
}
