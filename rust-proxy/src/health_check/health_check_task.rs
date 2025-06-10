use crate::constants::common_constants::TIMER_WAIT_SECONDS;
use crate::proxy::http1::http_client::HttpClients;
use crate::vojo::app_config::RouteConfig;
use crate::vojo::app_error::AppError;
use crate::vojo::cli::SharedConfig;
use crate::vojo::health_check::HealthCheckType;
use crate::vojo::health_check::HttpHealthCheckParam;
use async_trait::async_trait;
use bytes::Bytes;
use delay_timer::prelude::*;
use futures;
use futures::FutureExt;
use http::Request;
use http::StatusCode;
use http_body_util::combinators::BoxBody;
use http_body_util::BodyExt;
use http_body_util::Full;
use hyper::Response;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::task::JoinSet;
use tokio::time::sleep;

use url::Url;
#[derive(Clone)]
pub struct HealthCheckClient {
    pub http_clients: HttpClients,
}
impl HealthCheckClient {
    pub fn new() -> Self {
        HealthCheckClient {
            http_clients: HttpClients::new(),
        }
    }
}
#[async_trait]
impl HttpClientTrait for HealthCheckClient {
    async fn request_http(
        &self,
        req: Request<BoxBody<Bytes, AppError>>,
        time_out: u64,
    ) -> Result<Response<BoxBody<Bytes, AppError>>, AppError> {
        let fut = self
            .http_clients
            .request_http(req, time_out)
            .await??
            .map(|b| b.boxed())
            .map(|item: BoxBody<Bytes, hyper::Error>| {
                item.map_err(|_| -> AppError { unreachable!() }).boxed()
            });
        Ok(fut)
    }
}
#[async_trait]
pub trait HttpClientTrait: Send + Sync + Clone {
    async fn request_http(
        &self,
        req: Request<BoxBody<Bytes, AppError>>,
        time_out: u64,
    ) -> Result<Response<BoxBody<Bytes, AppError>>, AppError>;
}
#[derive(Hash, Clone, Eq, PartialEq, Debug)]
pub struct TaskKey {
    pub route_id: String,
    pub health_check_type: HealthCheckType,
    pub endpoint_list: Vec<String>,
    pub min_liveness_count: i32,
}
impl TaskKey {
    pub fn new(
        route_id: String,
        health_check_type: HealthCheckType,
        endpoint_list: Vec<String>,
        min_liveness_count: i32,
    ) -> Self {
        TaskKey {
            route_id,
            health_check_type,
            endpoint_list,
            min_liveness_count,
        }
    }
}
async fn get_endpoint_list(mut route: RouteConfig) -> Vec<String> {
    let mut result = vec![];
    let base_route_list = route.router.get_all_route().await.unwrap_or(vec![]);
    for item in base_route_list {
        result.push(item.endpoint);
    }
    result
}
pub struct HealthCheck {
    pub task_id_map: HashMap<TaskKey, u64>,
    pub delay_timer: DelayTimer,
    pub health_check_client: HealthCheckClient,
    pub current_id: Arc<AtomicU64>,
    pub shared_config: SharedConfig,
}
impl HealthCheck {
    pub fn from_shared_config(shared_config: SharedConfig) -> Self {
        HealthCheck {
            task_id_map: HashMap::new(),
            delay_timer: DelayTimerBuilder::default().build(),
            health_check_client: HealthCheckClient::new(),
            current_id: Arc::new(AtomicU64::new(0)),
            shared_config,
        }
    }
    pub async fn start_health_check_loop(&mut self) {
        loop {
            let async_result = std::panic::AssertUnwindSafe(self.do_health_check())
                .catch_unwind()
                .await;
            if async_result.is_err() {
                error!("start_health_check_loop catch panic successfully!");
            }
            sleep(std::time::Duration::from_secs(TIMER_WAIT_SECONDS)).await;
        }
    }

    async fn do_health_check(&mut self) -> Result<(), AppError> {
        let app_config = self.shared_config.shared_data.lock()?.clone();
        let mut route_list = HashMap::new();
        for (_, service_config) in app_config.api_service_config.iter() {
            for route in &service_config.service_config.route_configs {
                if route.health_check.is_none() || route.liveness_config.is_none() {
                    continue;
                }
                let endpoint_list = get_endpoint_list(route.clone()).await;

                let health_check = route.health_check.as_ref().ok_or("Health check is none!")?;
                let liveness_config = route
                    .liveness_config
                    .as_ref()
                    .ok_or("Liveness config is none!")?;

                let task_key = TaskKey::new(
                    route.route_id.clone(),
                    health_check.clone(),
                    endpoint_list,
                    liveness_config.min_liveness_count,
                );

                route_list.insert(task_key, route.clone());
            }
        }
        self.task_id_map.retain(|route_id, task_id| {
            if !route_list.contains_key(route_id) {
                let res = self.delay_timer.remove_task(*task_id);
                if let Err(err) = res {
                    error!("Health check task remove task error,the error is {}.", err);
                    return true;
                } else {
                    return false;
                }
            }
            true
        });
        let old_map = self.task_id_map.clone();

        route_list
            .iter()
            .filter(|(task_key, _)| !old_map.contains_key(&(*task_key).clone()))
            .for_each(|(task_key, route)| {
                info!("The route is {:?}", route);
                let current_id = self.current_id.fetch_add(1, Ordering::SeqCst);
                let submit_task_result = submit_task(
                    current_id,
                    route.clone(),
                    self.health_check_client.clone(),
                    self.shared_config.clone(),
                );
                if let Ok(submit_result) = submit_task_result {
                    let res = self.delay_timer.insert_task(submit_result);
                    if let Ok(_task_instance_chain) = res {
                        self.task_id_map.insert(task_key.clone(), current_id);
                    }
                } else {
                    error!("Submit task error");
                }
            });

        Ok(())
    }
}

async fn do_http_health_check<HC: HttpClientTrait + Send + Sync + 'static>(
    http_health_check_param: HttpHealthCheckParam,
    mut route: RouteConfig,
    timeout_number: i32,
    http_health_check_client: Arc<HC>,
    shared_config: SharedConfig,
) -> Result<(), AppError> {
    info!("Do http health check,the route is {:?}!", route);
    let route_list = route.router.get_all_route().await?;
    let mut set = JoinSet::new();
    for item in route_list {
        let http_client_shared = http_health_check_client.clone();
        let host_option = Url::parse(item.endpoint.as_str());
        if host_option.is_err() {
            error!("Parse host error,the error is {}", host_option.unwrap_err());
            continue;
        }

        let join_option = host_option?.join(http_health_check_param.path.clone().as_str());
        if join_option.is_err() {
            error!("Parse host error,the error is {}", join_option.unwrap_err());
            continue;
        }

        let req = Request::builder()
            .uri(join_option?.to_string())
            .method("GET")
            .body(Full::new(Bytes::new()).map_err(AppError::from).boxed())?;
        let cloned_route = route.clone();
        set.spawn(async move {
            let res = http_client_shared
                .request_http(req, timeout_number as u64)
                .await;
            (res, cloned_route, item)
        });
    }
    while let Some(response_result1) = set.join_next().await {
        match response_result1 {
            Ok((res, route, base_route)) => {
                let mut lock = shared_config.shared_data.lock()?;
                let shared_route = lock
                    .api_service_config
                    .iter_mut()
                    .flat_map(|(_, item)| &mut item.service_config.route_configs)
                    .find(|item| item.route_id == route.route_id);
                let new_route = match shared_route {
                    Some(route) => route,
                    None => {
                        continue;
                    }
                };

                if let Ok(res) = res {
                    if res.status() == StatusCode::OK {
                        let _ = new_route
                            .router
                            .update_route_alive(base_route.clone(), true);
                    }
                } else {
                    error!("Request time out, the url is {}", base_route.endpoint);
                    let _ = new_route.router.update_route_alive(base_route, false);
                }
            }
            Err(e) => {
                error!("set join error,the error is {}", e);
            }
        }
    }
    Ok(())
}
fn submit_task(
    task_id: u64,
    route: RouteConfig,
    health_check_clients: HealthCheckClient,
    shared_config: SharedConfig,
) -> Result<Task, AppError> {
    info!("Submit task!");
    if let Some(health_check) = route.health_check.clone() {
        let mut task_builder = TaskBuilder::default();
        let base_param = health_check.get_base_param();
        let timeout = base_param.timeout;
        let task = move || {
            let route_share = route.clone();
            let timeout_share = timeout;
            let health_check_client_shared = health_check_clients.clone();
            let health_check_type_shared = health_check.clone();
            let cloned_shared_config = shared_config.clone();
            async move {
                match health_check_type_shared {
                    HealthCheckType::HttpGet(http_health_check_param) => {
                        do_http_health_check(
                            http_health_check_param,
                            route_share,
                            timeout_share,
                            Arc::new(health_check_client_shared),
                            cloned_shared_config,
                        )
                        .await
                    }
                    HealthCheckType::Mysql(_) => Ok(()),
                    HealthCheckType::Redis(_) => Ok(()),
                }
            }
        };
        info!(
            "The timer task has been submit,the task param is interval:{}!",
            base_param.interval
        );
        return Ok(task_builder
            .set_task_id(task_id)
            .set_frequency_repeated_by_seconds(base_param.interval as u64)
            .set_maximum_parallel_runnable_num(1)
            .spawn_async_routine(task)?);
    }
    Err(AppError::from("Submit task error!"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vojo::app_config::ApiService;
    use crate::vojo::app_config::AppConfig;
    use crate::vojo::app_config::LivenessConfig;
    use crate::vojo::health_check::BaseHealthCheckParam;
    use crate::vojo::router::RandomRoute;
    use mockall::mock;
    use mockall::predicate::*;

    mock! {
            pub HttpClient {

            }
            impl Clone for HttpClient {
                fn clone(&self) -> Self;
            }
            #[async_trait]
            impl HttpClientTrait for HttpClient {
                async fn request_http(
                    &self,
                    req: Request<BoxBody<Bytes, AppError>>,
                    time_out: u64,
                ) -> Result<Response<BoxBody<Bytes, AppError>>, AppError> ;
        }
    }
    fn dummy_response(status_code: StatusCode) -> Response<BoxBody<Bytes, AppError>> {
        Response::builder()
            .status(status_code)
            .body(Full::new(Bytes::from("")).map_err(AppError::from).boxed())
            .unwrap()
    }

    fn create_test_shared_config(route_configs: Vec<RouteConfig>) -> SharedConfig {
        let mut map = HashMap::new();
        let api_service = ApiService {
            service_config: crate::vojo::app_config::ServiceConfig {
                route_configs,
                ..Default::default()
            },
            ..Default::default()
        };
        map.insert(8080, api_service);
        SharedConfig::from_app_config(AppConfig {
            api_service_config: map,
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn test_health_check_single_route_ok() {
        let http_health_check_param = HttpHealthCheckParam {
            base_health_check_param: BaseHealthCheckParam {
                interval: 10,
                timeout: 1000,
            },
            path: "/health".to_string(),
        };
        let shared_config = create_test_shared_config(vec![RouteConfig {
            route_id: "config1".to_string(),
            health_check: Some(HealthCheckType::HttpGet(http_health_check_param.clone())),
            ..Default::default()
        }]);
        let mut mock_http_client = MockHttpClient::new();

        mock_http_client.expect_request_http().returning(|_, _| {
            let response_result: Result<Response<BoxBody<Bytes, AppError>>, AppError> =
                Ok(dummy_response(StatusCode::OK));
            response_result
        });

        let route_config = RouteConfig {
            route_id: "config1".to_string(),
            router: crate::vojo::router::Router::Random(RandomRoute::new(vec![
                "http://192.168.0.0:8080".to_string(),
                "a".to_string(),
            ])),
            ..Default::default()
        };
        let result = do_http_health_check(
            http_health_check_param,
            route_config,
            1000,
            Arc::new(mock_http_client),
            shared_config,
        )
        .await;

        assert!(result.is_ok());
    }
    #[tokio::test]
    async fn test_health_check_single_route_error() {
        let http_health_check_param = HttpHealthCheckParam {
            base_health_check_param: BaseHealthCheckParam {
                interval: 10,
                timeout: 1000,
            },
            path: "/health".to_string(),
        };
        let shared_config = create_test_shared_config(vec![RouteConfig {
            route_id: "config1".to_string(),
            health_check: Some(HealthCheckType::HttpGet(http_health_check_param.clone())),
            ..Default::default()
        }]);
        let mut mock_http_client = MockHttpClient::new();

        mock_http_client.expect_request_http().returning(|_, _| {
            let response_result: Result<Response<BoxBody<Bytes, AppError>>, AppError> =
                Err(AppError("()".to_string()));
            response_result
        });

        let route_config = RouteConfig {
            route_id: "config1".to_string(),
            router: crate::vojo::router::Router::Random(RandomRoute::new(vec![
                "http://192.168.0.0:8080".to_string(),
                "a".to_string(),
            ])),
            ..Default::default()
        };
        let result = do_http_health_check(
            http_health_check_param,
            route_config,
            1000,
            Arc::new(mock_http_client),
            shared_config,
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_submit_task_ok() {
        let http_health_check_param = HttpHealthCheckParam {
            base_health_check_param: BaseHealthCheckParam {
                interval: 1,
                timeout: 1,
            },
            path: "/health".to_string(),
        };
        let task_id = 101;
        let route_config = RouteConfig {
            route_id: "config1".to_string(),
            router: crate::vojo::router::Router::Random(RandomRoute::new(vec![
                "http://192.168.0.0:8080".to_string(),
                "a".to_string(),
            ])),
            health_check: Some(HealthCheckType::HttpGet(http_health_check_param.clone())),
            ..Default::default()
        };

        let shared_config = create_test_shared_config(vec![RouteConfig {
            route_id: "config1".to_string(),
            health_check: Some(HealthCheckType::HttpGet(http_health_check_param.clone())),
            ..Default::default()
        }]);
        let health_check_clients = HealthCheckClient::new();

        let result = submit_task(task_id, route_config, health_check_clients, shared_config);

        assert!(result.is_ok());
        let task = result.unwrap();
        let delay_timer = DelayTimerBuilder::default().build();
        let _ = delay_timer.insert_task(task);
        sleep(std::time::Duration::from_secs(3)).await;
        let res = delay_timer.remove_task(task_id);
        assert!(res.is_ok());
    }

    #[test]
    fn test_submit_task_error_when_no_health_check() {
        let task_id = 201;
        let http_health_check_param = HttpHealthCheckParam {
            base_health_check_param: BaseHealthCheckParam {
                interval: 10,
                timeout: 1000,
            },
            path: "/health".to_string(),
        };
        let health_check_clients = HealthCheckClient::new();
        let shared_config = create_test_shared_config(vec![RouteConfig {
            route_id: "config1".to_string(),
            health_check: Some(HealthCheckType::HttpGet(http_health_check_param.clone())),
            ..Default::default()
        }]);
        let route = RouteConfig {
            health_check: None,
            ..Default::default()
        };

        let result = submit_task(task_id, route, health_check_clients, shared_config);

        assert!(result.is_err());
    }
    #[tokio::test]
    async fn test_do_health_check_success() {
        let http_health_check_param = HttpHealthCheckParam {
            base_health_check_param: BaseHealthCheckParam {
                interval: 10,
                timeout: 1000,
            },

            path: "/health".to_string(),
        };
        let shared_config = create_test_shared_config(vec![RouteConfig {
            route_id: "config1".to_string(),
            health_check: Some(HealthCheckType::HttpGet(http_health_check_param.clone())),
            liveness_config: Some(LivenessConfig {
                min_liveness_count: 1,
            }),
            ..Default::default()
        }]);
        let mut health_check = HealthCheck::from_shared_config(shared_config);
        health_check.task_id_map.insert(
            TaskKey {
                route_id: "".to_string(),
                health_check_type: HealthCheckType::HttpGet(http_health_check_param.clone()),
                endpoint_list: vec!["".to_string()],
                min_liveness_count: 1,
            },
            32,
        );
        let result = health_check.do_health_check().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_get_endpoint_list_success() {
        let route = RouteConfig {
            route_id: "config1".to_string(),
            router: crate::vojo::router::Router::Random(RandomRoute::new(vec![
                "http://192.168.0.1:8888".to_string(),
            ])),
            ..Default::default()
        };
        let res = get_endpoint_list(route).await;
        assert!(res.len() == 1);
    }
}
