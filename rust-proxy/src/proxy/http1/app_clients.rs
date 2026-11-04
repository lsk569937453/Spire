use crate::proxy::http1::grpc_client::GrpcClients;
use crate::proxy::http1::http_client::HttpClients;
use crate::vojo::app_config::ApiService;
use crate::vojo::app_error::AppError;
use crate::vojo::cli::SharedConfig;
#[derive(Clone)]
pub struct AppClients {
    pub http: HttpClients,
    pub grpc: Option<GrpcClients>,
}
impl AppClients {
    pub async fn new(shared_config: SharedConfig, port: i32) -> Result<AppClients, AppError> {
        let http_client = HttpClients::new();

        let grpc_configs = {
            let lock = shared_config.shared_data.read()?;
            if let Some(api_service) = lock.api_service_config.get(&port) {
                extract_grpc_configs(api_service)?
            } else {
                Vec::new()
            }
        };

        let grpc_clients = if grpc_configs.is_empty() {
            None
        } else {
            match GrpcClients::new(grpc_configs).await {
                Ok(clients) => Some(clients),
                Err(e) => {
                    error!(
                        "Failed to initialize gRPC clients for port {port}, proceeding without them. Error: {e}"
                    );
                    None
                }
            }
        };

        Ok(AppClients {
            http: http_client,
            grpc: grpc_clients,
        })
    }
}
fn extract_grpc_configs(api_service: &ApiService) -> Result<Vec<(String, String)>, AppError> {
    let mut grpc_list = Vec::new();

    for item in &api_service.route_configs {
        let all_routes_result = item.router.get_all_route();
        if all_routes_result.is_err() {
            return Ok(vec![]);
        }

        let grpc_endpoints: Vec<_> = all_routes_result?
            .into_iter()
            .filter(|route| route.endpoint.to_lowercase().starts_with("grpc://"))
            .map(|route| route.endpoint)
            .collect();

        if grpc_endpoints.is_empty() {
            continue;
        }

        let transcode_config = item.transcode.as_ref().ok_or_else(|| {
            AppError::from("Transcode config is required for gRPC routes but was not found.")
        })?;

        for endpoint in grpc_endpoints {
            // 步骤 5: 只克隆需要的数据
            grpc_list.push((endpoint, transcode_config.proto_descriptor_set.clone()));
        }
    }

    Ok(grpc_list)
}
