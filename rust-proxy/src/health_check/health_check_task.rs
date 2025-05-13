use crate::constants::common_constants::TIMER_WAIT_SECONDS;
use crate::proxy::http1::http_client::HttpClients;
use crate::vojo::app_config::Route;
use crate::vojo::app_error::AppError;
use crate::vojo::cli::SharedConfig;
use crate::vojo::health_check::HealthCheckType;
use crate::vojo::health_check::HttpHealthCheckParam;
use bytes::Bytes;
use delay_timer::prelude::*;
use futures;
use futures::future::join_all;
use futures::FutureExt;
use http::Request;
use http::StatusCode;
use http_body_util::BodyExt;
use http_body_util::Full;
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
async fn get_endpoint_list(mut route: Route) -> Vec<String> {
    let mut result = vec![];
    let base_route_list = route.route_cluster.get_all_route().await.unwrap_or(vec![]);
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
        let app_config = self.shared_config.shared_data.lock().unwrap().clone();
        let handles = app_config
            .api_service_config
            .iter()
            .flat_map(|(_, item)| item.service_config.routes.clone())
            .filter(|item| item.health_check.is_some() && item.liveness_config.is_some())
            .map(|item| {
                tokio::spawn(async move {
                    let endpoint_list = get_endpoint_list(item.clone()).await;
                    let min_liveness_count =
                        item.liveness_config.clone().unwrap().min_liveness_count;
                    (
                        TaskKey::new(
                            item.route_id.clone(),
                            item.health_check.clone().unwrap(),
                            endpoint_list,
                            min_liveness_count,
                        ),
                        item,
                    )
                })
            });
        let route_list = join_all(handles)
            .await
            .iter()
            .filter(|item| item.is_ok())
            .map(|item| {
                let (a, b) = item.as_ref().unwrap();
                (a.clone(), b.clone())
            })
            .collect::<HashMap<TaskKey, Route>>();
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

async fn do_http_health_check(
    http_health_check_param: HttpHealthCheckParam,
    mut route: Route,
    timeout_number: i32,
    http_health_check_client: HealthCheckClient,
    shared_config: SharedConfig,
) -> Result<(), AppError> {
    info!("Do http health check,the route is {:?}!", route);
    let route_list = route.route_cluster.get_all_route().await?;
    let http_client = http_health_check_client.http_clients.clone();
    let mut set = JoinSet::new();
    for item in route_list {
        let http_client_shared = http_client.clone();
        let host_option = Url::parse(item.endpoint.as_str());
        if host_option.is_err() {
            error!("Parse host error,the error is {}", host_option.unwrap_err());
            continue;
        }

        let join_option = host_option
            .unwrap()
            .join(http_health_check_param.path.clone().as_str());
        if join_option.is_err() {
            error!("Parse host error,the error is {}", join_option.unwrap_err());
            continue;
        }

        let req = Request::builder()
            .uri(join_option.unwrap().to_string())
            .method("GET")
            .body(Full::new(Bytes::new()).boxed())
            .unwrap();
        let task_with_timeout = http_client_shared
            .clone()
            .request_http(req, timeout_number as u64);
        let cloned_route = route.clone();
        set.spawn(async {
            let res = task_with_timeout.await;
            (res, cloned_route, item)
        });
    }
    while let Some(response_result1) = set.join_next().await {
        match response_result1 {
            Ok((res, route, base_route)) => {
                let mut lock = shared_config.shared_data.lock().unwrap();
                let shared_route = lock
                    .api_service_config
                    .iter_mut()
                    .flat_map(|(_, item)| &mut item.service_config.routes)
                    .find(|item| item.route_id == route.route_id);
                let new_route = match shared_route {
                    Some(route) => route,
                    None => {
                        continue;
                    }
                };

                if let Ok(res) = res {
                    match res {
                        Ok(o) => {
                            if o.status() == StatusCode::OK {
                                let _ = new_route
                                    .route_cluster
                                    .update_route_alive(base_route.clone(), true);
                            }
                        }
                        Err(e) => {
                            error!(
                                "Request error,url:{}, the error is {}",
                                base_route.endpoint, e
                            );
                            let _ = new_route
                                .route_cluster
                                .update_route_alive(base_route, false);
                        }
                    }
                } else {
                    error!("Request time out, the url is {}", base_route.endpoint);
                    let _ = new_route
                        .route_cluster
                        .update_route_alive(base_route, false);
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
    route: Route,
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
                            health_check_client_shared,
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
        return task_builder
            .set_task_id(task_id)
            .set_frequency_repeated_by_seconds(base_param.interval as u64)
            .set_maximum_parallel_runnable_num(1)
            .spawn_async_routine(task)
            .map_err(|err| AppError(err.to_string()));
    }
    Err(AppError(String::from("Submit task error!")))
}
