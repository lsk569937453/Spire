use serde::{Deserialize, Serialize};
use std::time::Duration;
use std::time::Instant;

/// 熔断器的内部状态
#[derive(Debug, Clone, PartialEq)]
enum State {
    /// 闭合状态：允许所有请求通过，并统计成功/失败次数。
    Closed {
        failures: u64,
        total_requests: u64,
        consecutive_failures: u32,
    },
    /// 断开状态：拒绝所有请求，直到指定时间点。
    Open {
        /// 熔断器将在此时间点之后切换到 HalfOpen 状态
        opens_at: Instant,
    },
    /// 半开状态：允许有限数量的探测请求通过，以确定服务是否恢复。
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
/// 熔断器主结构体
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CircuitBreaker {
    pub failure_rate_threshold: f64,
    pub consecutive_failure_threshold: u32,
    pub open_duration: Duration,
    pub half_open_max_requests: u32,
    pub min_requests_for_rate_calculation: u64,

    #[serde(skip)]
    state: State,
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

    /// 记录一次成功的请求
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
                        println!("[CircuitBreaker] Closed -> Open (Failure Rate)");
                        self.trip();
                    }
                }
            }
            State::HalfOpen {
                ref mut total_probes,
                ..
            } => {
                *total_probes += 1;
                println!("[CircuitBreaker] HalfOpen -> Open (Failed Probe)");
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
