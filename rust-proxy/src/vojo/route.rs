use super::app_config::LivenessConfig;
use super::app_config::LivenessStatus;
use super::app_error::AppError;
use crate::vojo::anomaly_detection::HttpAnomalyDetectionParam;

use core::fmt::Debug;
use http::HeaderMap;
use http::HeaderValue;
use log::Level;
use rand::prelude::*;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{sleep, Duration};
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum LoadbalancerStrategy {
    PollRoute(PollRoute),
    HeaderBased(HeaderBasedRoute),
    Random(RandomRoute),
    WeightBased(WeightBasedRoute),
}
impl Default for LoadbalancerStrategy {
    fn default() -> Self {
        LoadbalancerStrategy::PollRoute(PollRoute::default())
    }
}
impl LoadbalancerStrategy {
    pub async fn get_route(
        &mut self,
        headers: HeaderMap<HeaderValue>,
    ) -> Result<BaseRoute, AppError> {
        match self {
            LoadbalancerStrategy::PollRoute(poll_route) => poll_route.get_route(headers).await,

            LoadbalancerStrategy::HeaderBased(poll_route) => poll_route.get_route(headers).await,

            LoadbalancerStrategy::Random(poll_route) => poll_route.get_route(headers).await,

            LoadbalancerStrategy::WeightBased(poll_route) => poll_route.get_route(headers).await,
        }
    }
    pub async fn get_all_route(&mut self) -> Result<Vec<BaseRoute>, AppError> {
        match self {
            LoadbalancerStrategy::PollRoute(poll_route) => poll_route.get_all_route().await,
            LoadbalancerStrategy::HeaderBased(poll_route) => poll_route.get_all_route().await,

            LoadbalancerStrategy::Random(poll_route) => poll_route.get_all_route().await,

            LoadbalancerStrategy::WeightBased(poll_route) => poll_route.get_all_route().await,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AnomalyDetectionStatus {
    pub consecutive_5xx: i32,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct BaseRoute {
    pub endpoint: String,
    pub try_file: Option<String>,
    #[serde(skip_deserializing)]
    pub is_alive: Option<bool>,
    #[serde(skip_serializing, skip_deserializing)]
    pub anomaly_detection_status: AnomalyDetectionStatus,
}

impl BaseRoute {
    async fn update_ok(&mut self, liveness_status_lock: LivenessStatus) -> bool {
        false
    }
    async fn update_fail(&self, liveness_status_lock: Arc<RwLock<LivenessStatus>>) -> bool {
        false
    }
    pub async fn update_health_check_status_with_ok(
        &self,
        liveness_status_lock: Arc<RwLock<LivenessStatus>>,
    ) -> bool {
        false
    }
    pub async fn update_health_check_status_with_fail(
        &self,
        liveness_status_lock: Arc<RwLock<LivenessStatus>>,
        liveness_config: LivenessConfig,
    ) -> bool {
        false
    }
    pub async fn trigger_http_anomaly_detection(
        &self,
        http_anomaly_detection_param: HttpAnomalyDetectionParam,
        liveness_status_lock: Arc<RwLock<LivenessStatus>>,
        is_5xx: bool,
        liveness_config: LivenessConfig,
    ) -> Result<(), AppError> {
        Ok(())
    }

    pub async fn wait_for_alive(
        is_alive_lock: Arc<RwLock<Option<bool>>>,
        wait_second: u64,
        liveness_status_lock: Arc<RwLock<LivenessStatus>>,
        anomaly_detection_status_lock: Arc<RwLock<AnomalyDetectionStatus>>,
    ) {
        sleep(Duration::from_secs(wait_second)).await;
        let mut is_alive_option = is_alive_lock.write().await;
        let mut liveness_status = liveness_status_lock.write().await;
        let mut anomaly_detection_status = anomaly_detection_status_lock.write().await;
        *is_alive_option = Some(true);
        liveness_status.current_liveness_count += 1;
        anomaly_detection_status.consecutive_5xx = 0;
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WeightRoute {
    pub base_route: BaseRoute,
    pub weight: i32,
    pub index: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SplitSegment {
    pub split_by: String,
    pub split_list: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SplitItem {
    pub header_key: String,
    pub header_value: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]

pub struct RegexMatch {
    pub value: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextMatch {
    pub value: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum HeaderValueMappingType {
    Regex(RegexMatch),
    Text(TextMatch),
    Split(SplitSegment),
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeaderRoute {
    pub base_route: BaseRoute,
    pub header_key: String,
    pub header_value_mapping_type: HeaderValueMappingType,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeaderBasedRoute {
    pub routes: Vec<HeaderRoute>,
}

impl HeaderBasedRoute {
    async fn get_all_route(&mut self) -> Result<Vec<BaseRoute>, AppError> {
        Ok(self
            .routes
            .iter()
            .map(|item| item.base_route.clone())
            .collect::<Vec<BaseRoute>>())
    }

    async fn get_route(&mut self, headers: HeaderMap<HeaderValue>) -> Result<BaseRoute, AppError> {
        Ok(BaseRoute::default())
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RandomBaseRoute {
    pub base_route: BaseRoute,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RandomRoute {
    pub routes: Vec<RandomBaseRoute>,
}

impl RandomRoute {
    async fn get_all_route(&mut self) -> Result<Vec<BaseRoute>, AppError> {
        Ok(self
            .routes
            .iter()
            .map(|item| item.base_route.clone())
            .collect::<Vec<BaseRoute>>())
    }

    async fn get_route(&mut self, _headers: HeaderMap<HeaderValue>) -> Result<BaseRoute, AppError> {
        Ok(BaseRoute::default())
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct PollBaseRoute {
    pub base_route: BaseRoute,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct PollRoute {
    pub current_index: i128,
    pub routes: Vec<PollBaseRoute>,
}
impl PollRoute {}

impl PollRoute {
    async fn get_all_route(&mut self) -> Result<Vec<BaseRoute>, AppError> {
        Ok(self
            .routes
            .iter_mut()
            .map(|item| item.base_route.clone())
            .collect::<Vec<BaseRoute>>())
    }

    async fn get_route(&mut self, _headers: HeaderMap<HeaderValue>) -> Result<BaseRoute, AppError> {
        Ok(BaseRoute::default())
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WeightBasedRoute {
    pub routes: Vec<WeightRoute>,
}

impl WeightBasedRoute {
    async fn get_all_route(&mut self) -> Result<Vec<BaseRoute>, AppError> {
        Ok(vec![])
    }

    async fn get_route(&mut self, _headers: HeaderMap<HeaderValue>) -> Result<BaseRoute, AppError> {
        Err(AppError(String::from("WeightRoute get route error")))
    }
}
