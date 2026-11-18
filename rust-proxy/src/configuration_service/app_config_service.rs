use crate::app_error;
#[cfg(any(unix, test))]
use crate::configuration_service::config_loader;
use crate::health_check::health_check_task::HealthCheck;
use crate::proxy::http1::http_proxy::HttpProxy;
use crate::proxy::http2::grpc_proxy::GrpcProxy;
use crate::proxy::tcp::tcp_proxy::TcpProxy;
use crate::tls::certificate_manager::CertificateManager;
#[cfg(any(unix, test))]
use crate::vojo::app_config::AppConfig;
use crate::vojo::app_config::ServiceType;
use crate::vojo::app_error::AppError;
use crate::vojo::cli::SharedConfig;
#[cfg(any(unix, test))]
use std::collections::{BTreeSet, HashMap};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::mpsc;

/// A configured service that still needs its listener task.
type Spawnable = (i32, ServiceType, mpsc::Sender<()>, mpsc::Receiver<()>);

/// What a reload has to add and remove compared with the running listeners.
///
/// Reloading is driven by `SIGHUP`, which does not exist on Windows: the
/// machinery is only compiled where it is reachable (or under test).
#[cfg(any(unix, test))]
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReloadPlan {
    pub added: Vec<i32>,
    pub removed: Vec<i32>,
}

pub async fn init(shared_config: SharedConfig) -> Result<(), AppError> {
    let cloned_config = shared_config.clone();
    tokio::task::spawn(async {
        let mut health_check = HealthCheck::from_shared_config(cloned_config);
        health_check.start_health_check_loop().await;
    });

    {
        let app_config = shared_config.shared_data.read()?;
        let mut certificate_manager = CertificateManager::new(Arc::new(app_config.clone()));
        drop(app_config);
        certificate_manager.start_renewal_task();
    }

    let services = attach_shutdown_channels(&shared_config)?;

    // Pre-flight the ports so that a listener that cannot be bound fails the
    // whole startup instead of being logged and silently never serving traffic.
    preflight_ports(services.iter().map(|(port, _, _, _)| *port)).await?;

    spawn_services(&shared_config, services);
    Ok(())
}

/// Gives every configured service a shutdown channel and returns the services
/// that still need a listener task.
fn attach_shutdown_channels(shared_config: &SharedConfig) -> Result<Vec<Spawnable>, AppError> {
    // Take the write lock only for the bookkeeping below, and release it before
    // awaiting anything: a std lock guard must not be held across an `.await`.
    let mut app_config = shared_config.shared_data.write()?;
    let mut services = Vec::with_capacity(app_config.api_service_config.len());
    for item in app_config.api_service_config.values_mut() {
        let (sender, receiver) = mpsc::channel::<()>(1000);
        item.sender = sender.clone();
        services.push((item.listen_port, item.server_type.clone(), sender, receiver));
    }
    Ok(services)
}

/// Binds every port once and releases it again; the proxy binds it for real
/// right after. Returns the first failure, so a reload can abort before it
/// commits anything.
async fn preflight_ports(ports: impl IntoIterator<Item = i32>) -> Result<(), AppError> {
    for port in ports {
        let addr = SocketAddr::from(([0, 0, 0, 0], port as u16));
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => drop(listener),
            Err(e) => {
                return Err(app_error!(
                    "Failed to bind listener on {addr} (configured under 'servers'): {e}"
                ));
            }
        }
    }
    Ok(())
}

fn spawn_services(shared_config: &SharedConfig, services: Vec<Spawnable>) {
    for (port, server_type, _sender, receiver) in services {
        let mapping_key = format!("{port}-{server_type}");
        let cloned_config = shared_config.clone();
        tokio::task::spawn(async move {
            if let Err(err) =
                start_proxy(cloned_config, port, receiver, server_type, mapping_key).await
            {
                error!("Proxy on port {port} stopped: {err}");
            }
        });
    }
}

/// Decides what a reload changes, and refuses what cannot be applied in place.
///
/// A protocol change on a port that is already listening cannot be applied
/// atomically: the old socket has to close before the new one can bind, so a
/// failure would leave the port dead. Such a change requires a restart.
#[cfg(any(unix, test))]
pub fn plan_reload(current: &AppConfig, new: &AppConfig) -> Result<ReloadPlan, AppError> {
    for (port, running) in &current.api_service_config {
        let Some(replacement) = new.api_service_config.get(port) else {
            continue;
        };
        if running.server_type != replacement.server_type {
            return Err(app_error!(
                "Changing the protocol of port {port} from '{}' to '{}' requires a restart",
                running.server_type,
                replacement.server_type
            ));
        }
        // The TLS listener (and its SNI resolver) is built once, when the
        // listener starts: a new domain list cannot be applied to a live one.
        if running.domain_config != replacement.domain_config {
            return Err(app_error!(
                "Changing the 'domains' of port {port} requires a restart (the TLS listener is built at startup)"
            ));
        }
    }

    let current_ports: BTreeSet<i32> = current.api_service_config.keys().copied().collect();
    let new_ports: BTreeSet<i32> = new.api_service_config.keys().copied().collect();
    Ok(ReloadPlan {
        added: new_ports.difference(&current_ports).copied().collect(),
        removed: current_ports.difference(&new_ports).copied().collect(),
    })
}

/// Applies a parsed configuration to the running process.
///
/// Two-phase on purpose: every listener that has to appear is bound *before*
/// anything is committed, so a reload that cannot bind a port leaves the running
/// configuration — and every listener — untouched.
#[cfg(any(unix, test))]
pub async fn apply_reload(
    shared_config: &SharedConfig,
    mut new_config: AppConfig,
) -> Result<ReloadPlan, AppError> {
    let (plan, running_senders) = {
        let current = shared_config.shared_data.read()?;
        let plan = plan_reload(&current, &new_config)?;
        let senders = current
            .api_service_config
            .iter()
            .map(|(port, service)| (*port, service.sender.clone()))
            .collect::<HashMap<i32, mpsc::Sender<()>>>();
        (plan, senders)
    };

    preflight_ports(plan.added.iter().copied()).await?;

    let mut new_services: Vec<Spawnable> = Vec::new();
    for (port, service) in new_config.api_service_config.iter_mut() {
        match running_senders.get(port) {
            // A listener that survives the reload keeps its shutdown channel.
            Some(sender) => service.sender = sender.clone(),
            None => {
                let (sender, receiver) = mpsc::channel::<()>(1000);
                service.sender = sender.clone();
                new_services.push((*port, service.server_type.clone(), sender, receiver));
            }
        }
    }

    {
        let mut current = shared_config.shared_data.write()?;
        *current = new_config;
    }

    // Only after the new configuration is live: a listener that is going away
    // must not be able to stop traffic that the new configuration still routes.
    for port in &plan.removed {
        if let Some(sender) = running_senders.get(port) {
            let _ = sender.send(()).await;
        }
    }
    spawn_services(shared_config, new_services);
    Ok(plan)
}

/// SIGHUP path: re-read the configuration file and apply it.
///
/// A failure here must never disturb the running process: the caller logs it and
/// keeps serving with the previous configuration, which is also what nginx does.
#[cfg(any(unix, test))]
pub async fn reload_from_disk(shared_config: &SharedConfig) -> Result<(), AppError> {
    let path = shared_config.config_path.as_ref();
    let mut new_config = config_loader::read_app_config(path).await?;

    // Bound once at startup, never rebound: keep the running values so that the
    // in-memory configuration always describes what the process actually serves.
    {
        let current = shared_config.shared_data.read()?;
        if current.observability != new_config.observability {
            error!(
                "'observability' changes require a restart; keeping the running listener on '{}'",
                current.observability.listen
            );
            new_config.observability = current.observability.clone();
        }
        if current.log_level != new_config.log_level {
            warn!("'log_level' changes require a restart; the running logger is kept.");
            new_config.log_level = current.log_level.clone();
        }
        if acme_domains(&current) != acme_domains(&new_config) {
            warn!(
                "The certificate renewal task keeps the domains it started with; restart Spire to \
                 pick up the changed domain list."
            );
        }
    }

    // Recompile the plugin directory first (a package may have been rebuilt on
    // disk) and validate every reference against it before the new configuration
    // is committed.
    #[cfg(feature = "wasm")]
    {
        if let Err(e) = crate::wasm::reload() {
            warn!("wasm: plugin reload failed, keeping the previous generation: {e}");
        }
        crate::wasm::validate_references(&new_config)?;
    }
    #[cfg(not(feature = "wasm"))]
    reject_wasm_middlewares(&new_config)?;

    let plan = apply_reload(shared_config, new_config).await?;
    info!(
        "Configuration reloaded from '{}': {} listener(s) added, {} removed",
        path.display(),
        plan.added.len(),
        plan.removed.len()
    );
    Ok(())
}

/// Without WASM support a `kind: wasm` middleware is a hard error: a reload must
/// never silently accept a route this build cannot serve.
#[cfg(all(not(feature = "wasm"), any(unix, test)))]
fn reject_wasm_middlewares(config: &AppConfig) -> Result<(), AppError> {
    for service in config.api_service_config.values() {
        for route in &service.route_configs {
            for middleware in route.middlewares.iter().flatten() {
                if matches!(
                    middleware,
                    crate::middleware::middlewares::MiddleWares::Wasm(_)
                ) {
                    return Err(app_error!(
                        "route {} uses `kind: wasm`, but this build has no WASM plugin support; \
                         rebuild with `--features wasm`",
                        route.route_id
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Domains the certificate renewal task was started with.
#[cfg(any(unix, test))]
fn acme_domains(config: &AppConfig) -> BTreeSet<String> {
    config
        .api_service_config
        .values()
        .filter(|service| {
            service.server_type == ServiceType::Https
                || service.server_type == ServiceType::Http2Tls
        })
        .flat_map(|service| service.domain_config.iter().cloned())
        .collect()
}

pub async fn start_proxy(
    shared_config: SharedConfig,
    port: i32,
    channel: mpsc::Receiver<()>,
    server_type: ServiceType,
    mapping_key: String,
) -> Result<(), AppError> {
    if server_type == ServiceType::Http {
        let mut http_proxy = HttpProxy {
            shared_config,
            port,
            channel,
            mapping_key: Arc::from(mapping_key.as_str()),
        };
        http_proxy.start_http_server().await
    } else if server_type == ServiceType::Https {
        let mut http_proxy = HttpProxy {
            shared_config: shared_config.clone(),
            port,
            channel,
            mapping_key: Arc::from(mapping_key.as_str()),
        };
        let domains = {
            let config = shared_config.shared_data.write()?;
            config
                .api_service_config
                .get(&port)
                .ok_or(app_error!(
                    "Missing 'domains' configuration for HTTPS service on port {}",
                    port
                ))?
                .domain_config
                .to_vec()
        };
        http_proxy.start_https_server(domains).await
    } else if server_type == ServiceType::Tcp {
        let mut tcp_proxy = TcpProxy {
            shared_config,
            port,
            mapping_key,
            channel,
        };
        tcp_proxy.start_proxy().await
    } else if server_type == ServiceType::Http2 {
        let mut grpc_proxy = GrpcProxy {
            shared_config,
            port,
            mapping_key,
            channel,
        };
        grpc_proxy.start_proxy().await
    } else {
        let mut grpc_proxy = GrpcProxy {
            shared_config: shared_config.clone(),
            port,
            mapping_key,
            channel,
        };
        let domains = {
            let config = shared_config.shared_data.write()?;
            config
                .api_service_config
                .get(&port)
                .ok_or(app_error!(
                    "Missing 'domains' configuration for HTTPS service on port {}",
                    port
                ))?
                .domain_config
                .to_vec()
        };
        grpc_proxy.start_tls_proxy(domains).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vojo::app_config::ApiService;
    use std::time::Duration;
    use tokio::sync::mpsc;

    fn service(port: i32, server_type: ServiceType) -> ApiService {
        ApiService {
            listen_port: port,
            server_type,
            domain_config: vec![],
            route_configs: vec![],
            ..Default::default()
        }
    }

    fn config_with(services: Vec<ApiService>) -> AppConfig {
        AppConfig {
            api_service_config: services
                .into_iter()
                .map(|service| (service.listen_port, service))
                .collect(),
            ..Default::default()
        }
    }

    fn shared(config: AppConfig) -> SharedConfig {
        SharedConfig::from_app_config_with_path(config, "config.yaml")
    }

    /// A port that is very likely free: bound once, released immediately.
    fn free_port() -> i32 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("free port");
        listener.local_addr().expect("local addr").port() as i32
    }

    #[test]
    fn plan_reload_computes_added_and_removed_ports() {
        let current = config_with(vec![
            service(8080, ServiceType::Http),
            service(8081, ServiceType::Http),
        ]);
        let new = config_with(vec![
            service(8081, ServiceType::Http),
            service(8082, ServiceType::Http),
        ]);

        let plan = plan_reload(&current, &new).expect("plan");

        assert_eq!(plan.added, vec![8082]);
        assert_eq!(plan.removed, vec![8080]);
    }

    #[test]
    fn plan_reload_rejects_a_protocol_change_on_the_same_port() {
        let current = config_with(vec![service(443, ServiceType::Https)]);
        let new = config_with(vec![service(443, ServiceType::Http)]);

        let error = plan_reload(&current, &new).expect_err("protocol change needs a restart");

        assert!(
            error.0.contains("requires a restart"),
            "unexpected error: {}",
            error.0
        );
    }

    #[test]
    fn plan_reload_rejects_a_domain_change_on_the_same_port() {
        let mut current_service = service(443, ServiceType::Https);
        current_service.domain_config = vec!["a.example.com".to_string()];
        let mut new_service = service(443, ServiceType::Https);
        new_service.domain_config = vec!["a.example.com".to_string(), "b.example.com".to_string()];

        let error = plan_reload(
            &config_with(vec![current_service]),
            &config_with(vec![new_service]),
        )
        .expect_err("domain change needs a restart");

        assert!(
            error.0.contains("requires a restart"),
            "unexpected error: {}",
            error.0
        );
    }

    #[tokio::test]
    async fn apply_reload_adds_and_removes_listeners() {
        let kept = free_port();
        let added = free_port();
        let removed = free_port();

        let shared_config = shared(config_with(vec![
            service(kept, ServiceType::Http),
            service(removed, ServiceType::Http),
        ]));
        init(shared_config.clone()).await.expect("init");

        let old_sender = shared_config
            .shared_data
            .read()
            .expect("read")
            .api_service_config
            .get(&kept)
            .expect("service")
            .sender
            .clone();

        let new = config_with(vec![
            service(kept, ServiceType::Http),
            service(added, ServiceType::Http),
        ]);
        let plan = apply_reload(&shared_config, new).await.expect("reload");

        assert_eq!(plan.added, vec![added]);
        assert_eq!(plan.removed, vec![removed]);

        {
            let config = shared_config.shared_data.read().expect("read");
            assert!(config.api_service_config.contains_key(&added));
            assert!(!config.api_service_config.contains_key(&removed));
            // A surviving listener keeps its shutdown channel.
            assert!(
                config
                    .api_service_config
                    .get(&kept)
                    .expect("service")
                    .sender
                    .same_channel(&old_sender)
            );
        }

        shutdown_listeners_for_test(&shared_config, kept).await;
    }

    #[tokio::test]
    async fn apply_reload_keeps_the_old_config_when_a_port_cannot_be_bound() {
        // Hold a port so the reload cannot bind it.
        let squatter = std::net::TcpListener::bind("0.0.0.0:0").expect("bind");
        let busy_port = squatter.local_addr().expect("addr").port() as i32;
        let kept = free_port();

        let shared_config = shared(config_with(vec![service(kept, ServiceType::Http)]));
        init(shared_config.clone()).await.expect("init");

        let new = config_with(vec![
            service(kept, ServiceType::Http),
            service(busy_port, ServiceType::Http),
        ]);
        let error = apply_reload(&shared_config, new)
            .await
            .expect_err("bind must fail");

        assert!(error.0.contains("Failed to bind"), "{}", error.0);
        {
            let config = shared_config.shared_data.read().expect("read");
            assert!(!config.api_service_config.contains_key(&busy_port));
        }
        drop(squatter);

        shutdown_listeners_for_test(&shared_config, kept).await;
    }

    #[tokio::test]
    async fn start_proxy_http() {
        let shared_config = SharedConfig::from_app_config(AppConfig::default());
        let (tx, rx) = mpsc::channel(1);

        let proxy_task = tokio::spawn(start_proxy(
            shared_config,
            8080,
            rx,
            ServiceType::Http,
            "test-http".to_string(),
        ));

        tokio::time::sleep(Duration::from_millis(10)).await; // Give it time to start
        let res = tx.send(()).await;
        assert!(res.is_ok(), "Expected Ok, got {res:?}");
        let result = proxy_task.await.expect("Proxy task panicked");
        assert!(result.is_ok(), "Expected Ok, got {result:?}");
    }

    /// Builds a config with one HTTPS/HTTP2TLS service on `port`.
    fn https_shared_config(port: i32, server_type: ServiceType) -> SharedConfig {
        SharedConfig::from_app_config(AppConfig {
            api_service_config: HashMap::from([(
                port,
                ApiService {
                    listen_port: port,
                    server_type: server_type.clone(),
                    domain_config: vec!["localhost".to_string()],
                    route_configs: vec![],
                    ..Default::default()
                },
            )]),
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn test_start_proxy_https_success() {
        let shared_config = https_shared_config(8081, ServiceType::Https);
        let (tx, rx) = mpsc::channel(1);

        let proxy_task = tokio::spawn(start_proxy(
            shared_config,
            8081,
            rx,
            ServiceType::Https,
            "test-https".to_string(),
        ));
        tokio::time::sleep(Duration::from_millis(100)).await;
        let _ = tx.send(()).await;
        let result = proxy_task.await.expect("Proxy task panicked");
        assert!(
            result.is_ok(),
            "the HTTPS listener should start and stop cleanly, got {result:?}"
        );
    }

    #[tokio::test]
    async fn test_start_proxy_https_missing_domains() {
        let shared_config = SharedConfig::from_app_config(AppConfig::default());
        let (_tx, rx) = mpsc::channel(1); // tx not used as it should fail before listening

        let result = start_proxy(
            shared_config,
            8082,
            rx,
            ServiceType::Https,
            "test-https-fail".to_string(),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err(),
            AppError("Missing 'domains' configuration for HTTPS service on port 8082".to_string())
        );
    }

    #[tokio::test]
    async fn test_start_proxy_tcp() {
        let shared_config = SharedConfig::from_app_config(AppConfig::default());
        let (tx, rx) = mpsc::channel(1);

        let proxy_task = tokio::spawn(start_proxy(
            shared_config,
            8083,
            rx,
            ServiceType::Tcp,
            "test-tcp".to_string(),
        ));
        tokio::time::sleep(Duration::from_millis(10)).await;
        tx.send(()).await.expect("Failed to send shutdown signal");
        let result = proxy_task.await.expect("Proxy task panicked");
        assert!(result.is_ok(), "Expected Ok, got {result:?}");
    }

    #[tokio::test]
    async fn test_start_proxy_http2() {
        let shared_config = SharedConfig::from_app_config(AppConfig::default());
        let (tx, rx) = mpsc::channel(1);

        let proxy_task = tokio::spawn(start_proxy(
            shared_config,
            8084,
            rx,
            ServiceType::Http2,
            "test-http2".to_string(),
        ));
        tokio::time::sleep(Duration::from_millis(10)).await;
        tx.send(()).await.expect("Failed to send shutdown signal");
        let result = proxy_task.await.expect("Proxy task panicked");
        assert!(result.is_ok(), "Expected Ok, got {result:?}");
    }

    #[tokio::test]
    async fn test_start_proxy_grpc_tls_success() {
        let shared_config = SharedConfig::from_app_config(AppConfig::default());
        let (tx, rx) = mpsc::channel(1);

        let proxy_task = tokio::spawn(start_proxy(
            shared_config,
            8085,
            rx,
            ServiceType::Http2Tls,
            "test-grpc-tls".to_string(),
        ));
        tokio::time::sleep(Duration::from_millis(10)).await;
        let tt = tx.send(()).await;
        println!("{tt:?}");
        let result = proxy_task.await.expect("Proxy task panicked");
        assert!(result.is_err(), "Expected Ok, got {result:?}");
    }

    #[tokio::test]
    async fn test_start_proxy_grpc_tls_missing_domains() {
        let shared_config = SharedConfig::from_app_config(AppConfig::default());
        let (_tx, rx) = mpsc::channel(1);

        let result = start_proxy(
            shared_config,
            8086,
            rx,
            ServiceType::Http2Tls,
            "test-grpc-tls-fail".to_string(),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err(),
            AppError("Missing 'domains' configuration for HTTPS service on port 8086".to_string())
        );
    }

    #[tokio::test]
    async fn test_init_function() {
        let services_to_init = vec![("http_service".to_string(), 9001, ServiceType::Http)];
        let shared_config = SharedConfig::from_app_config(AppConfig {
            api_service_config: HashMap::from([(
                9001,
                ApiService {
                    listen_port: 9001,

                    ..Default::default()
                },
            )]),
            ..Default::default()
        });

        let init_result = init(shared_config.clone()).await;
        assert!(init_result.is_ok());
        {
            let app_config_guard = shared_config.shared_data.read().unwrap();
            for (_, port, service_conf) in &services_to_init {
                let api_service = app_config_guard
                    .api_service_config
                    .get(&9001)
                    .expect("Service not found in config after init");
                assert_eq!(api_service.listen_port, *port);
                assert_eq!(api_service.server_type, service_conf.clone());
            }
        }
        shutdown_listeners_for_test(&shared_config, 9001).await;
    }

    #[tokio::test]
    async fn reload_from_disk_applies_the_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("config.yaml");
        let port = free_port();
        std::fs::write(
            &path,
            format!(
                "servers:\n  - listen: {port}\n    protocol: http\n    routes:\n      - route_id: api\n        matchers:\n          - kind: path\n            value: /\n        forward_to: http://127.0.0.1:9393\n"
            ),
        )
        .expect("write config");

        let shared_config = SharedConfig::from_app_config_with_path(config_with(vec![]), &path);
        reload_from_disk(&shared_config).await.expect("reload");

        assert!(
            shared_config
                .shared_data
                .read()
                .expect("read")
                .api_service_config
                .contains_key(&port)
        );
        shutdown_listeners_for_test(&shared_config, port).await;
    }

    #[tokio::test]
    async fn reload_from_disk_keeps_the_running_config_on_a_bad_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, "admin_port: 8870\nservers: []\n").expect("write config");

        let running_port = free_port();
        let shared_config = SharedConfig::from_app_config_with_path(
            config_with(vec![service(running_port, ServiceType::Http)]),
            &path,
        );

        let error = reload_from_disk(&shared_config)
            .await
            .expect_err("a removed field must fail the reload");

        assert!(error.0.contains("admin_port"), "{}", error.0);
        let config = shared_config.shared_data.read().expect("read");
        assert!(config.api_service_config.contains_key(&running_port));
    }

    #[tokio::test]
    async fn reload_from_disk_refuses_a_protocol_change() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("config.yaml");
        let port = free_port();
        std::fs::write(
            &path,
            format!("servers:\n  - listen: {port}\n    protocol: http\n    routes: []\n"),
        )
        .expect("write config");

        let shared_config = SharedConfig::from_app_config_with_path(
            config_with(vec![service(port, ServiceType::Https)]),
            &path,
        );

        let error = reload_from_disk(&shared_config)
            .await
            .expect_err("protocol change needs a restart");

        assert!(error.0.contains("requires a restart"), "{}", error.0);
    }

    /// Stops the listener on `port` (and any other listening port) so a test
    /// never leaks a task that keeps the socket bound.
    async fn shutdown_listeners_for_test(shared_config: &SharedConfig, _port: i32) {
        let senders = {
            let Ok(config) = shared_config.shared_data.read() else {
                return;
            };
            config
                .api_service_config
                .values()
                .map(|service| service.sender.clone())
                .collect::<Vec<_>>()
        };
        for sender in senders {
            let _ = sender.send(()).await;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
