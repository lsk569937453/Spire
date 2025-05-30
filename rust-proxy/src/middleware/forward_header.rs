use crate::AppError;
use bytes::Bytes;
use http::Request;
use http_body_util::combinators::BoxBody;
use serde::Deserialize;
use serde::Serialize;
use std::convert::Infallible;
use std::net::SocketAddr;
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForwardHeader {}
impl ForwardHeader {
    pub fn handle_before_request(
        &self,
        peer_addr: SocketAddr,

        req: &mut Request<BoxBody<Bytes, Infallible>>,
    ) -> Result<(), AppError> {
        let client_ip = peer_addr.ip().to_string();
        req.headers_mut().insert("X-Real-IP", client_ip.parse()?);

        if let Some(existing_forwarded) = req.headers().get("X-Forwarded-For") {
            let new_value = format!("{}, {}", existing_forwarded.to_str()?, client_ip);
            req.headers_mut()
                .insert("X-Forwarded-For", new_value.parse()?);
        } else {
            req.headers_mut()
                .insert("X-Forwarded-For", client_ip.parse()?);
        }

        Ok(())
    }
}
