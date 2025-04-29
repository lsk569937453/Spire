use configuration_service::logger::start_logger;

use vojo::app_error::AppError;
use vojo::handler::Handler;

extern crate derive_builder;
mod configuration_service;
mod constants;
mod control_plane;
mod health_check;
mod middleware;
mod monitor;
mod proxy;
mod utils;
mod vojo;
use crate::constants::common_constants::DEFAULT_ADMIN_PORT;
use crate::constants::common_constants::ENV_ADMIN_PORT;
use std::env;
use std::num::ParseIntError;
use std::sync::Arc;
use tokio::sync::Mutex;
#[macro_use]
extern crate tracing;

use mimalloc::MiMalloc;
use tokio::runtime;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;
fn main() {
    if let Err(e) = run_async_task() {
        error!("main error, the error is {}!", e);
    }
}

fn run_async_task() -> Result<(), AppError> {
    let num = num_cpus::get();
    let rt = runtime::Builder::new_multi_thread()
        .worker_threads(num * 2)
        .enable_all()
        .build()
        .map_err(|e| AppError(e.to_string()))?;
    rt.block_on(async {
        let _work_guard = start_logger();
        let admin_port: i32 = env::var(ENV_ADMIN_PORT)
            .unwrap_or(String::from(DEFAULT_ADMIN_PORT))
            .parse()
            .map_err(|e: ParseIntError| AppError(e.to_string()))?;
        let mut handler = Handler {
            shared_app_config: Arc::new(Mutex::new(Default::default())),
        };
        handler.run(admin_port).await?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {

    // #[tokio::test]
    // async fn pool_key_value_get_set() {
    //     tokio::spawn(async move { block_start_with_error().await });
    //     sleep(Duration::from_millis(1000)).await;
    //     let listener = TcpListener::bind("0.0.0.0:5402");
    //     assert!(listener.is_ok());
    // }
    // #[test]
    // fn test_main_success() {
    //     let res = main_with_error(async move {
    //         let mut _a: i32 = 1;
    //         _a = 2;
    //         Ok(())
    //     });
    //     assert!(res.is_ok());
    // }
}
