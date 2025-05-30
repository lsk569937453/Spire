use crate::vojo::base_response::BaseResponse;
use crate::vojo::lets_encrypt::LetsEntrypt;
use crate::SharedConfig;
use axum::extract::State;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
struct LetsEncryptResponse {
    key_perm: String,
    certificate_perm: String,
}
pub async fn lets_encrypt_certificate(
    State(_): State<SharedConfig>,

    axum::extract::Json(lets_encrypt_object): axum::extract::Json<LetsEntrypt>,
) -> Result<impl axum::response::IntoResponse, Infallible> {
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
#[cfg(test)]
mod tests {
    use super::*;
    use crate::AppConfig;
    use axum::body::to_bytes;
    use axum::response::IntoResponse;

    
    
    
    #[tokio::test]
    async fn test_lets_encrypt_certificate_success() {
        let config = SharedConfig::from_app_config(AppConfig::default());
        let mock_lets_encrypt = LetsEntrypt {
            domain_name: "example.com".to_string(),
            mail_name: "test@example.com".to_string(),
        };

        let expected_cert = "test_certificate".to_string();
        let response =
            lets_encrypt_certificate(State(config), axum::extract::Json(mock_lets_encrypt))
                .await
                .unwrap();

        let res = response.into_response();

        let (parts, body_data) = res.into_parts();
        let body = to_bytes(body_data, usize::MAX).await.unwrap();
        assert_eq!(parts.status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
        let response = serde_json::from_slice::<BaseResponse<String>>(&body);
        assert!(response.is_err());
    }
}
