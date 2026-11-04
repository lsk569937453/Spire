use crate::middleware::middlewares::CheckResult;
use crate::middleware::middlewares::Denial;
use crate::middleware::middlewares::Middleware;
use base64::{Engine as _, engine::general_purpose};
use core::fmt::Debug;
use http::HeaderMap;
use http::HeaderValue;
use http::StatusCode;
use http::header;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use subtle::ConstantTimeEq;

use crate::vojo::app_error::AppError;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub enum JwtAlgorithm {
    #[default]
    HS256,
    HS384,
    HS512,
}
impl From<JwtAlgorithm> for Algorithm {
    fn from(val: JwtAlgorithm) -> Self {
        match val {
            JwtAlgorithm::HS256 => Algorithm::HS256,
            JwtAlgorithm::HS384 => Algorithm::HS384,
            JwtAlgorithm::HS512 => Algorithm::HS512,
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct JwtAuth {
    pub secret: String,
    pub algorithm: JwtAlgorithm,
    pub issuer: Option<String>,
    pub audience: Option<String>,
}

/// Never print the JWT signing secret into logs.
impl Debug for JwtAuth {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("JwtAuth")
            .field("secret", &"<redacted>")
            .field("algorithm", &self.algorithm)
            .field("issuer", &self.issuer)
            .field("audience", &self.audience)
            .finish()
    }
}

impl JwtAuth {
    fn check_authentication(&self, headers: &HeaderMap<HeaderValue>) -> Result<bool, AppError> {
        if let Some(auth_header) = headers.get("Authorization") {
            if let Ok(auth_str) = auth_header.to_str() {
                if let Some(token) = auth_str.strip_prefix("Bearer ") {
                    let mut validation = Validation::new(self.algorithm.clone().into());
                    if let Some(iss) = &self.issuer {
                        validation.set_issuer(&[iss]);
                    }
                    if let Some(aud) = &self.audience {
                        validation.set_audience(&[aud]);
                    }

                    let key = DecodingKey::from_secret(self.secret.as_bytes());

                    match decode::<serde_json::Value>(token, &key, &validation) {
                        Ok(_) => return Ok(true),
                        Err(e) => {
                            error!("JWT validation failed: {e}");
                            return Ok(false);
                        }
                    }
                } else {
                    error!("[JWT AUTH]-Invalid Authorization header format,missing Bearer.");
                }
            }
        } else {
            error!(
                "[JWT AUTH]-Invalid Authorization header format,cannot find Authorization header."
            );
        }
        Ok(false)
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "scheme", rename_all = "PascalCase")]
pub enum Authentication {
    #[serde(rename = "basic")]
    Basic(BasicAuth),
    #[serde(rename = "api_key")]
    ApiKey(ApiKeyAuth),
    #[serde(rename = "jwt")]
    Jwt(JwtAuth),
}
impl Middleware for Authentication {
    fn check_request(
        &self,
        _peer_addr: &SocketAddr,
        headers_option: Option<&HeaderMap<HeaderValue>>,
    ) -> Result<CheckResult, AppError> {
        if let Some(header_map) = headers_option
            && !self.check_authentication(header_map)?
        {
            let denial = Denial {
                status: StatusCode::UNAUTHORIZED,
                headers: HeaderMap::new(),
                body: "Authentication failed".to_string(),
            };
            return Ok(CheckResult::Denied(denial));
        }
        Ok(CheckResult::Allowed)
    }
}
impl Authentication {
    pub fn check_authentication(&self, headers: &HeaderMap<HeaderValue>) -> Result<bool, AppError> {
        match self {
            Authentication::Basic(auth) => auth.check_authentication(headers),
            Authentication::ApiKey(auth) => auth.check_authentication(headers),
            Authentication::Jwt(auth) => auth.check_authentication(headers),
        }
    }
}

/// Constant-time byte comparison, so that credential checks do not leak the
/// position of the first mismatching byte through response timing.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.ct_eq(right).into()
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct BasicAuth {
    pub credentials: String,
}

/// Never print the configured credentials into logs.
impl Debug for BasicAuth {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("BasicAuth")
            .field("credentials", &"<redacted>")
            .finish()
    }
}

impl BasicAuth {
    fn check_authentication(&self, headers: &HeaderMap<HeaderValue>) -> Result<bool, AppError> {
        let Some(value) = headers.get(header::AUTHORIZATION) else {
            return Ok(false);
        };
        let value = value.to_str()?;
        let Some((scheme, encoded)) = value.split_once(' ') else {
            return Ok(false);
        };
        if !scheme.eq_ignore_ascii_case("Basic") {
            return Ok(false);
        }
        // RFC 7617 clients send padded base64, some send it unpadded. Accept
        // both instead of only the exact encoding we happen to produce.
        let decoded = general_purpose::STANDARD
            .decode(encoded)
            .or_else(|_| general_purpose::STANDARD_NO_PAD.decode(encoded));
        let Ok(decoded) = decoded else {
            return Ok(false);
        };
        Ok(constant_time_eq(
            decoded.as_slice(),
            self.credentials.as_bytes(),
        ))
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ApiKeyAuth {
    pub key: String,
    pub value: String,
}

/// Never print the configured API key into logs.
impl Debug for ApiKeyAuth {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ApiKeyAuth")
            .field("key", &self.key)
            .field("value", &"<redacted>")
            .finish()
    }
}

impl ApiKeyAuth {
    fn check_authentication(&self, headers: &HeaderMap<HeaderValue>) -> Result<bool, AppError> {
        let Some(header_value) = headers.get(self.key.as_str()) else {
            return Ok(false);
        };
        let header_value = header_value.to_str()?;
        Ok(constant_time_eq(
            header_value.as_bytes(),
            self.value.as_bytes(),
        ))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    #[test]
    fn test_basic_auth_success() {
        let auth = BasicAuth {
            credentials: "user:pass".to_string(),
        };
        let encoded = general_purpose::STANDARD_NO_PAD.encode("user:pass");
        let mut headers = HeaderMap::new();
        headers.insert(
            "Authorization",
            HeaderValue::from_str(&format!("Basic {encoded}")).unwrap(),
        );

        assert!(auth.check_authentication(&headers).unwrap());
    }

    #[test]
    fn test_basic_auth_missing_header() {
        let auth = BasicAuth {
            credentials: "user:pass".to_string(),
        };
        let headers = HeaderMap::new();

        assert!(!auth.check_authentication(&headers).unwrap());
    }

    #[test]
    fn test_basic_auth_invalid_format() {
        let auth = BasicAuth {
            credentials: "user:pass".to_string(),
        };
        let mut headers = HeaderMap::new();
        headers.insert("Authorization", HeaderValue::from_static("Bearer token"));

        assert!(!auth.check_authentication(&headers).unwrap());
    }

    #[test]
    fn test_basic_auth_wrong_credentials() {
        let auth = BasicAuth {
            credentials: "user:wrong".to_string(),
        };
        let encoded = general_purpose::STANDARD_NO_PAD.encode("user:pass");
        let mut headers = HeaderMap::new();
        headers.insert(
            "Authorization",
            HeaderValue::from_str(&format!("Basic {encoded}")).unwrap(),
        );

        assert!(!auth.check_authentication(&headers).unwrap());
    }

    #[test]
    fn test_api_key_auth_success() {
        let auth = ApiKeyAuth {
            key: "X-API-KEY".to_string(),
            value: "secret".to_string(),
        };
        let mut headers = HeaderMap::new();
        headers.insert("X-API-KEY", HeaderValue::from_static("secret"));

        assert!(auth.check_authentication(&headers).unwrap());
    }

    #[test]
    fn test_api_key_auth_missing_header() {
        let auth = ApiKeyAuth {
            key: "X-API-KEY".to_string(),
            value: "secret".to_string(),
        };
        let headers = HeaderMap::new();

        assert!(!auth.check_authentication(&headers).unwrap());
    }

    #[test]
    fn test_api_key_auth_wrong_value() {
        let auth = ApiKeyAuth {
            key: "X-API-KEY".to_string(),
            value: "secret".to_string(),
        };
        let mut headers = HeaderMap::new();
        headers.insert("X-API-KEY", HeaderValue::from_static("wrong"));

        assert!(!auth.check_authentication(&headers).unwrap());
    }

    #[test]
    fn test_api_key_auth_case_sensitive() {
        let auth = ApiKeyAuth {
            key: "X-API-KEY".to_string(),
            value: "Secret".to_string(),
        };
        let mut headers = HeaderMap::new();
        headers.insert("X-API-KEY", HeaderValue::from_static("secret"));

        assert!(!auth.check_authentication(&headers).unwrap());
    }

    #[test]
    fn test_authentication_enum_basic() {
        let auth = Authentication::Basic(BasicAuth {
            credentials: "admin:admin".to_string(),
        });
        let encoded = general_purpose::STANDARD_NO_PAD.encode("admin:admin");
        let mut headers = HeaderMap::new();
        headers.insert(
            "Authorization",
            HeaderValue::from_str(&format!("Basic {encoded}")).unwrap(),
        );

        assert!(auth.check_authentication(&headers).unwrap());
    }

    #[test]
    fn test_authentication_enum_api_key() {
        let auth = Authentication::ApiKey(ApiKeyAuth {
            key: "Authorization".to_string(),
            value: "Bearer token".to_string(),
        });
        let mut headers = HeaderMap::new();
        headers.insert("Authorization", HeaderValue::from_static("Bearer token"));

        assert!(auth.check_authentication(&headers).unwrap());
    }

    #[test]
    fn test_invalid_header_value() {
        let auth = BasicAuth {
            credentials: "user:pass".to_string(),
        };
        let mut headers = HeaderMap::new();
        let invalid_value = vec![0xff, 0xff, 0xff];
        headers.insert(
            "Authorization",
            HeaderValue::from_bytes(&invalid_value).unwrap(),
        );

        let result = auth.check_authentication(&headers);
        assert!(result.is_err());
    }
    use chrono::Duration;
    use chrono::Utc;
    use jsonwebtoken::{EncodingKey, Header, encode};
    use serde_json::json;

    fn create_test_jwt(
        secret: &str,
        claims_json: &serde_json::Value,
        algorithm: Algorithm,
    ) -> String {
        let header = Header::new(algorithm);
        encode(
            &header,
            &claims_json,
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap()
    }

    #[test]
    fn test_jwt_auth_success() {
        let secret = "my_super_secret".to_string();
        let auth = JwtAuth {
            secret: secret.clone(),
            ..Default::default()
        };
        let exp = (Utc::now() + Duration::hours(1)).timestamp();
        let claims = json!({ "sub": "user123", "exp": exp });
        let token = create_test_jwt(&secret, &claims, Algorithm::HS256);

        let mut headers = HeaderMap::new();
        headers.insert(
            "Authorization",
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );

        assert!(auth.check_authentication(&headers).unwrap());
    }

    #[test]
    fn test_jwt_auth_missing_header() {
        let auth = JwtAuth {
            secret: "secret".to_string(),
            ..Default::default()
        };
        let headers = HeaderMap::new();

        assert!(!auth.check_authentication(&headers).unwrap());
    }

    #[test]
    fn test_jwt_auth_invalid_format_no_bearer() {
        let auth = JwtAuth {
            secret: "secret".to_string(),
            ..Default::default()
        };
        let mut headers = HeaderMap::new();
        headers.insert("Authorization", HeaderValue::from_static("some_token"));

        assert!(!auth.check_authentication(&headers).unwrap());
    }

    #[test]
    fn test_jwt_auth_wrong_secret() {
        let encoding_secret = "correct_secret".to_string();
        let decoding_secret = "wrong_secret".to_string();
        let auth = JwtAuth {
            secret: decoding_secret,
            ..Default::default()
        };
        let exp = (Utc::now() + Duration::hours(1)).timestamp();
        let claims = json!({ "sub": "user123", "exp": exp });
        let token = create_test_jwt(&encoding_secret, &claims, Algorithm::HS256);

        let mut headers = HeaderMap::new();
        headers.insert(
            "Authorization",
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        assert!(!auth.check_authentication(&headers).unwrap());
    }

    #[test]
    fn test_jwt_auth_expired_token() {
        let secret = "my_secret".to_string();
        let auth = JwtAuth {
            secret: secret.clone(),
            ..Default::default()
        };
        let exp = (Utc::now() - Duration::hours(1)).timestamp();
        let claims = json!({ "sub": "user123", "exp": exp });
        let token = create_test_jwt(&secret, &claims, Algorithm::HS256);

        let mut headers = HeaderMap::new();
        headers.insert(
            "Authorization",
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );

        assert!(!auth.check_authentication(&headers).unwrap());
    }

    #[test]
    fn test_jwt_auth_with_issuer_success() {
        let secret = "my_secret".to_string();
        let issuer = "my_app".to_string();
        let auth = JwtAuth {
            secret: secret.clone(),
            issuer: Some(issuer.clone()),
            ..Default::default()
        };

        let exp = (Utc::now() + Duration::hours(1)).timestamp();
        let claims = json!({ "sub": "user123", "exp": exp, "iss": issuer });
        let token = create_test_jwt(&secret, &claims, Algorithm::HS256);

        let mut headers = HeaderMap::new();
        headers.insert(
            "Authorization",
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );

        assert!(auth.check_authentication(&headers).unwrap());
    }

    #[test]
    fn test_jwt_auth_with_wrong_issuer() {
        let secret = "my_secret".to_string();
        let auth = JwtAuth {
            secret: secret.clone(),
            issuer: Some("expected_issuer".to_string()),
            ..Default::default()
        };
        let exp = (Utc::now() + Duration::hours(1)).timestamp();
        let claims = json!({ "sub": "user123", "exp": exp, "iss": "wrong_issuer" });
        let token = create_test_jwt(&secret, &claims, Algorithm::HS256);

        let mut headers = HeaderMap::new();
        headers.insert(
            "Authorization",
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );

        assert!(!auth.check_authentication(&headers).unwrap());
    }

    #[test]
    fn test_jwt_auth_with_audience_success() {
        let secret = "my_secret".to_string();
        let audience = "my_audience".to_string();
        let auth = JwtAuth {
            secret: secret.clone(),
            audience: Some(audience.clone()),
            ..Default::default()
        };
        let exp = (Utc::now() + Duration::hours(1)).timestamp();
        let claims = json!({ "sub": "user123", "exp": exp, "aud": audience });
        let token = create_test_jwt(&secret, &claims, Algorithm::HS256);

        let mut headers = HeaderMap::new();
        headers.insert(
            "Authorization",
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );

        assert!(auth.check_authentication(&headers).unwrap());
    }

    #[test]
    fn test_authentication_enum_jwt() {
        let secret = "my_super_secret".to_string();
        let auth = Authentication::Jwt(JwtAuth {
            secret: secret.clone(),
            ..Default::default()
        });
        let exp = (Utc::now() + Duration::hours(1)).timestamp();
        let claims = json!({ "sub": "user123", "exp": exp });
        let token = create_test_jwt(&secret, &claims, Algorithm::HS256);

        let mut headers = HeaderMap::new();
        headers.insert(
            "Authorization",
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );

        assert!(auth.check_authentication(&headers).unwrap());
    }
}
