use crate::vojo::app_config::{ApiService, AppConfig, RouteConfig};
use crate::vojo::app_error::AppError;
use crate::vojo::cli::ConvertArgs;
use crate::vojo::matcher::{MatcherRule, PathMatchType};
use crate::vojo::router::{BaseRoute, RandomRoute, Router};
use regex::Regex;
use std::collections::{HashMap, HashSet};
use url::Url;

pub async fn handle_convert_command(args: ConvertArgs) -> Result<AppConfig, AppError> {
    let yaml = std::fs::read_to_string(args.input_file)?;
    let spec = oas3::from_yaml(&yaml).map_err(|e| AppError(e.to_string()))?;

    let mut app_config = AppConfig::default();
    let mut services: HashMap<i32, ApiService> = HashMap::new();

    let default_server_url = "http://127.0.0.1:8080".to_string();
    let servers = if spec.servers.is_empty() {
        vec![default_server_url]
    } else {
        spec.servers.iter().map(|item| item.url.clone()).collect()
    };

    let default_upstream = servers
        .first()
        .cloned()
        .unwrap_or_else(|| "http://localhost:8000".to_string().clone());
    let paths = spec
        .paths
        .ok_or(AppError("No paths found in the OpenAPI spec".to_string()))?;
    let re = Regex::new(r"\{[^{}]+\}")?;
    for (path, path_item_ref) in paths.iter() {
        let operation = path_item_ref.methods();
        for (method, _) in operation.into_iter() {
            for server in &servers {
                let url = Url::parse(server)?;
                let port = url.port_or_known_default().unwrap_or(80) as i32;

                let service = services.entry(port).or_insert_with(|| {
                    let (sender, _) = tokio::sync::mpsc::channel(1);
                    ApiService {
                        listen_port: port,
                        server_type: match url.scheme() {
                            "https" => crate::vojo::app_config::ServiceType::Https,
                            _ => crate::vojo::app_config::ServiceType::Http,
                        },
                        sender,
                        ..Default::default()
                    }
                });

                let mut methods = HashSet::new();
                methods.insert(method.as_str().to_string());
                let (path_value, match_type) = if path.contains('{') && path.contains('}') {
                    let regex_path = format!("^{}$", re.replace_all(path, "[^/]+"));
                    (regex_path, PathMatchType::Regex)
                } else {
                    (path.clone(), PathMatchType::Exact)
                };
                let route_config = RouteConfig {
                    matchers: vec![
                        MatcherRule::Path {
                            value: path_value,
                            match_type,
                            regex: Default::default(),
                        },
                        MatcherRule::Method { values: methods },
                    ],
                    router: Router::Random(RandomRoute {
                        routes: vec![BaseRoute {
                            endpoint: default_upstream.clone(),
                            ..Default::default()
                        }],
                    }),
                    ..Default::default()
                };
                service.route_configs.push(route_config);
            }
        }
    }

    app_config.api_service_config = services;

    let output_yaml = serde_yaml::to_string(&app_config)
        .map_err(|e| AppError(format!("Failed to serialize to YAML: {e}")))?;

    println!("{output_yaml}");

    Ok(app_config)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::vojo::app_config::ServiceType;
    use crate::vojo::cli::ConvertArgs;
    use crate::vojo::matcher::{MatcherRule, PathMatchType};
    use crate::vojo::router::{RandomRoute, Router};

    use std::fs::File;
    use std::io::Write;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_handle_convert_command_no_servers() {
        let yaml_content = r#"
openapi: 3.0.0
info:
  title: Simple API
  version: 1.0.0
paths:
  /test:
    get:
      summary: A simple test endpoint
  /users/{id}:
    post:
      summary: Create a user
"#;
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test_spec.yml");
        let mut file = File::create(&file_path).unwrap();
        writeln!(file, "{yaml_content}").unwrap();

        let args = ConvertArgs {
            input_file: file_path.to_str().unwrap().to_string().into(),
            output_file: None,
            format: crate::vojo::cli::InputFormat::Openapi,
        };

        let result = handle_convert_command(args).await;
        assert!(result.is_ok());

        let app_config = result.unwrap();

        assert_eq!(app_config.api_service_config.len(), 1);
        let service = app_config.api_service_config.get(&8080).unwrap();
        assert_eq!(service.listen_port, 8080);
        assert!(matches!(service.server_type, ServiceType::Http));
        assert_eq!(service.route_configs.len(), 2);

        let test_route = service
            .route_configs
            .iter()
            .find(|r| {
                r.matchers.iter().any(|m| match m {
                    MatcherRule::Path { value, .. } => value == "/test",
                    _ => false,
                })
            })
            .unwrap();

        assert_eq!(test_route.matchers.len(), 2);
        assert!(
            matches!(&test_route.matchers[0], MatcherRule::Path { value, match_type, .. } if value == "/test" && *match_type == PathMatchType::Exact)
        );
        let methods = match &test_route.matchers[1] {
            MatcherRule::Method { values } => values,
            _ => panic!("Expected Method matcher"),
        };
        assert!(methods.contains("GET"));

        let user_route = service
            .route_configs
            .iter()
            .find(|r| {
                r.matchers.iter().any(|m| match m {
                    MatcherRule::Path { value, .. } => value.contains("users"),
                    _ => false,
                })
            })
            .unwrap();
        assert_eq!(user_route.matchers.len(), 2);
        assert!(
            matches!(&user_route.matchers[0], MatcherRule::Path { value, match_type, .. } if value == "^/users/[^/]+$" && *match_type == PathMatchType::Regex)
        );
        let methods = match &user_route.matchers[1] {
            MatcherRule::Method { values } => values,
            _ => panic!("Expected Method matcher"),
        };
        assert!(methods.contains("POST"));

        if let Router::Random(RandomRoute { routes }) = &user_route.router {
            assert_eq!(routes.len(), 1);
            assert_eq!(routes[0].endpoint, "http://127.0.0.1:8080".to_string());
        } else {
            panic!("Expected Random router");
        }
    }

    #[tokio::test]
    async fn test_handle_convert_command_with_servers() {
        let yaml_content = r#"
openapi: 3.0.0
info:
  title: Simple API with Servers
  version: 1.0.0
servers:
  - url: https://api.example.com:8443
  - url: http://localhost:9000
paths:
  /status:
    get:
      summary: A simple status endpoint
"#;
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test_spec_servers.yml");
        let mut file = File::create(&file_path).unwrap();
        writeln!(file, "{yaml_content}").unwrap();

        let args = ConvertArgs {
            input_file: file_path.to_str().unwrap().to_string().into(),
            output_file: None,
            format: crate::vojo::cli::InputFormat::Openapi,
        };

        let result = handle_convert_command(args).await;
        assert!(result.is_ok());

        let app_config = result.unwrap();

        assert_eq!(app_config.api_service_config.len(), 2);

        let https_service = app_config.api_service_config.get(&8443).unwrap();
        assert_eq!(https_service.listen_port, 8443);
        assert!(matches!(https_service.server_type, ServiceType::Https));
        assert_eq!(https_service.route_configs.len(), 1);
        let https_route = &https_service.route_configs[0];
        assert_eq!(https_route.matchers.len(), 2);
        assert!(
            matches!(&https_route.matchers[0], MatcherRule::Path { value, match_type, .. } if value == "/status" && *match_type == PathMatchType::Exact)
        );

        if let Router::Random(RandomRoute { routes }) = &https_route.router {
            assert_eq!(routes.len(), 1);
            assert_eq!(routes[0].endpoint, "https://api.example.com:8443");
        } else {
            panic!("Expected Random router");
        }

        let http_service = app_config.api_service_config.get(&9000).unwrap();
        assert_eq!(http_service.listen_port, 9000);
        assert!(matches!(http_service.server_type, ServiceType::Http));
        assert_eq!(http_service.route_configs.len(), 1);
        let http_route = &http_service.route_configs[0];
        assert_eq!(http_route.matchers.len(), 2);
        assert!(
            matches!(&http_route.matchers[0], MatcherRule::Path { value, match_type, .. } if value == "/status" && *match_type == PathMatchType::Exact)
        );
        if let Router::Random(RandomRoute { routes }) = &http_route.router {
            assert_eq!(routes.len(), 1);
            assert_eq!(routes[0].endpoint, "https://api.example.com:8443");
        } else {
            panic!("Expected Random router");
        }
    }
}
