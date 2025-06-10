use crate::proxy::proxy_trait::RouterDestination;
use serde::Serializer;

use super::app_error::AppError;
use core::fmt::Debug;
use http::HeaderMap;
use http::HeaderValue;
use rand::prelude::*;
use regex::Regex;
use serde::de;
use serde::de::MapAccess;
use serde::de::Visitor;
use serde::ser::SerializeStruct;
use serde::Deserializer;
use std::path::Path;

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Router {
    #[serde(rename = "file")]
    StaticFile(StaticFileRoute),
    #[serde(rename = "poll")]
    Poll(PollRoute),
    #[serde(rename = "header")]
    HeaderBased(HeaderBasedRoute),
    #[serde(rename = "random")]
    Random(RandomRoute),
    #[serde(rename = "weight")]
    WeightBased(WeightBasedRoute),
}
#[derive(Debug, Clone, PartialEq, Serialize, Default, Eq)]

pub struct StaticFileRoute {
    pub doc_root: String,
}
impl<'de> Deserialize<'de> for StaticFileRoute {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(StaticFileRouteVisitor)
    }
}

struct StaticFileRouteVisitor;

impl<'de> Visitor<'de> for StaticFileRouteVisitor {
    type Value = StaticFileRoute;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a map representing StaticFileRoute")
    }

    fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut doc_root = None;

        while let Some((key, value)) = map.next_entry::<String, String>()? {
            match key.as_str() {
                "doc_root" => {
                    let path = Path::new(&value);
                    if !path.exists() {
                        return Err(serde::de::Error::custom(format!(
                            "doc_root '{}' does not exist in the file system",
                            value
                        )));
                    }
                    if !path.is_dir() {
                        return Err(serde::de::Error::custom(format!(
                            "doc_root '{}' is not a directory",
                            value
                        )));
                    }
                    doc_root = Some(value);
                }
                unknown_key => {
                    return Err(serde::de::Error::unknown_field(unknown_key, &["doc_root"]));
                }
            }
        }

        let doc_root = doc_root.ok_or_else(|| serde::de::Error::missing_field("doc_root"))?;

        Ok(StaticFileRoute { doc_root })
    }
}
impl Default for Router {
    fn default() -> Self {
        Self::Poll(PollRoute::default())
    }
}
impl Router {
    pub fn get_route(
        &mut self,
        headers: &HeaderMap<HeaderValue>,
    ) -> Result<RouterDestination, AppError> {
        match self {
            Router::StaticFile(s) => Ok(RouterDestination::File(s.clone())),
            Router::Poll(poll_route) => Ok(RouterDestination::Http(poll_route.get_route(headers)?)),

            Router::HeaderBased(poll_route) => {
                Ok(RouterDestination::Http(poll_route.get_route(headers)?))
            }

            Router::Random(poll_route) => {
                Ok(RouterDestination::Http(poll_route.get_route(headers)?))
            }

            Router::WeightBased(poll_route) => {
                Ok(RouterDestination::Http(poll_route.get_route(headers)?))
            }
        }
    }
    pub async fn get_all_route(&mut self) -> Result<Vec<BaseRoute>, AppError> {
        match self {
            Router::StaticFile(_) => {
                Err(AppError("StaticFile router can not get route".to_string()))
            }
            Router::Poll(poll_route) => poll_route.get_all_route().await,
            Router::HeaderBased(poll_route) => poll_route.get_all_route().await,

            Router::Random(poll_route) => poll_route.get_all_route().await,

            Router::WeightBased(poll_route) => poll_route.get_all_route().await,
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
            Router::Poll(poll_route) => poll_route.update_route_alive(base_route, is_alive),
            Router::HeaderBased(poll_route) => poll_route.update_route_alive(base_route, is_alive),

            Router::Random(poll_route) => poll_route.update_route_alive(base_route, is_alive),

            Router::WeightBased(poll_route) => poll_route.update_route_alive(base_route, is_alive),
        }
    }
}
pub fn deserialize_router<'de, D>(deserializer: D) -> Result<Router, D::Error>
where
    D: Deserializer<'de>,
{
    struct StrategyVisitor;

    impl<'de> Visitor<'de> for StrategyVisitor {
        type Value = Router;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("string, array or object")
        }

        fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(Router::Random(RandomRoute {
                routes: vec![BaseRoute {
                    endpoint: v.to_string(),
                    is_alive: None,
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
            Ok(Router::Random(RandomRoute::new(backends)))
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AnomalyDetectionStatus {
    pub consecutive_5xx: i32,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default, Eq)]
pub struct BaseRoute {
    pub endpoint: String,
    #[serde(skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub is_alive: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SplitSegment {
    #[serde(rename = "by")]
    pub split_by: String,
    #[serde(rename = "matches")]
    pub split_list: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SplitItem {
    pub header_key: String,
    pub header_value: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]

pub struct RegexMatch {
    pub value: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextMatch {
    pub value: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum HeaderValueMappingType {
    Regex(String),
    Text(String),
    Split(SplitSegment),
}
impl Default for HeaderValueMappingType {
    fn default() -> Self {
        Self::Text("".to_string())
    }
}
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HeaderRoutingRule {
    pub endpoint: String,
    pub is_alive: Option<bool>,
    pub header_key: String,
    pub header_value_mapping_type: HeaderValueMappingType,
}
impl HeaderRoutingRule {
    pub fn get_base_route(&self) -> BaseRoute {
        BaseRoute {
            endpoint: self.endpoint.clone(),
            is_alive: self.is_alive,
        }
    }
}

impl Serialize for HeaderRoutingRule {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut field_count = 3;
        if self.is_alive.is_some() {
            field_count += 1;
        }

        let mut state = serializer.serialize_struct("HeaderRoutingRule", field_count)?;

        state.serialize_field("endpoint", &self.endpoint)?;

        if let Some(alive) = self.is_alive {
            state.serialize_field("is_alive", &alive)?;
        }

        state.serialize_field("header", &self.header_key)?;

        match &self.header_value_mapping_type {
            HeaderValueMappingType::Text(value) => {
                state.serialize_field("match", &serde_json::json!({ "text": value }))?;
            }
            HeaderValueMappingType::Regex(value) => {
                state.serialize_field("match", &serde_json::json!({ "regex": value }))?;
            }
            HeaderValueMappingType::Split(segment) => {
                state.serialize_field("match", &serde_json::json!({ "split": segment }))?;
            }
        }

        state.end()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum MatchTypeHelper {
    Text(String),
    Regex(String),
    Split(SplitSegment),
}

impl<'de> Deserialize<'de> for HeaderRoutingRule {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(field_identifier, rename_all = "snake_case")]
        enum Field {
            Endpoint,
            Header,
            Match,
        }

        struct HeaderRoutingRuleVisitor;

        impl<'de> Visitor<'de> for HeaderRoutingRuleVisitor {
            type Value = HeaderRoutingRule;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str(
                    "struct HeaderRoutingRule with fields 'endpoint', 'header', and 'match'",
                )
            }

            fn visit_map<V>(self, mut map: V) -> Result<HeaderRoutingRule, V::Error>
            where
                V: MapAccess<'de>,
            {
                let mut endpoint = None;
                let mut header_key = None;
                let mut mapping_type = None;

                while let Some(key) = map.next_key()? {
                    match key {
                        Field::Endpoint => {
                            if endpoint.is_some() {
                                return Err(de::Error::duplicate_field("endpoint"));
                            }
                            endpoint = Some(map.next_value()?);
                        }
                        Field::Header => {
                            if header_key.is_some() {
                                return Err(de::Error::duplicate_field("header"));
                            }
                            header_key = Some(map.next_value()?);
                        }
                        Field::Match => {
                            if mapping_type.is_some() {
                                return Err(de::Error::duplicate_field("match"));
                            }
                            let helper: MatchTypeHelper = map.next_value()?;
                            mapping_type = Some(match helper {
                                MatchTypeHelper::Text(v) => HeaderValueMappingType::Text(v),
                                MatchTypeHelper::Regex(v) => HeaderValueMappingType::Regex(v),
                                MatchTypeHelper::Split(v) => HeaderValueMappingType::Split(v),
                            });
                        }
                    }
                }

                let endpoint = endpoint.ok_or_else(|| de::Error::missing_field("endpoint"))?;
                let header_key = header_key.ok_or_else(|| de::Error::missing_field("header"))?;
                let header_value_mapping_type =
                    mapping_type.ok_or_else(|| de::Error::missing_field("match"))?;

                Ok(HeaderRoutingRule {
                    endpoint,
                    header_key,
                    header_value_mapping_type,

                    is_alive: None,
                })
            }
        }

        const FIELDS: &[&str] = &["endpoint", "header", "match"];
        deserializer.deserialize_struct("HeaderRoutingRule", FIELDS, HeaderRoutingRuleVisitor)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeaderBasedRoute {
    #[serde(rename = "targets")]
    pub routes: Vec<HeaderRoutingRule>,
}

impl HeaderBasedRoute {
    async fn get_all_route(&mut self) -> Result<Vec<BaseRoute>, AppError> {
        Ok(self
            .routes
            .iter()
            .map(|item| item.get_base_route().clone())
            .collect::<Vec<BaseRoute>>())
    }

    fn get_route(&mut self, headers: &HeaderMap<HeaderValue>) -> Result<BaseRoute, AppError> {
        let has_unconfigured = self.routes.iter().any(|r| r.is_alive.is_none());
        debug!("has_unconfigured:{}", has_unconfigured);
        let routes = if has_unconfigured {
            self.routes.clone()
        } else {
            self.routes
                .iter()
                .filter(|r| r.is_alive == Some(true))
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
                    let re = Regex::new(&regex_str)?;
                    let capture_option = re.captures(header_value_str);
                    if capture_option.is_none() {
                        continue;
                    } else {
                        return Ok(item.clone().get_base_route());
                    }
                }
                HeaderValueMappingType::Text(text_str) => {
                    if text_str == header_value_str {
                        return Ok(item.clone().get_base_route());
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
                        return Ok(item.clone().get_base_route());
                    }
                }
            }
        }
        error!("Can not find the route!And Spire has selected the first route!");

        let first = self
            .routes
            .first()
            .ok_or("The first item not found.")?
            .get_base_route()
            .clone();
        Ok(first)
    }
    fn update_route_alive(
        &mut self,
        base_route: BaseRoute,
        is_alive: bool,
    ) -> Result<(), AppError> {
        for item in self.routes.iter_mut() {
            if item.endpoint == base_route.endpoint {
                item.is_alive = Some(is_alive);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RandomRoute {
    #[serde(rename = "targets")]
    pub routes: Vec<BaseRoute>,
}

impl RandomRoute {
    pub fn new(backends: Vec<String>) -> Self {
        Self {
            routes: backends
                .iter()
                .map(|item| BaseRoute {
                    endpoint: item.clone(),
                    is_alive: None,
                })
                .collect::<Vec<BaseRoute>>(),
        }
    }
    async fn get_all_route(&mut self) -> Result<Vec<BaseRoute>, AppError> {
        Ok(self.routes.to_vec())
    }

    fn get_route(&mut self, _headers: &HeaderMap<HeaderValue>) -> Result<BaseRoute, AppError> {
        let has_unconfigured = self.routes.iter().any(|r| r.is_alive.is_none());

        if has_unconfigured {
            let mut rng = rand::rng();
            let random_index = rng.random_range(0..self.routes.len());
            Ok(self.routes[random_index].clone())
        } else {
            let alive_indices: Vec<usize> = self
                .routes
                .iter()
                .enumerate()
                .filter(|(_, r)| r.is_alive == Some(true))
                .map(|(i, _)| i)
                .collect();
            if alive_indices.is_empty() {
                debug!("All routes are dead, selecting a random route");
                let mut rng = rand::rng();
                let random_index = rng.random_range(0..self.routes.len());
                Ok(self.routes[random_index].clone())
            } else {
                let mut rng = rand::rng();
                let random_index = rng.random_range(0..alive_indices.len());
                Ok(self.routes[alive_indices[random_index]].clone())
            }
        }
    }
    fn update_route_alive(
        &mut self,
        base_route: BaseRoute,
        is_alive: bool,
    ) -> Result<(), AppError> {
        for item in self.routes.iter_mut() {
            if item.endpoint == base_route.endpoint {
                item.is_alive = Some(is_alive);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct PollRoute {
    #[serde(skip_deserializing, skip_serializing)]
    pub current_index: i128,
    #[serde(rename = "targets")]
    pub routes: Vec<BaseRoute>,
}

impl PollRoute {
    async fn get_all_route(&mut self) -> Result<Vec<BaseRoute>, AppError> {
        Ok(self.routes.clone())
    }

    fn get_route(&mut self, _headers: &HeaderMap<HeaderValue>) -> Result<BaseRoute, AppError> {
        let has_unconfigured = self.routes.iter().any(|r| r.is_alive.is_none());
        if has_unconfigured {
            self.current_index += 1;
            if self.current_index >= self.routes.len() as i128 {
                self.current_index = 0;
            }
            debug!("current_index:{}", self.current_index);
            let route = self.routes[self.current_index as usize].clone();
            Ok(route)
        } else {
            let alive_indices: Vec<usize> = self
                .routes
                .iter()
                .enumerate()
                .filter(|(_, r)| r.is_alive == Some(true))
                .map(|(i, _)| i)
                .collect();
            if alive_indices.is_empty() {
                debug!("All routes are dead, selecting a random route");
                let mut rng = rand::rng();
                let random_index = rng.random_range(0..self.routes.len());
                Ok(self.routes[random_index].clone())
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
                let route = self.routes[selected_index].clone();
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
            if item.endpoint == base_route.endpoint {
                item.is_alive = Some(is_alive);
            }
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WeightBasedRoute {
    #[serde(rename = "targets")]
    pub routes: Vec<WeightedRouteItem>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct WeightedRouteItem {
    pub endpoint: String,
    #[serde(skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub is_alive: Option<bool>,
    pub weight: i32,
    #[serde(skip_deserializing, skip_serializing, default)]
    pub index: i32,
}
impl WeightedRouteItem {
    fn get_base_route(&self) -> BaseRoute {
        BaseRoute {
            endpoint: self.endpoint.clone(),
            is_alive: self.is_alive,
        }
    }
}
impl WeightBasedRoute {
    async fn get_all_route(&mut self) -> Result<Vec<BaseRoute>, AppError> {
        Ok(self
            .routes
            .iter_mut()
            .map(|item| item.get_base_route().clone())
            .collect::<Vec<BaseRoute>>())
    }

    fn get_route(&mut self, _headers: &HeaderMap<HeaderValue>) -> Result<BaseRoute, AppError> {
        if self.routes.is_empty() {
            return Err(AppError::from("No routes available"));
        }

        let has_unconfigured = self.routes.iter().any(|r| r.is_alive.is_none());
        if has_unconfigured {
            let all_reached = self.routes.iter().all(|r| r.index >= r.weight);
            if all_reached {
                for route in &mut self.routes {
                    route.index = 0;
                }
            }
            if let Some(route) = self.routes.iter_mut().find(|r| r.index < r.weight) {
                route.index += 1;
                Ok(route.get_base_route().clone())
            } else {
                Err(AppError::from("WeightRoute get route error"))
            }
        } else {
            let alive_indices: Vec<usize> = self
                .routes
                .iter()
                .enumerate()
                .filter(|(_, r)| r.is_alive == Some(true))
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
                        return Ok(self.routes[i].get_base_route().clone());
                    }
                }
                Err(AppError::from("WeightRoute get route error"))
            } else {
                let mut rng = rand::rng();
                let idx = rng.random_range(0..self.routes.len());
                self.routes[idx].index += 1;
                Ok(self.routes[idx].get_base_route().clone())
            }
        }
    }
    fn update_route_alive(
        &mut self,
        base_route: BaseRoute,
        is_alive: bool,
    ) -> Result<(), AppError> {
        for item in self.routes.iter_mut() {
            if item.endpoint == base_route.endpoint {
                item.is_alive = Some(is_alive);
            }
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use http::{HeaderMap, HeaderValue};
    use serde_json;

    use tempfile::tempdir;
    #[test]
    fn test_static_file_route_deserialization() {
        let temp_dir = tempdir().unwrap();
        let path_str = temp_dir.path().to_str().unwrap();
        let json = format!(r#"{{"doc_root": "{}"}}"#, path_str.replace('\\', "\\\\"));
        let result: Result<StaticFileRoute, _> = serde_json::from_str(&json);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().doc_root, path_str);

        let json = r#"{"doc_root": "/a/b/c/non-existent-path"}"#;
        let result: Result<StaticFileRoute, _> = serde_json::from_str(json);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("does not exist"));

        let temp_file = tempfile::NamedTempFile::new().unwrap();
        let file_path_str = temp_file.path().to_str().unwrap();
        let json = format!(
            r#"{{"doc_root": "{}"}}"#,
            file_path_str.replace('\\', "\\\\")
        );
        let result: Result<StaticFileRoute, _> = serde_json::from_str(&json);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("is not a directory"));

        let json = r#"{"a":"b"}"#;
        let result: Result<StaticFileRoute, _> = serde_json::from_str(json);
        assert!(result.is_err());

        assert!(result
            .unwrap_err()
            .to_string()
            .contains("expected `doc_root`"));

        let json = r#""a":"b"}"#;
        let result: Result<StaticFileRoute, _> = serde_json::from_str(json);

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("expected a map representing StaticFileRoute"));
    }

    #[test]
    fn test_deserialize_router_flexible_formats() {
        let json = r#""http://localhost:8080""#;
        let router: Router =
            deserialize_router(&mut serde_json::Deserializer::from_str(json)).unwrap();
        match router {
            Router::Random(r) => {
                assert_eq!(r.routes.len(), 1);
                assert_eq!(r.routes[0].endpoint, "http://localhost:8080");
            }
            _ => panic!("Expected RandomRoute"),
        }

        let json = r#"["http://localhost:8080", "http://localhost:8081"]"#;
        let router: Router =
            deserialize_router(&mut serde_json::Deserializer::from_str(json)).unwrap();
        match router {
            Router::Random(r) => {
                assert_eq!(r.routes.len(), 2);
                assert_eq!(r.routes[1].endpoint, "http://localhost:8081");
            }
            _ => panic!("Expected RandomRoute"),
        }

        let json = r#"{
        "kind": "poll",
        "routes": [
            {"endpoint": "http://s1"},
            {"endpoint": "http://s2"}
        ]
    }"#;
        let router: Router =
            deserialize_router(&mut serde_json::Deserializer::from_str(json)).unwrap();
        match router {
            Router::Poll(p) => {
                assert_eq!(p.routes.len(), 2);
                assert_eq!(p.routes[0].endpoint, "http://s1");
            }
            _ => panic!("Expected PollRoute"),
        }
    }
    #[test]
    fn test_poll_route_logic() {
        let mut poll_route = PollRoute {
            current_index: -1,
            routes: vec![
                BaseRoute {
                    endpoint: "s1".to_string(),
                    is_alive: None,
                },
                BaseRoute {
                    endpoint: "s2".to_string(),
                    is_alive: None,
                },
                BaseRoute {
                    endpoint: "s3".to_string(),
                    is_alive: None,
                },
            ],
        };

        assert_eq!(
            poll_route.get_route(&HeaderMap::new()).unwrap().endpoint,
            "s1"
        );
        assert_eq!(
            poll_route.get_route(&HeaderMap::new()).unwrap().endpoint,
            "s2"
        );
        assert_eq!(
            poll_route.get_route(&HeaderMap::new()).unwrap().endpoint,
            "s3"
        );
        assert_eq!(
            poll_route.get_route(&HeaderMap::new()).unwrap().endpoint,
            "s1"
        );

        poll_route
            .update_route_alive(
                BaseRoute {
                    endpoint: "s2".to_string(),
                    is_alive: None,
                },
                false,
            )
            .unwrap();
        poll_route
            .update_route_alive(
                BaseRoute {
                    endpoint: "s1".to_string(),
                    is_alive: None,
                },
                true,
            )
            .unwrap();
        poll_route
            .update_route_alive(
                BaseRoute {
                    endpoint: "s3".to_string(),
                    is_alive: None,
                },
                true,
            )
            .unwrap();

        poll_route.current_index = -1; // reset for testing

        assert_eq!(
            poll_route.get_route(&HeaderMap::new()).unwrap().endpoint,
            "s1"
        );
        assert_eq!(
            poll_route.get_route(&HeaderMap::new()).unwrap().endpoint,
            "s3"
        );
        assert_eq!(
            poll_route.get_route(&HeaderMap::new()).unwrap().endpoint,
            "s1"
        );
    }
    #[test]
    fn test_random_route_logic() {
        let mut random_route = RandomRoute {
            routes: vec![
                BaseRoute {
                    endpoint: "s1".to_string(),
                    is_alive: None,
                },
                BaseRoute {
                    endpoint: "s2".to_string(),
                    is_alive: None,
                },
            ],
        };

        let route = random_route.get_route(&HeaderMap::new()).unwrap();
        assert!(route.endpoint == "s1" || route.endpoint == "s2");

        random_route
            .update_route_alive(
                BaseRoute {
                    endpoint: "s1".to_string(),
                    is_alive: None,
                },
                false,
            )
            .unwrap();
        random_route
            .update_route_alive(
                BaseRoute {
                    endpoint: "s2".to_string(),
                    is_alive: None,
                },
                true,
            )
            .unwrap();

        for _ in 0..10 {
            assert_eq!(
                random_route.get_route(&HeaderMap::new()).unwrap().endpoint,
                "s2"
            );
        }

        random_route
            .update_route_alive(
                BaseRoute {
                    endpoint: "s2".to_string(),
                    is_alive: None,
                },
                false,
            )
            .unwrap();

        let route = random_route.get_route(&HeaderMap::new()).unwrap();
        assert!(route.endpoint == "s1" || route.endpoint == "s2");
    }
    #[test]
    fn test_weight_based_route_logic() {
        let mut weight_route = WeightBasedRoute {
            routes: vec![
                WeightedRouteItem {
                    base_route: BaseRoute {
                        endpoint: "s1".to_string(),
                        is_alive: None,
                    },
                    weight: 2,
                    index: 0,
                },
                WeightedRouteItem {
                    base_route: BaseRoute {
                        endpoint: "s2".to_string(),
                        is_alive: None,
                    },
                    weight: 1,
                    index: 0,
                },
            ],
        };

        assert_eq!(
            weight_route.get_route(&HeaderMap::new()).unwrap().endpoint,
            "s1"
        );
        assert_eq!(
            weight_route.get_route(&HeaderMap::new()).unwrap().endpoint,
            "s1"
        );
        assert_eq!(
            weight_route.get_route(&HeaderMap::new()).unwrap().endpoint,
            "s2"
        );
        assert_eq!(
            weight_route.get_route(&HeaderMap::new()).unwrap().endpoint,
            "s1"
        );
        assert_eq!(
            weight_route.get_route(&HeaderMap::new()).unwrap().endpoint,
            "s1"
        );

        weight_route
            .update_route_alive(
                BaseRoute {
                    endpoint: "s1".to_string(),
                    is_alive: None,
                },
                false,
            )
            .unwrap();
        weight_route
            .update_route_alive(
                BaseRoute {
                    endpoint: "s2".to_string(),
                    is_alive: None,
                },
                true,
            )
            .unwrap();

        for r in &mut weight_route.routes {
            r.index = 0;
        }

        for _ in 0..5 {
            assert_eq!(
                weight_route.get_route(&HeaderMap::new()).unwrap().endpoint,
                "s2"
            );
        }
    }
    #[test]
    fn test_header_based_route_logic() {
        let mut header_route = HeaderBasedRoute {
            routes: vec![
                HeaderRoutingRule {
                    base_route: BaseRoute {
                        endpoint: "user-service".to_string(),
                        is_alive: Some(true),
                    },
                    header_key: "x-request-id".to_string(),
                    header_value_mapping_type: HeaderValueMappingType::Regex(RegexMatch {
                        value: r"^user-\d+$".to_string(),
                    }),
                },
                HeaderRoutingRule {
                    base_route: BaseRoute {
                        endpoint: "admin-service".to_string(),
                        is_alive: Some(true),
                    },
                    header_key: "x-user-role".to_string(),
                    header_value_mapping_type: HeaderValueMappingType::Text(TextMatch {
                        value: "admin".to_string(),
                    }),
                },
                HeaderRoutingRule {
                    base_route: BaseRoute {
                        endpoint: "feature-service".to_string(),
                        is_alive: Some(true),
                    },
                    header_key: "x-flags".to_string(),
                    header_value_mapping_type: HeaderValueMappingType::Split(SplitSegment {
                        split_by: ",".to_string(),
                        split_list: vec!["beta".to_string(), "canary".to_string()],
                    }),
                },
            ],
        };

        let mut headers = HeaderMap::new();
        headers.insert("x-request-id", HeaderValue::from_static("user-12345"));
        assert_eq!(
            header_route.get_route(&headers).unwrap().endpoint,
            "user-service"
        );

        headers.clear();
        headers.insert("x-user-role", HeaderValue::from_static("admin"));
        assert_eq!(
            header_route.get_route(&headers).unwrap().endpoint,
            "admin-service"
        );

        headers.clear();
        headers.insert("x-flags", HeaderValue::from_static("canary,new-ui,beta"));
        assert_eq!(
            header_route.get_route(&headers).unwrap().endpoint,
            "feature-service"
        );

        headers.clear();
        headers.insert("x-some-other-header", HeaderValue::from_static("value"));
        assert_eq!(
            header_route.get_route(&headers).unwrap().endpoint,
            "user-service"
        );

        header_route
            .update_route_alive(
                BaseRoute {
                    endpoint: "admin-service".to_string(),
                    is_alive: None,
                },
                false,
            )
            .unwrap();
        headers.clear();
        headers.insert("x-user-role", HeaderValue::from_static("admin"));
        assert_eq!(
            header_route.get_route(&headers).unwrap().endpoint,
            "user-service"
        );
    }
    #[test]
    fn test_router_enum_dispatch() {
        let mut static_file_router = Router::StaticFile(StaticFileRoute {
            doc_root: "".to_string(),
        });
        static_file_router.get_route(&HeaderMap::new()).unwrap();
        let mut header_based_router = Router::HeaderBased(HeaderBasedRoute {
            routes: vec![HeaderRoutingRule {
                header_key: "a".to_string(),
                header_value_mapping_type: HeaderValueMappingType::Text(TextMatch {
                    value: "b".to_string(),
                }),
                base_route: BaseRoute {
                    endpoint: "s1".to_string(),
                    is_alive: None,
                },
            }],
        });
        header_based_router.get_route(&HeaderMap::new()).unwrap();
        let mut router = Router::Poll(PollRoute {
            current_index: -1,
            routes: vec![BaseRoute {
                endpoint: "s1".to_string(),
                is_alive: None,
            }],
        });

        let dest = router.get_route(&HeaderMap::new()).unwrap();
        assert_eq!(
            dest,
            RouterDestination::Http(BaseRoute {
                endpoint: "s1".to_string(),
                is_alive: None
            })
        );

        router
            .update_route_alive(
                BaseRoute {
                    endpoint: "s1".to_string(),
                    is_alive: None,
                },
                false,
            )
            .unwrap();
        if let Router::Poll(p) = router {
            assert_eq!(p.routes[0].is_alive, Some(false));
        } else {
            panic!("Router type changed unexpectedly");
        }
    }

    #[tokio::test]
    async fn test_router_get_all_route() {
        let mut header_based_router = Router::HeaderBased(HeaderBasedRoute { routes: vec![] });
        let _ = header_based_router.get_all_route().await;
        let _ = header_based_router.update_route_alive(
            BaseRoute {
                endpoint: "test".to_string(),
                is_alive: None,
            },
            false,
        );
        let mut weight_based_router = Router::WeightBased(WeightBasedRoute { routes: vec![] });
        let _ = weight_based_router.get_all_route().await;
        let _ = weight_based_router.update_route_alive(
            BaseRoute {
                endpoint: "test".to_string(),
                is_alive: None,
            },
            false,
        );
        let mut router = Router::Random(RandomRoute {
            routes: vec![
                BaseRoute {
                    endpoint: "s1".to_string(),
                    is_alive: None,
                },
                BaseRoute {
                    endpoint: "s2".to_string(),
                    is_alive: None,
                },
            ],
        });
        let _ = router.update_route_alive(
            BaseRoute {
                endpoint: "test".to_string(),
                is_alive: None,
            },
            false,
        );
        let all_routes = router.get_all_route().await.unwrap();
        assert_eq!(all_routes.len(), 2);
        assert_eq!(all_routes[0].endpoint, "s1");

        let mut static_router = Router::StaticFile(StaticFileRoute {
            doc_root: ".".to_string(),
        });
        let _ = static_router.update_route_alive(
            BaseRoute {
                endpoint: "test".to_string(),
                is_alive: None,
            },
            false,
        );
        let result = static_router.get_all_route().await;
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err(),
            AppError("StaticFile router can not get route".to_string())
        );
    }
}
