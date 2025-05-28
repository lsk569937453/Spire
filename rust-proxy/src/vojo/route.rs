use crate::proxy::proxy_trait::RouterDestination;

use super::app_error::AppError;
use core::fmt::Debug;
use http::HeaderMap;
use http::HeaderValue;
use rand::prelude::*;
use regex::Regex;
use serde::de;
use serde::de::Visitor;
use serde::Deserializer;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Router {
    StaticFile(StaticFileRoute),
    #[serde(deserialize_with = "deserialize_loadbalancer")]
    Loadbalancer(LoadbalancerStrategy),
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]

pub struct StaticFileRoute {
    pub doc_root: String,
}
impl Default for Router {
    fn default() -> Self {
        Self::Loadbalancer(LoadbalancerStrategy::Poll(PollRoute::default()))
    }
}
impl Router {
    pub fn get_route(
        &mut self,
        headers: &HeaderMap<HeaderValue>,
    ) -> Result<RouterDestination, AppError> {
        match self {
            Router::StaticFile(s) => Ok(RouterDestination::File(s.clone())),
            Router::Loadbalancer(loadbalancer_strategy) => loadbalancer_strategy
                .get_route(headers)
                .map(RouterDestination::Http),
        }
    }
    pub async fn get_all_route(&mut self) -> Result<Vec<BaseRoute>, AppError> {
        match self {
            Router::StaticFile(_) => {
                Err(AppError("StaticFile router can not get route".to_string()))
            }
            Router::Loadbalancer(loadbalancer_strategy) => {
                loadbalancer_strategy.get_all_route().await
            }
        }
    }
    pub fn update_route_alive(
        &mut self,
        base_route: BaseRoute,
        is_alive: bool,
    ) -> Result<(), AppError> {
        match self {
            Router::StaticFile(_) => {
                Err(AppError("StaticFile router can not get route".to_string()))
            }
            Router::Loadbalancer(loadbalancer_strategy) => {
                loadbalancer_strategy.update_route_alive(base_route, is_alive)
            }
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum LoadbalancerStrategy {
    #[serde(rename = "Poll")]
    Poll(PollRoute),
    #[serde(rename = "HeaderBased")]
    HeaderBased(HeaderBasedRoute),
    #[serde(rename = "Random")]
    Random(RandomRoute),
    #[serde(rename = "WeightBased")]
    WeightBased(WeightBasedRoute),
}
pub fn deserialize_loadbalancer<'de, D>(deserializer: D) -> Result<LoadbalancerStrategy, D::Error>
where
    D: Deserializer<'de>,
{
    // 实现混合模式解析器
    struct StrategyVisitor;

    impl<'de> Visitor<'de> for StrategyVisitor {
        type Value = LoadbalancerStrategy;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("string, array or object")
        }

        // 处理字符串输入
        fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(LoadbalancerStrategy::Random(RandomRoute {
                routes: vec![RandomBaseRoute {
                    base_route: BaseRoute {
                        endpoint: v.to_string(),
                        is_alive: None,
                        anomaly_detection_status: AnomalyDetectionStatus::default(),
                    },
                }],
            }))
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: de::SeqAccess<'de>,
        {
            let mut backends = Vec::new();
            while let Some(s) = seq.next_element::<String>()? {
                backends.push(s);
            }
            Ok(LoadbalancerStrategy::Random(RandomRoute::new(backends)))
        }

        fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
        where
            A: de::MapAccess<'de>,
        {
            Deserialize::deserialize(de::value::MapAccessDeserializer::new(map))
        }
    }

    deserializer.deserialize_any(StrategyVisitor)
}

impl Default for LoadbalancerStrategy {
    fn default() -> Self {
        LoadbalancerStrategy::Poll(PollRoute::default())
    }
}
impl LoadbalancerStrategy {
    pub fn get_route(&mut self, headers: &HeaderMap<HeaderValue>) -> Result<BaseRoute, AppError> {
        match self {
            LoadbalancerStrategy::Poll(poll_route) => poll_route.get_route(headers),

            LoadbalancerStrategy::HeaderBased(poll_route) => poll_route.get_route(headers),

            LoadbalancerStrategy::Random(poll_route) => poll_route.get_route(headers),

            LoadbalancerStrategy::WeightBased(poll_route) => poll_route.get_route(headers),
        }
    }
    pub async fn get_all_route(&mut self) -> Result<Vec<BaseRoute>, AppError> {
        match self {
            LoadbalancerStrategy::Poll(poll_route) => poll_route.get_all_route().await,
            LoadbalancerStrategy::HeaderBased(poll_route) => poll_route.get_all_route().await,

            LoadbalancerStrategy::Random(poll_route) => poll_route.get_all_route().await,

            LoadbalancerStrategy::WeightBased(poll_route) => poll_route.get_all_route().await,
        }
    }
    pub fn update_route_alive(
        &mut self,
        base_route: BaseRoute,
        is_alive: bool,
    ) -> Result<(), AppError> {
        match self {
            LoadbalancerStrategy::Poll(poll_route) => {
                poll_route.update_route_alive(base_route, is_alive)
            }
            LoadbalancerStrategy::HeaderBased(poll_route) => {
                poll_route.update_route_alive(base_route, is_alive)
            }

            LoadbalancerStrategy::Random(poll_route) => {
                poll_route.update_route_alive(base_route, is_alive)
            }

            LoadbalancerStrategy::WeightBased(poll_route) => {
                poll_route.update_route_alive(base_route, is_alive)
            }
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
    #[serde(skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub is_alive: Option<bool>,
    #[serde(skip_serializing, skip_deserializing)]
    pub anomaly_detection_status: AnomalyDetectionStatus,
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

    fn get_route(&mut self, headers: &HeaderMap<HeaderValue>) -> Result<BaseRoute, AppError> {
        let has_unconfigured = self.routes.iter().any(|r| r.base_route.is_alive.is_none());
        debug!("has_unconfigured:{}", has_unconfigured);
        let routes = if has_unconfigured {
            self.routes.clone()
        } else {
            self.routes
                .iter()
                .filter(|r| r.base_route.is_alive == Some(true))
                .cloned()
                .collect()
        };
        for item in routes.iter() {
            let headers_contais_key = headers.contains_key(item.header_key.clone());
            if !headers_contais_key {
                continue;
            }
            let header_value = headers
                .get(item.header_key.clone())
                .ok_or("Can not find the headervalue")?;
            let header_value_str = header_value.to_str()?;
            match item.clone().header_value_mapping_type {
                HeaderValueMappingType::Regex(regex_str) => {
                    let re = Regex::new(&regex_str.value)?;
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
        error!("Can not find the route!And Spire has selected the first route!");

        let first = self
            .routes
            .first()
            .ok_or("The first item not found.")?
            .base_route
            .clone();
        Ok(first)
    }
    fn update_route_alive(
        &mut self,
        base_route: BaseRoute,
        is_alive: bool,
    ) -> Result<(), AppError> {
        for item in self.routes.iter_mut() {
            if item.base_route.endpoint == base_route.endpoint {
                item.base_route.is_alive = Some(is_alive);
            }
        }
        Ok(())
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
    pub fn new(backends: Vec<String>) -> Self {
        Self {
            routes: backends
                .iter()
                .map(|item| RandomBaseRoute {
                    base_route: BaseRoute {
                        endpoint: item.clone(),
                        is_alive: None,
                        anomaly_detection_status: AnomalyDetectionStatus::default(),
                    },
                })
                .collect::<Vec<RandomBaseRoute>>(),
        }
    }
    async fn get_all_route(&mut self) -> Result<Vec<BaseRoute>, AppError> {
        Ok(self
            .routes
            .iter()
            .map(|item| item.base_route.clone())
            .collect::<Vec<BaseRoute>>())
    }

    fn get_route(&mut self, _headers: &HeaderMap<HeaderValue>) -> Result<BaseRoute, AppError> {
        let has_unconfigured = self.routes.iter().any(|r| r.base_route.is_alive.is_none());

        if has_unconfigured {
            let mut rng = rand::rng();
            let random_index = rng.random_range(0..self.routes.len());
            Ok(self.routes[random_index].base_route.clone())
        } else {
            let alive_indices: Vec<usize> = self
                .routes
                .iter()
                .enumerate()
                .filter(|(_, r)| r.base_route.is_alive == Some(true))
                .map(|(i, _)| i)
                .collect();
            if alive_indices.is_empty() {
                debug!("All routes are dead, selecting a random route");
                let mut rng = rand::rng();
                let random_index = rng.random_range(0..self.routes.len());
                Ok(self.routes[random_index].base_route.clone())
            } else {
                let mut rng = rand::rng();
                let random_index = rng.random_range(0..alive_indices.len());
                Ok(self.routes[alive_indices[random_index]].base_route.clone())
            }
        }
    }
    fn update_route_alive(
        &mut self,
        base_route: BaseRoute,
        is_alive: bool,
    ) -> Result<(), AppError> {
        for item in self.routes.iter_mut() {
            if item.base_route.endpoint == base_route.endpoint {
                item.base_route.is_alive = Some(is_alive);
            }
        }
        Ok(())
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

    fn get_route(&mut self, _headers: &HeaderMap<HeaderValue>) -> Result<BaseRoute, AppError> {
        let has_unconfigured = self.routes.iter().any(|r| r.base_route.is_alive.is_none());
        if has_unconfigured {
            self.current_index += 1;
            if self.current_index >= self.routes.len() as i128 {
                self.current_index = 0;
            }
            debug!("current_index:{}", self.current_index);
            let route = self.routes[self.current_index as usize].base_route.clone();
            Ok(route)
        } else {
            let alive_indices: Vec<usize> = self
                .routes
                .iter()
                .enumerate()
                .filter(|(_, r)| r.base_route.is_alive == Some(true))
                .map(|(i, _)| i)
                .collect();
            if alive_indices.is_empty() {
                debug!("All routes are dead, selecting a random route");
                let mut rng = rand::rng();
                let random_index = rng.random_range(0..self.routes.len());
                Ok(self.routes[random_index].base_route.clone())
            } else {
                self.current_index += 1;
                if self.current_index >= alive_indices.len() as i128 {
                    self.current_index = 0;
                }
                let selected_index = alive_indices[self.current_index as usize];
                debug!(
                    "current_index:{} (alive index), selected_index: {}",
                    self.current_index, selected_index
                );
                let route = self.routes[selected_index].base_route.clone();
                Ok(route)
            }
        }
    }
    fn update_route_alive(
        &mut self,
        base_route: BaseRoute,
        is_alive: bool,
    ) -> Result<(), AppError> {
        for item in self.routes.iter_mut() {
            if item.base_route.endpoint == base_route.endpoint {
                item.base_route.is_alive = Some(is_alive);
            }
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WeightBasedRoute {
    pub routes: Vec<WeightRoute>,
}

impl WeightBasedRoute {
    async fn get_all_route(&mut self) -> Result<Vec<BaseRoute>, AppError> {
        Ok(self
            .routes
            .iter_mut()
            .map(|item| item.base_route.clone())
            .collect::<Vec<BaseRoute>>())
    }

    fn get_route(&mut self, _headers: &HeaderMap<HeaderValue>) -> Result<BaseRoute, AppError> {
        if self.routes.is_empty() {
            return Err(AppError(String::from("No routes available")));
        }

        let has_unconfigured = self.routes.iter().any(|r| r.base_route.is_alive.is_none());
        if has_unconfigured {
            let all_reached = self.routes.iter().all(|r| r.index >= r.weight);
            if all_reached {
                for route in &mut self.routes {
                    route.index = 0;
                }
            }
            if let Some(route) = self.routes.iter_mut().find(|r| r.index < r.weight) {
                route.index += 1;
                Ok(route.base_route.clone())
            } else {
                Err(AppError(String::from("WeightRoute get route error")))
            }
        } else {
            let alive_indices: Vec<usize> = self
                .routes
                .iter()
                .enumerate()
                .filter(|(_, r)| r.base_route.is_alive == Some(true))
                .map(|(i, _)| i)
                .collect();

            if !alive_indices.is_empty() {
                let all_reached = alive_indices
                    .iter()
                    .all(|&i| self.routes[i].index >= self.routes[i].weight);
                if all_reached {
                    for &i in &alive_indices {
                        self.routes[i].index = 0;
                    }
                }
                for &i in &alive_indices {
                    if self.routes[i].index < self.routes[i].weight {
                        self.routes[i].index += 1;
                        return Ok(self.routes[i].base_route.clone());
                    }
                }
                Err(AppError(String::from("WeightRoute get route error")))
            } else {
                let mut rng = rand::rng();
                let idx = rng.random_range(0..self.routes.len());
                self.routes[idx].index += 1;
                Ok(self.routes[idx].base_route.clone())
            }
        }
    }
    fn update_route_alive(
        &mut self,
        base_route: BaseRoute,
        is_alive: bool,
    ) -> Result<(), AppError> {
        for item in self.routes.iter_mut() {
            if item.base_route.endpoint == base_route.endpoint {
                item.base_route.is_alive = Some(is_alive);
            }
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    #[tokio::test]
    async fn test_poll_route() {
        let mut poll_route = PollRoute {
            routes: vec![
                PollBaseRoute {
                    base_route: BaseRoute {
                        endpoint: "server1".to_string(),
                        ..Default::default()
                    },
                },
                PollBaseRoute {
                    base_route: BaseRoute {
                        endpoint: "server2".to_string(),
                        ..Default::default()
                    },
                },
            ],
            ..Default::default()
        };

        assert_eq!(
            poll_route.get_route(&HeaderMap::new()).unwrap().endpoint,
            "server2"
        );
        assert_eq!(
            poll_route.get_route(&HeaderMap::new()).unwrap().endpoint,
            "server1"
        );
        assert_eq!(
            poll_route.get_route(&HeaderMap::new()).unwrap().endpoint,
            "server2"
        );
    }

    #[tokio::test]
    async fn test_header_based_route() {
        let header_route = HeaderBasedRoute {
            routes: vec![
                HeaderRoute {
                    header_key: "x-version".to_string(),
                    header_value_mapping_type: HeaderValueMappingType::Text(TextMatch {
                        value: "v1".to_string(),
                    }),
                    base_route: BaseRoute {
                        endpoint: "server_v1".to_string(),
                        ..Default::default()
                    },
                },
                HeaderRoute {
                    header_key: "x-debug".to_string(),
                    header_value_mapping_type: HeaderValueMappingType::Regex(RegexMatch {
                        value: r"true|1".to_string(),
                    }),
                    base_route: BaseRoute {
                        endpoint: "debug_server".to_string(),
                        ..Default::default()
                    },
                },
            ],
        };

        let mut strategy = LoadbalancerStrategy::HeaderBased(header_route);

        let mut headers = HeaderMap::new();
        headers.insert("x-version", HeaderValue::from_static("v1"));
        assert_eq!(strategy.get_route(&headers).unwrap().endpoint, "server_v1");

        // 测试正则匹配
        let mut headers = HeaderMap::new();
        headers.insert("x-debug", HeaderValue::from_static("true"));
        assert_eq!(
            strategy.get_route(&headers).unwrap().endpoint,
            "debug_server"
        );
    }

    #[tokio::test]
    async fn test_random_route() {
        let mut strategy = LoadbalancerStrategy::Random(RandomRoute {
            routes: vec![
                RandomBaseRoute {
                    base_route: BaseRoute {
                        endpoint: "server_a".to_string(),
                        ..Default::default()
                    },
                },
                RandomBaseRoute {
                    base_route: BaseRoute {
                        endpoint: "server_b".to_string(),
                        ..Default::default()
                    },
                },
            ],
        });

        let mut results = vec![];
        for _ in 0..100 {
            let route = strategy.get_route(&HeaderMap::new()).unwrap();
            results.push(route.endpoint);
        }
        assert!(results.contains(&"server_a".to_string()));
        assert!(results.contains(&"server_b".to_string()));
    }

    #[tokio::test]
    async fn test_weight_based_route() {
        let mut strategy = LoadbalancerStrategy::WeightBased(WeightBasedRoute {
            routes: vec![
                WeightRoute {
                    weight: 3,
                    base_route: BaseRoute {
                        endpoint: "server_heavy".to_string(),
                        ..Default::default()
                    },
                    index: 0,
                },
                WeightRoute {
                    weight: 1,
                    base_route: BaseRoute {
                        endpoint: "server_light".to_string(),
                        ..Default::default()
                    },
                    index: 0,
                },
            ],
        });

        let mut results = vec![];
        for _ in 0..4 {
            let route = strategy.get_route(&HeaderMap::new()).unwrap();
            results.push(route.endpoint);
        }
        assert_eq!(results[0..3], vec!["server_heavy"; 3]);
        assert_eq!(results[3], "server_light");
    }

    #[tokio::test]
    async fn test_get_all_routes() {
        let mut poll_strategy = LoadbalancerStrategy::Poll(PollRoute {
            routes: vec![
                PollBaseRoute {
                    base_route: BaseRoute {
                        endpoint: "s1".to_string(),
                        ..Default::default()
                    },
                },
                PollBaseRoute {
                    base_route: BaseRoute {
                        endpoint: "s2".to_string(),
                        ..Default::default()
                    },
                },
            ],
            ..Default::default()
        });

        let routes = poll_strategy.get_all_route().await.unwrap();
        assert_eq!(routes.len(), 2);
        assert_eq!(routes[0].endpoint, "s1");
        assert_eq!(routes[1].endpoint, "s2");
    }

    #[tokio::test]
    async fn test_empty_routes() {
        let mut strategy = LoadbalancerStrategy::WeightBased(WeightBasedRoute { routes: vec![] });
        assert!(strategy.get_route(&HeaderMap::new()).is_err());
    }
}
