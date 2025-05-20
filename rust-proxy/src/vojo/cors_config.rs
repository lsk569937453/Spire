use regex::Regex;
use serde::Deserialize;
use serde::Serialize;

use super::app_error::AppError;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CorsConfig {
    pub allowed_origins: Vec<String>,
    pub allowed_methods: Vec<Method>,
    pub allowed_headers: Vec<HeaderName>,
    pub allow_credentials: bool,
    pub max_age: i32,
    pub options_passthrough: bool,
}
impl CorsConfig {
    pub fn validate_origin(&self, origin: &str) -> Result<bool, AppError> {
        // if self.allow_any_origin {
        //     return !self.allow_credentials;
        // }
        for allowed in &self.allowed_origins {
            if allowed == "*" {
                return Ok(self.allow_credentials);
            }
            let regex = Regex::new(allowed).map_err(|e| AppError(e.to_string()))?;
            if regex.is_match(origin) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Method {
    Get,
    Post,
    Put,
    Delete,
    Head,
    Options,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum HeaderName {
    AcessControlAllowOrigin,
    AccessControlAllowMethods,
    AccessControlAllowHeaders,
    AccessControlMaxAge,
    AccessControlAllowCredentials,
}
