use crate::vojo::lets_encrypt::LetsEntrypt;
use crate::vojo::{app_error::AppError, base_response::BaseResponse};
use crate::SharedConfig;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::Json;
use mockall::automock;
use serde::{Deserialize, Serialize};

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
) -> Result<impl IntoResponse, AppError> {
    let certificate_response = lets_encrypt_object.start_request2().await?;
    let data = BaseResponse {
        response_code: 0,
        response_object: certificate_response,
    };

    Ok(Json(data))
}
pub async fn lets_encrypt_certificate(
    State(_): State<SharedConfig>,

    axum::extract::Json(lets_encrypt_object): axum::extract::Json<LetsEntrypt>,
) -> Result<impl axum::response::IntoResponse, AppError> {
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
        assert!(response.is_err());
    }
    #[tokio::test]
    async fn test_lets_encrypt_certificat_error() {
        let mut mock_le_actions = MockLetsEncryptActions::new();
        mock_le_actions
            .expect_start_request2()
            .times(1)
            .returning(|| Err(AppError("mock_certificate_content".to_string())));
        let response = lets_encrypt_certificate_logic(mock_le_actions).await;

        let res = response.into_response();

        let (parts, body_data) = res.into_parts();
        let body = to_bytes(body_data, usize::MAX).await.unwrap();
        assert_eq!(parts.status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
        let response = serde_json::from_slice::<String>(&body);
        assert!(response.is_err());
    }
}
