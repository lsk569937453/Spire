use base64::{engine::general_purpose, Engine as _};
use core::fmt::Debug;
use dyn_clone::DynClone;
use http::HeaderMap;
use http::HeaderValue;

use serde::{Deserialize, Serialize};
use std::any::Any;

use super::app_error::AppError;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "PascalCase")]
pub enum Authentication {
    Basic(BasicAuth),
    ApiKey(ApiKeyAuth),
}

impl Authentication {
    pub fn check_authentication(
        &mut self,
        headers: HeaderMap<HeaderValue>,
    ) -> Result<bool, AppError> {
        match self {
            Authentication::Basic(auth) => auth.check_authentication(headers),
            Authentication::ApiKey(auth) => auth.check_authentication(headers),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct BasicAuth {
    pub credentials: String,
}

impl BasicAuth {
    fn check_authentication(&mut self, headers: HeaderMap<HeaderValue>) -> Result<bool, AppError> {
        // 原有实现逻辑
        if headers.is_empty() || !headers.contains_key("Authorization") {
            return Ok(false);
        }
        let value = headers
            .get("Authorization")
            .unwrap()
            .to_str()
            .map_err(|err| AppError(err.to_string()))?;
        let split_list: Vec<_> = value.split(' ').collect();
        if split_list.len() != 2 || split_list[0] != "Basic" {
            return Ok(false);
        }
        let encoded: String = general_purpose::STANDARD_NO_PAD.encode(&self.credentials);
        Ok(split_list[1] == encoded)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ApiKeyAuth {
    pub key: String,
    pub value: String,
}

impl ApiKeyAuth {
    fn check_authentication(&mut self, headers: HeaderMap<HeaderValue>) -> Result<bool, AppError> {
        // 原有实现逻辑
        if headers.is_empty() || !headers.contains_key(&self.key) {
            return Ok(false);
        }
        let header_value = headers
            .get(&self.key)
            .unwrap()
            .to_str()
            .map_err(|err| AppError(err.to_string()))?;
        Ok(header_value == self.value)
    }
}
