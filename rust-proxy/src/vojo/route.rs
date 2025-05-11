use super::app_config::LivenessConfig;
use super::app_config::LivenessStatus;
use super::app_error::AppError;
use crate::vojo::anomaly_detection::HttpAnomalyDetectionParam;

use core::fmt::Debug;
use http::HeaderMap;
use http::HeaderValue;
use rand::prelude::*;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{sleep, Duration};
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum LoadbalancerStrategy {
    PollRoute(PollRoute),
    HeaderBased(HeaderBasedRoute),
    #[serde(rename = "randomRoute")]
    Random(RandomRoute),
    WeightBased(WeightBasedRoute),
}
impl Default for LoadbalancerStrategy {
    fn default() -> Self {
        LoadbalancerStrategy::PollRoute(PollRoute::default())
    }
}
impl LoadbalancerStrategy {
    pub fn get_route(&mut self, headers: HeaderMap<HeaderValue>) -> Result<BaseRoute, AppError> {
        match self {
            LoadbalancerStrategy::PollRoute(poll_route) => poll_route.get_route(headers),

            LoadbalancerStrategy::HeaderBased(poll_route) => poll_route.get_route(headers),

            LoadbalancerStrategy::Random(poll_route) => poll_route.get_route(headers),

            LoadbalancerStrategy::WeightBased(poll_route) => poll_route.get_route(headers),
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub try_file: Option<String>,
    #[serde(skip_deserializing, skip_serializing_if = "Option::is_none")]
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
    #[serde(skip_deserializing, skip_serializing, default)]
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

    fn get_route(&mut self, headers: HeaderMap<HeaderValue>) -> Result<BaseRoute, AppError> {
        for item in self.routes.iter() {
            let headers_contais_key = headers.contains_key(item.header_key.clone());
            if !headers_contais_key {
                continue;
            }
            let header_value = headers.get(item.header_key.clone()).unwrap();
            let header_value_str = header_value.to_str().unwrap();
            match item.clone().header_value_mapping_type {
                HeaderValueMappingType::Regex(regex_str) => {
                    let re = Regex::new(&regex_str.value).unwrap();
                    let capture_option = re.captures(header_value_str);
                    if capture_option.is_none() {
                        continue;
                    } else {
                        return Ok(item.clone().base_route);
                    }
                }
                HeaderValueMappingType::Text(text_str) => {
                    if text_str.value == header_value_str {
                        return Ok(item.clone().base_route);
                    } else {
                        continue;
                    }
                }
                HeaderValueMappingType::Split(split_segment) => {
                    let split_set: HashSet<_> =
                        header_value_str.split(&split_segment.split_by).collect();
                    if split_set.is_empty() {
                        continue;
                    }
                    let mut flag = true;
                    for split_item in split_segment.split_list.iter() {
                        if !split_set.contains(split_item.clone().as_str()) {
                            flag = false;
                            break;
                        }
                    }
                    if flag {
                        return Ok(item.clone().base_route);
                    }
                }
            }
        }
        error!("Can not find the route!And siverWind has selected the first route!");

        let first = self.routes.first().unwrap().base_route.clone();
        Ok(first)
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

    fn get_route(&mut self, _headers: HeaderMap<HeaderValue>) -> Result<BaseRoute, AppError> {
        let mut rng = rand::thread_rng();
        let random_index = rng.gen_range(0..self.routes.len());
        Ok(self.routes[random_index].base_route.clone())
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct PollBaseRoute {
    pub base_route: BaseRoute,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct PollRoute {
    #[serde(skip_deserializing, skip_serializing)]
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

    fn get_route(&mut self, _headers: HeaderMap<HeaderValue>) -> Result<BaseRoute, AppError> {
        self.current_index += 1;
        if self.current_index >= self.routes.len() as i128 {
            self.current_index = 0;
        }
        debug!("current_index:{}", self.current_index);
        let route = self.routes[self.current_index as usize].base_route.clone();
        Ok(route)
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

    fn get_route(&mut self, _headers: HeaderMap<HeaderValue>) -> Result<BaseRoute, AppError> {
        if self.routes.is_empty() {
            return Err(AppError(String::from("No routes available")));
        }
        let all_reached = self.routes.iter().all(|r| r.index >= r.weight);
        if all_reached {
            for route in &mut self.routes {
                route.index = 0;
            }
        }
        debug!("{:?}", self.routes);
        if let Some(route) = self.routes.iter_mut().find(|r| r.index < r.weight) {
            route.index += 1;
            Ok(route.base_route.clone())
        } else {
            Err(AppError(String::from("WeightRoute get route error")))
        }
    }
}
