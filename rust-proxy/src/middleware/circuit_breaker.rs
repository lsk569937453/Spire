use crate::middleware::middlewares::CheckResult;
use crate::middleware::middlewares::Denial;
use crate::middleware::middlewares::Middleware;
use crate::utils::duration_urils::human_duration;
use crate::vojo::app_error::AppError;
use bytes::Bytes;
use http::HeaderMap;
use http::HeaderValue;
use http::Response;
use http::StatusCode;
use http::header;
use http_body_util::combinators::BoxBody;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;
#[derive(Debug, Clone, PartialEq)]
enum State {
    Closed {
        failures: u64,
        total_requests: u64,
        consecutive_failures: u32,
    },
    Open {
        opens_at: Instant,
    },
    HalfOpen {
        success_probes: u32,
        total_probes: u32,
    },
}
impl Default for State {
    fn default() -> Self {
        State::Closed {
            failures: 0,
            total_requests: 0,
            consecutive_failures: 0,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CircuitBreaker {
    #[serde(rename = "failure_threshold")]
    pub failure_rate_threshold: f64,

    #[serde(rename = "consecutive_failures")]
    pub consecutive_failure_threshold: u32,

    #[serde(rename = "cooldown", with = "human_duration")]
    pub open_duration: Duration,

    #[serde(rename = "half_open_requests")]
    pub half_open_max_requests: u32,

    #[serde(rename = "request_volume_threshold")]
    pub min_requests_for_rate_calculation: u64,
    #[serde(skip)]
    state: State,
}
impl Middleware for Arc<Mutex<CircuitBreaker>> {
    fn check_request(
        &self,
        _peer_addr: &SocketAddr,
        _headers: Option<&HeaderMap<HeaderValue>>,
    ) -> Result<CheckResult, AppError> {
        let mut lock = self.lock()?;
        let is_allowed = lock.is_call_allowed();
        if !is_allowed {
            debug!(
                "[CircuitBreaker] Request denied,the info is {:?}",
                lock.state_info()
            );
            let mut headers = HeaderMap::new();

            headers.insert(header::RETRY_AFTER, HeaderValue::from_static("30"));

            let denial = Denial {
                status: StatusCode::SERVICE_UNAVAILABLE,
                headers,
                body: "Service unavailable".to_string(),
            };
            Ok(CheckResult::Denied(denial))
        } else {
            Ok(CheckResult::Allowed)
        }
    }

    fn record_outcome(
        &self,
        response_result: &Result<Response<BoxBody<Bytes, AppError>>, AppError>,
    ) {
        let mut lock = match self.lock() {
            Ok(l) => l,
            Err(_) => return,
        };

        match response_result {
            Ok(response) if response.status().is_success() => {
                lock.record_success();
            }
            _ => {
                lock.record_failure();
            }
        }
    }
}
impl CircuitBreaker {
    pub fn is_call_allowed(&mut self) -> bool {
        match self.state {
            State::Closed { .. } => true,
            State::Open { opens_at } => {
                if Instant::now() >= opens_at {
                    debug!("[CircuitBreaker] Open -> HalfOpen");
                    self.state = State::HalfOpen {
                        success_probes: 0,
                        total_probes: 0,
                    };
                    true
                } else {
                    false
                }
            }
            State::HalfOpen { total_probes, .. } => total_probes < self.half_open_max_requests,
        }
    }

    pub fn record_success(&mut self) {
        match self.state {
            State::Closed {
                ref mut total_requests,
                ref mut consecutive_failures,
                ..
            } => {
                *total_requests += 1;
                *consecutive_failures = 0;
            }
            State::HalfOpen {
                ref mut success_probes,
                ref mut total_probes,
            } => {
                *success_probes += 1;
                *total_probes += 1;

                debug!("[CircuitBreaker] HalfOpen -> Closed (Success Probe)");
                self.reset_to_closed();
            }
            State::Open { .. } => {}
        }
    }

    pub fn record_failure(&mut self) {
        match self.state {
            State::Closed {
                ref mut failures,
                ref mut total_requests,
                ref mut consecutive_failures,
            } => {
                *failures += 1;
                *total_requests += 1;
                *consecutive_failures += 1;

                if *consecutive_failures >= self.consecutive_failure_threshold {
                    error!("[CircuitBreaker] Closed -> Open (Consecutive Failures)");
                    self.trip();
                    return;
                }

                if *total_requests >= self.min_requests_for_rate_calculation {
                    let current_failure_rate = *failures as f64 / *total_requests as f64;
                    if current_failure_rate >= self.failure_rate_threshold {
                        debug!("[CircuitBreaker] Closed -> Open (Failure Rate)");
                        self.trip();
                    }
                }
            }
            State::HalfOpen {
                ref mut total_probes,
                ..
            } => {
                *total_probes += 1;
                debug!("[CircuitBreaker] HalfOpen -> Open (Failed Probe)");
                self.trip();
            }
            State::Open { .. } => {}
        }
    }

    fn trip(&mut self) {
        self.state = State::Open {
            opens_at: Instant::now() + self.open_duration,
        };
    }

    fn reset_to_closed(&mut self) {
        self.state = State::Closed {
            failures: 0,
            total_requests: 0,
            consecutive_failures: 0,
        };
    }

    pub fn state_info(&self) -> String {
        format!("{:?}", self.state)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn default_breaker() -> CircuitBreaker {
        CircuitBreaker {
            failure_rate_threshold: 0.5,
            consecutive_failure_threshold: 5,
            open_duration: Duration::from_millis(100),
            half_open_max_requests: 2,
            min_requests_for_rate_calculation: 10,
            state: State::default(),
        }
    }

    #[test]
    fn initial_state_is_closed_and_allows_requests() {
        let mut cb = default_breaker();
        assert_eq!(
            cb.state,
            State::Closed {
                failures: 0,
                total_requests: 0,
                consecutive_failures: 0,
            }
        );
        assert!(cb.is_call_allowed());
    }

    #[test]
    fn trip_on_consecutive_failures() {
        let mut cb = default_breaker();
        cb.consecutive_failure_threshold = 3;

        for _ in 0..cb.consecutive_failure_threshold {
            assert!(cb.is_call_allowed());
            cb.record_failure();
        }

        assert!(!matches!(cb.state, State::Closed { .. }));
        assert!(matches!(cb.state, State::Open { .. }));
        assert!(!cb.is_call_allowed());
    }

    #[test]
    fn trip_on_failure_rate_threshold_corrected() {
        let mut cb = default_breaker();
        cb.min_requests_for_rate_calculation = 10;
        cb.failure_rate_threshold = 0.5; // Trip when failure rate is >= 0.5

        for _ in 0..4 {
            cb.record_failure();
        }
        for _ in 0..5 {
            cb.record_success();
        }
        if let State::Closed {
            failures,
            total_requests,
            ..
        } = cb.state
        {
            assert_eq!(total_requests, 9);
            assert_eq!(failures, 4);
        } else {
            panic!(
                "The circuit breaker should be in the Closed state before the threshold is triggered"
            );
        }
        assert!(
            cb.is_call_allowed(),
            "Should not trip when failure rate is 4/9 (≈0.44)"
        );
        cb.record_failure();
        assert!(
            !cb.is_call_allowed(),
            "Should trip after 10 requests with a 50% failure rate"
        );

        assert!(
            matches!(cb.state, State::Open { .. }),
            "State should be Open"
        );
    }
    #[test]
    fn success_resets_consecutive_failures() {
        let mut cb = default_breaker();
        cb.consecutive_failure_threshold = 5;

        for _ in 0..cb.consecutive_failure_threshold - 1 {
            cb.record_failure();
        }

        cb.record_success();

        if let State::Closed {
            consecutive_failures,
            ..
        } = cb.state
        {
            assert_eq!(consecutive_failures, 0);
        } else {
            panic!("State should be Closed");
        }

        cb.record_failure();
        assert!(cb.is_call_allowed());
        assert!(matches!(cb.state, State::Closed { .. }));
    }

    #[test]
    fn open_to_half_open_after_duration() {
        let mut cb = default_breaker();
        cb.open_duration = Duration::from_millis(50);
        cb.trip();

        assert!(!cb.is_call_allowed());
        assert!(matches!(cb.state, State::Open { .. }));

        thread::sleep(cb.open_duration);

        assert!(cb.is_call_allowed());
        assert!(matches!(cb.state, State::HalfOpen { .. }));
    }

    #[test]
    fn half_open_success_probe_closes_breaker() {
        let mut cb = default_breaker();
        cb.state = State::HalfOpen {
            success_probes: 0,
            total_probes: 0,
        };

        assert!(cb.is_call_allowed());
        cb.record_success();

        assert!(matches!(cb.state, State::Closed { .. }));
        assert!(cb.is_call_allowed());
    }

    #[test]
    fn half_open_failed_probe_opens_breaker() {
        let mut cb = default_breaker();
        cb.state = State::HalfOpen {
            success_probes: 0,
            total_probes: 0,
        };
        cb.open_duration = Duration::from_millis(50);

        assert!(cb.is_call_allowed());
        cb.record_failure();

        assert!(matches!(cb.state, State::Open { .. }));
        assert!(!cb.is_call_allowed());
    }

    #[test]
    fn half_open_allows_limited_probes() {
        let mut cb = default_breaker();
        cb.half_open_max_requests = 3;
        cb.state = State::HalfOpen {
            success_probes: 0,
            total_probes: 0,
        };

        for _ in 0..cb.half_open_max_requests - 1 {
            assert!(cb.is_call_allowed(), "Should allow probe");

            if let &mut State::HalfOpen {
                ref mut total_probes,
                ..
            } = &mut cb.state
            {
                *total_probes += 1;
            }
        }

        assert!(cb.is_call_allowed(), "Should allow the last probe");
        if let &mut State::HalfOpen {
            ref mut total_probes,
            ..
        } = &mut cb.state
        {
            *total_probes += 1;
        }

        assert!(!cb.is_call_allowed(), "Should deny after max probes");
    }
}
