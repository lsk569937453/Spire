use crate::AppError;
use bytes::Bytes;
use http::Response;
use http_body_util::combinators::BoxBody;
use serde::Deserialize;
use serde::Serialize;
use std::convert::Infallible;
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Headers {
    StaticSource(StaticResourceHeaders),
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StaticResourceHeaders {
    expires: i32,
    extensions: Vec<String>,
}
impl Headers {
    pub fn handle_before_response(
        &self,
        response: &mut Response<BoxBody<Bytes, Infallible>>,
    ) -> Result<(), AppError> {
        Ok(())
    }
}
