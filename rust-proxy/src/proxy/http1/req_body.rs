//! The request body type sent to an upstream.
//!
//! hyper-util's legacy client is generic over exactly one concrete request
//! body type, but Spire forwards two kinds of request: the client's streaming
//! body (`Incoming` — the same `Bytes` slices hyper read off the socket) and an
//! empty body (health probes and WebSocket upgrade requests). A concrete enum
//! unifies them without a `BoxBody`: the hot path costs no allocation and no
//! per-frame trait-object dispatch, which is what the `Full`/`BoxBody` wrapper
//! used to add to every proxied request.
//!
//! The response body is always `hyper::body::Incoming` (hyper-util's
//! `ResponseFuture` is not generic over the request body type), so this enum is
//! only ever the *request* side.

use crate::vojo::app_error::AppError;
use bytes::Bytes;
use http_body::Body;
use http_body::Frame;
use http_body::SizeHint;
#[cfg(any(test, feature = "wasm"))]
use http_body_util::combinators::BoxBody;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

/// One request body type for every upstream call.
#[derive(Debug)]
pub enum ProxyRequestBody {
    /// The downstream client's body, streamed straight to the upstream.
    Incoming(hyper::body::Incoming),
    /// No request body (health checks, WebSocket upgrades).
    Empty,
    /// A body the proxy built itself: plugin HTTP calls and unit tests. Those
    /// are rare and already own their bytes, so the box is acceptable; the
    /// proxied hot path never takes this branch.
    #[cfg(any(test, feature = "wasm"))]
    Boxed(BoxBody<Bytes, AppError>),
}

impl Body for ProxyRequestBody {
    type Data = Bytes;
    type Error = AppError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        match self.get_mut() {
            ProxyRequestBody::Incoming(body) => match Pin::new(body).poll_frame(cx) {
                Poll::Ready(Some(Ok(frame))) => Poll::Ready(Some(Ok(frame))),
                // Translated here (cold path) so the body's error type stays
                // `AppError` for the rest of the proxy stack.
                Poll::Ready(Some(Err(err))) => Poll::Ready(Some(Err(AppError::from(err)))),
                Poll::Ready(None) => Poll::Ready(None),
                Poll::Pending => Poll::Pending,
            },
            #[cfg(any(test, feature = "wasm"))]
            ProxyRequestBody::Boxed(body) => match Pin::new(body).poll_frame(cx) {
                Poll::Ready(Some(Ok(frame))) => Poll::Ready(Some(Ok(frame))),
                Poll::Ready(Some(Err(err))) => Poll::Ready(Some(Err(err))),
                Poll::Ready(None) => Poll::Ready(None),
                Poll::Pending => Poll::Pending,
            },
            ProxyRequestBody::Empty => Poll::Ready(None),
        }
    }

    fn is_end_stream(&self) -> bool {
        match self {
            ProxyRequestBody::Incoming(body) => body.is_end_stream(),
            #[cfg(any(test, feature = "wasm"))]
            ProxyRequestBody::Boxed(body) => body.is_end_stream(),
            ProxyRequestBody::Empty => true,
        }
    }

    fn size_hint(&self) -> SizeHint {
        match self {
            ProxyRequestBody::Incoming(body) => body.size_hint(),
            #[cfg(any(test, feature = "wasm"))]
            ProxyRequestBody::Boxed(body) => body.size_hint(),
            // Same hint an empty `Full<Bytes>` produced, so hyper keeps writing
            // `content-length: 0` for probes and upgrades.
            ProxyRequestBody::Empty => SizeHint::with_exact(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    #[tokio::test]
    async fn empty_body_is_already_finished() {
        let mut body = ProxyRequestBody::Empty;
        assert!(body.is_end_stream());
        assert_eq!(body.size_hint().exact(), Some(0));
        assert!(body.frame().await.is_none());
    }

    /// Drives the `Incoming` arm the way the proxy does: a real hyper server
    /// connection hands its request body over unchanged and the bytes must come
    /// out intact (the proxy streams them to the upstream).
    #[tokio::test]
    async fn incoming_body_streams_the_request_payload() {
        use http_body_util::Full;
        use hyper::Request;
        use hyper::Response;
        use hyper::server::conn::http1;
        use hyper::service::service_fn;
        use hyper_util::rt::TokioIo;
        use std::convert::Infallible;
        use tokio::io::AsyncReadExt;
        use tokio::io::AsyncWriteExt;
        use tokio::net::TcpListener;
        use tokio::net::TcpStream;

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (body_tx, body_rx) = tokio::sync::oneshot::channel::<Vec<u8>>();
        let body_tx = std::sync::Mutex::new(Some(body_tx));

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let service = service_fn(move |req: Request<hyper::body::Incoming>| {
                let sender = body_tx.lock().ok().and_then(|mut slot| slot.take());
                async move {
                    let body = ProxyRequestBody::Incoming(req.into_body());
                    let bytes = body
                        .collect()
                        .await
                        .expect("body must stream without error")
                        .to_bytes();
                    if let Some(sender) = sender {
                        let _ = sender.send(bytes.to_vec());
                    }
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from_static(b"ok"))))
                }
            });
            // Keep-alive: the connection stays open after the response, so the
            // task is simply left to finish when the client drops its socket.
            let _ = http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await;
        });

        let mut client = TcpStream::connect(addr).await.expect("connect");
        client
            .write_all(b"POST /echo HTTP/1.1\r\nHost: spire\r\nContent-Length: 5\r\n\r\nhello")
            .await
            .expect("write request");
        let mut response = [0u8; 64];
        let _ = client.read(&mut response).await;

        let received = body_rx.await.expect("handler must report the body");
        assert_eq!(received, b"hello");
    }
}
