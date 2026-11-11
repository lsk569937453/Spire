use tokio::io;

use crate::proxy::http1::http_client::HttpClients;
use crate::vojo::app_error::AppError;
use bytes::Bytes;
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::upgrade::OnUpgrade;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::io::AsyncWriteExt;

use crate::proxy::proxy_trait::HandlingResult;
async fn proxy_websocket_connection(
    client_upgrade_fut: OnUpgrade,
    upstream_upgrade_fut: OnUpgrade,
) {
    match tokio::try_join!(client_upgrade_fut, upstream_upgrade_fut) {
        Ok((client_upgraded, upstream_upgraded)) => {
            let client_io = TokioIo::new(client_upgraded);
            let upstream_io = TokioIo::new(upstream_upgraded);

            let (mut client_reader, mut client_writer) = io::split(client_io);
            let (mut upstream_reader, mut upstream_writer) = io::split(upstream_io);

            let client_to_upstream = async {
                io::copy(&mut client_reader, &mut upstream_writer).await?;
                upstream_writer.shutdown().await
            };

            let upstream_to_client = async {
                io::copy(&mut upstream_reader, &mut client_writer).await?;
                client_writer.shutdown().await
            };

            if let Err(e) = tokio::try_join!(client_to_upstream, upstream_to_client) {
                warn!("Error during WebSocket data proxying: {}", e);
            }
            debug!("WebSocket proxy connection closed successfully.");
        }
        Err(e) => {
            error!("WebSocket upgrade failed: {}", e);
        }
    }
}

pub async fn server_upgrade<B>(
    req: Request<B>,
    check_result: HandlingResult,
    http_client: HttpClients,
) -> Result<Response<BoxBody<Bytes, AppError>>, AppError>
where
    B: http_body::Body<Data = Bytes> + Send + 'static,
    B::Error: Into<AppError>,
{
    debug!("Attempting to upgrade request: {:?}", req.headers());

    if !req.headers().contains_key(hyper::header::UPGRADE) {
        let mut res = Response::new(Full::new(Bytes::new()).map_err(AppError::from).boxed());
        *res.status_mut() = StatusCode::BAD_REQUEST;
        return Ok(res);
    }
    let headers_clone = req.headers().clone(); // 假设 HandlingResult 已经包含了头信息
    let method_clone = req.method().clone(); // 假设 HandlingResult 包含了方法

    let client_upgrade_fut = hyper::upgrade::on(req);

    let request_path = check_result.request_path.clone();

    let mut upstream_req = Request::builder()
        .method(method_clone)
        .uri(request_path.clone())
        .body(Full::new(Bytes::new()).map_err(AppError::from).boxed())?;
    *upstream_req.headers_mut() = headers_clone.clone();

    debug!("Forwarding upgrade request to upstream: {:?}", upstream_req);

    // Respect the route's timeout instead of a hardcoded value.
    let timeout_ms = check_result.timeout;
    let request_future = if upstream_req.uri().to_string().starts_with("https") {
        http_client.request_https(upstream_req, timeout_ms)
    } else {
        http_client.request_http(upstream_req, timeout_ms)
    };

    let upstream_res = match request_future.await {
        Ok(response) => response.map_err(AppError::from),
        Err(_) => Err(AppError(format!(
            "Request to upstream timed out, uri is {request_path}"
        ))),
    }?;

    if upstream_res.status() != StatusCode::SWITCHING_PROTOCOLS {
        warn!(
            "Upstream server rejected upgrade with status: {}",
            upstream_res.status()
        );
        let (parts, body) = upstream_res.into_parts();
        let boxed_body = body.map_err(AppError::from).boxed();
        return Ok(Response::from_parts(parts, boxed_body));
    }

    let response_headers_clone = upstream_res.headers().clone();
    let upstream_upgrade_fut = hyper::upgrade::on(upstream_res);

    tokio::spawn(async move {
        proxy_websocket_connection(client_upgrade_fut, upstream_upgrade_fut).await;
    });

    let mut client_res = Response::new(Full::new(Bytes::new()).map_err(AppError::from).boxed());
    *client_res.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    *client_res.headers_mut() = response_headers_clone; // 使用克隆的头信息

    debug!("Returning 101 Switching Protocols to client.");
    Ok(client_res)
}
