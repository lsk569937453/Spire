//! Response-body wrappers that keep an upstream request *bounded and
//! accounted* until the last byte has streamed to the client.
//!
//! `TimedBody` enforces an inter-frame idle timeout: the route timeout
//! already bounds connect + send + time-to-first-byte, but without this the
//! body phase is unbounded — a downstream that answers headers instantly and
//! then drips one byte per second holds the task and the connection forever.
//! There is deliberately no whole-body deadline: legitimate long streams
//! (SSE, large downloads) keep making progress; only silence is punished.
//!
//! `PermitBody` holds the concurrency-limiter permit (and the in-flight
//! gauge, plus the body-phase latency histogram) until the body finishes or
//! is dropped. Releasing the permit when headers arrive would bound almost
//! nothing.

use crate::monitor::prometheus_exporter::metrics;
use crate::proxy::concurrency::UpstreamPermit;
use crate::vojo::app_error::AppError;
use bytes::Bytes;
use http_body::Body;
use http_body::Frame;
use http_body::SizeHint;
use http_body_util::combinators::BoxBody;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;
use std::time::Duration;
use std::time::Instant;
use tokio::time::Sleep;

/// Labels shared by every metrics call a wrapper makes.
#[derive(Clone)]
pub struct UpstreamLabels {
    pub mapping_key: String,
    pub route: String,
    pub endpoint: String,
}

pub struct TimedBody {
    inner: BoxBody<Bytes, AppError>,
    idle_timeout: Duration,
    /// Idle watchdog. Armed (re-created) before polling `inner` while no
    /// frame is in flight; cleared on every frame so the timer measures the
    /// gap between frames, not total stream age.
    watchdog: Option<Pin<Box<Sleep>>>,
    labels: UpstreamLabels,
}

impl TimedBody {
    pub fn new(
        inner: BoxBody<Bytes, AppError>,
        idle_timeout: Duration,
        labels: UpstreamLabels,
    ) -> Self {
        Self {
            inner,
            idle_timeout,
            watchdog: None,
            labels,
        }
    }

    fn poll_inner(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, AppError>>> {
        let this = &mut *self;
        let deadline = tokio::time::Instant::now() + this.idle_timeout;
        let watchdog = this
            .watchdog
            .get_or_insert_with(|| Box::pin(tokio::time::sleep_until(deadline)));

        match Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                // Progress: restart the idle measurement on the next frame.
                this.watchdog = None;
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(other) => Poll::Ready(other),
            Poll::Pending => match watchdog.as_mut().poll(cx) {
                // Downstream went silent mid-body: count it like a timeout
                // and cut the stream. Headers are already on the wire, so
                // the client sees a truncated response — that is the point.
                Poll::Ready(()) => {
                    error!(
                        "[UpstreamBody] idle timeout after {:?} waiting for the next body frame from {} (route {})",
                        this.idle_timeout, this.labels.endpoint, this.labels.route
                    );
                    metrics::record_upstream_timeout(
                        &this.labels.mapping_key,
                        &this.labels.route,
                    );
                    Poll::Ready(Some(Err(AppError(
                        "upstream body idle timeout exceeded".to_string(),
                    ))))
                }
                Poll::Pending => Poll::Pending,
            },
        }
    }
}

impl http_body::Body for TimedBody {
    type Data = Bytes;
    type Error = AppError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        Self::poll_inner(self, cx)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

/// Releases the concurrency permit and records body-phase metrics exactly
/// once, whether the body completes normally, errors out, or is dropped
/// because the client went away.
pub struct PermitBody {
    inner: BoxBody<Bytes, AppError>,
    release: Option<Release>,
}

struct Release {
    permit: UpstreamPermit,
    labels: UpstreamLabels,
    body_started: Instant,
}

impl PermitBody {
    /// `body_started` is the instant response headers arrived; the body-phase
    /// histogram covers headers → last frame.
    pub fn new(
        inner: BoxBody<Bytes, AppError>,
        permit: UpstreamPermit,
        labels: UpstreamLabels,
        body_started: Instant,
    ) -> Self {
        Self {
            inner,
            release: Some(Release {
                permit,
                labels,
                body_started,
            }),
        }
    }

    fn finish(&mut self) {
        if let Some(release) = self.release.take() {
            // Release the permit before recording metrics so a queued waiter
            // can start while we bookkeep.
            drop(release.permit);
            metrics::upstream_in_flight_delta(
                &release.labels.mapping_key,
                &release.labels.route,
                &release.labels.endpoint,
                -1,
            );
            metrics::observe_upstream_duration(
                &release.labels.mapping_key,
                &release.labels.route,
                &release.labels.endpoint,
                "body",
                release.body_started.elapsed().as_secs_f64(),
            );
        }
    }
}

impl http_body::Body for PermitBody {
    type Data = Bytes;
    type Error = AppError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        let poll = Pin::new(&mut this.inner).poll_frame(cx);
        if matches!(poll, Poll::Ready(None) | Poll::Ready(Some(Err(_)))) {
            this.finish();
        }
        poll
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

impl Drop for PermitBody {
    fn drop(&mut self) {
        self.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use http_body_util::Full;
    use std::sync::Arc;

    fn labels() -> UpstreamLabels {
        UpstreamLabels {
            mapping_key: "test".to_string(),
            route: "route-1".to_string(),
            endpoint: "http://127.0.0.1:1".to_string(),
        }
    }

    fn full_body(data: &'static str) -> BoxBody<Bytes, AppError> {
        Full::new(Bytes::from_static(data.as_bytes()))
            .map_err(|e| match e {})
            .boxed()
    }

    #[tokio::test]
    async fn timed_body_passes_frames_through() {
        let _ = crate::monitor::prometheus_exporter::metrics::init();
        let mut body = TimedBody::new(full_body("hello"), Duration::from_secs(60), labels());
        let frame = body.frame().await;
        assert!(frame.expect("frame expected").is_ok());
    }

    #[tokio::test]
    async fn timed_body_cuts_silent_stream() {
        let _ = crate::monitor::prometheus_exporter::metrics::init();
        // A body whose poll never resolves: simulates a downstream that sent
        // headers and then went silent.
        struct Stalled;
        impl http_body::Body for Stalled {
            type Data = Bytes;
            type Error = AppError;
            fn poll_frame(
                self: Pin<&mut Self>,
                _: &mut Context<'_>,
            ) -> Poll<Option<Result<Frame<Bytes>, AppError>>> {
                Poll::Pending
            }
        }
        let mut body = TimedBody::new(Stalled.boxed(), Duration::from_millis(20), labels());
        let frame = body.frame().await;
        let err = frame
            .expect("must resolve via watchdog")
            .expect_err("must be an error");
        assert!(err.0.contains("idle timeout"));
    }

    #[tokio::test]
    async fn permit_body_releases_exactly_once() {
        use crate::proxy::concurrency::UpstreamPermit;
        let _ = crate::monitor::prometheus_exporter::metrics::init();
        let endpoint_sem = Arc::new(tokio::sync::Semaphore::new(1));
        let global_sem = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = UpstreamPermit::from_parts(
            endpoint_sem.clone().try_acquire_owned().unwrap(),
            global_sem.clone().try_acquire_owned().unwrap(),
        );
        assert_eq!(endpoint_sem.available_permits(), 0);
        assert_eq!(global_sem.available_permits(), 0);

        let mut body = PermitBody::new(full_body("abc"), permit, labels(), Instant::now());
        // First frame of a Full body is also its last; drop releases anyway.
        let frame = body.frame().await;
        assert!(frame.expect("frame expected").is_ok());
        drop(body);
        assert_eq!(endpoint_sem.available_permits(), 1, "permit must be back");
        assert_eq!(global_sem.available_permits(), 1, "permit must be back");
    }
}
