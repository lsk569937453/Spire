use clap::Parser;

use super::app_config::AppConfig;
use std::sync::Arc;
use std::sync::Mutex;
#[derive(Parser, Debug, Clone)]
#[command(name = "Spire", version = "1.0", about = "The Spire API Gateway", long_about = None) ]
pub struct Cli {
    /// The config file path
    #[arg(short = 'f', long, default_value = "config.yaml")]
    pub config_path: String,
}
#[derive(Clone)]
pub struct SharedConfig {
    pub shared_data: Arc<Mutex<AppConfig>>,
}
impl SharedConfig {
    pub fn from_app_config(app_config: AppConfig) -> Self {
        Self {
            shared_data: Arc::new(Mutex::new(app_config)),
        }
    }
}
