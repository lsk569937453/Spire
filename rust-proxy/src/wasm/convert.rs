//! Conversions between the gateway's `http` types and the WIT records.
//!
//! This is the only place where the two worlds touch, and it is deliberately
//! boring: copy in, validate, copy out.

use http::HeaderMap;
use http::HeaderName;
use http::HeaderValue;
use http::StatusCode;

use crate::middleware::middlewares::Denial;
use crate::wasm::bindings::spire::filter::types as wit;

/// Borrowed header map -> owned WIT list.
///
/// Names are emitted lowercased (the WIT promises that) and values that are not
/// valid UTF-8 are skipped rather than dropped silently: HTTP header values are
/// `Latin-1`-ish bytes in the wild, but the component model has only `string`,
/// so a non-UTF-8 value cannot cross the boundary at all.
pub fn headers_to_wit(headers: &HeaderMap) -> Vec<wit::Header> {
    let mut out = Vec::with_capacity(headers.len());
    for (name, value) in headers.iter() {
        let Ok(value) = value.to_str() else { continue };
        out.push(wit::Header {
            name: name.as_str().to_ascii_lowercase(),
            value: value.to_string(),
        });
    }
    out
}

/// Applies the plugin's mutations in order. Fails on anything the `http` crate
/// would reject, so a plugin cannot inject a malformed header into the
/// upstream request.
pub fn apply_header_ops(headers: &mut HeaderMap, ops: Vec<wit::HeaderOp>) -> Result<(), String> {
    for op in ops {
        match op {
            wit::HeaderOp::Set(header) | wit::HeaderOp::Add(header) => {
                let name = HeaderName::from_bytes(header.name.as_bytes())
                    .map_err(|e| format!("invalid header name `{}`: {e}", header.name))?;
                let value = HeaderValue::from_str(&header.value)
                    .map_err(|e| format!("invalid value for header `{}`: {e}", header.name))?;
                // `set` and `add` are both "put this value"; the difference in
                // the WIT is documentation for the guest, not behaviour: an
                // `HeaderMap` keeps one logical value per name and `insert`
                // replaces, which is what a gateway wants (never duplicate
                // `authorization` by accident).
                headers.insert(name, value);
            }
            wit::HeaderOp::Remove(name) => {
                if let Ok(name) = HeaderName::from_bytes(name.as_bytes()) {
                    headers.remove(name);
                }
            }
        }
    }
    Ok(())
}

/// A guest-generated response, bounded so a plugin cannot allocate an arbitrary
/// amount of gateway memory.
pub fn local_reply_to_denial(reply: wit::LocalReply, max_output: u64) -> Result<Denial, String> {
    if reply.body.len() as u64 > max_output {
        return Err(format!(
            "local reply body is {} bytes, over the {} byte limit",
            reply.body.len(),
            max_output
        ));
    }
    let status = StatusCode::from_u16(reply.status)
        .map_err(|e| format!("invalid local reply status {}: {e}", reply.status))?;
    let mut headers = HeaderMap::new();
    apply_header_ops(
        &mut headers,
        reply.headers.into_iter().map(wit::HeaderOp::Set).collect(),
    )?;
    let body =
        String::from_utf8(reply.body).map_err(|e| format!("local reply body is not UTF-8: {e}"))?;
    Ok(Denial {
        status,
        headers,
        body,
    })
}

/// `CEPT`-free request view: everything the guest may observe about the
/// inbound request, already resolved (client address behind a trusted proxy,
/// matched route id).
pub struct RequestView<'a> {
    pub method: &'a str,
    pub scheme: &'a str,
    pub authority: &'a str,
    pub path: &'a str,
    pub query: &'a str,
    pub headers: &'a HeaderMap,
    pub client_ip: &'a str,
    pub route_id: &'a str,
    pub listener_port: u32,
    pub request_id: &'a str,
}

pub fn request_to_wit(view: RequestView<'_>) -> wit::Request {
    wit::Request {
        method: view.method.to_string(),
        scheme: view.scheme.to_string(),
        authority: view.authority.to_string(),
        path: view.path.to_string(),
        query: view.query.to_string(),
        headers: headers_to_wit(view.headers),
        client_ip: view.client_ip.to_string(),
        route_id: view.route_id.to_string(),
        listener_port: view.listener_port,
        request_id: view.request_id.to_string(),
    }
}

/// The owned snapshot the pipeline captured for the response phase.
pub fn snapshot_to_wit(snapshot: &crate::middleware::middlewares::RequestSnapshot) -> wit::Request {
    wit::Request {
        method: snapshot.method.clone(),
        scheme: snapshot.scheme.clone(),
        authority: snapshot.authority.clone(),
        path: snapshot.path.clone(),
        query: snapshot.query.clone(),
        headers: headers_to_wit(&snapshot.headers),
        client_ip: snapshot.client_addr.ip().to_string(),
        route_id: snapshot.route_id.clone(),
        listener_port: snapshot.listener_port.max(0) as u32,
        request_id: snapshot.request_id.clone(),
    }
}

pub fn response_to_wit(status: StatusCode, headers: &HeaderMap) -> wit::Response {
    wit::Response {
        status: status.as_u16(),
        headers: headers_to_wit(headers),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowercases_names_and_round_trips() {
        let mut headers = HeaderMap::new();
        headers.insert("X-Test", HeaderValue::from_static("v"));
        let wit_headers = headers_to_wit(&headers);
        assert_eq!(wit_headers.len(), 1);
        assert_eq!(wit_headers[0].name, "x-test");
        assert_eq!(wit_headers[0].value, "v");
    }

    #[test]
    fn applies_ops_in_order() {
        let mut headers = HeaderMap::new();
        headers.insert("x-a", HeaderValue::from_static("1"));
        let ops = vec![
            wit::HeaderOp::Set(wit::Header {
                name: "x-a".into(),
                value: "2".into(),
            }),
            wit::HeaderOp::Remove("x-b".into()),
        ];
        apply_header_ops(&mut headers, ops).unwrap();
        assert_eq!(headers.get("x-a").unwrap(), "2");
    }

    #[test]
    fn rejects_a_bad_header_name() {
        let mut headers = HeaderMap::new();
        let ops = vec![wit::HeaderOp::Set(wit::Header {
            name: "bad header".into(),
            value: "x".into(),
        })];
        assert!(apply_header_ops(&mut headers, ops).is_err());
    }

    #[test]
    fn bounds_local_reply_size() {
        let reply = wit::LocalReply {
            status: 403,
            headers: vec![wit::Header {
                name: "content-type".into(),
                value: "text/plain".into(),
            }],
            body: b"nope".to_vec(),
        };
        let denial = local_reply_to_denial(reply.clone(), 16).unwrap();
        assert_eq!(denial.status, StatusCode::FORBIDDEN);
        assert_eq!(denial.body, "nope");

        let mut big = reply;
        big.body = vec![b'x'; 32];
        assert!(local_reply_to_denial(big, 16).is_err());
    }
}
