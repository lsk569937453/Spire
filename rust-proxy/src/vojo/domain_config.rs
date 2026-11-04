use serde::{Deserialize, Serialize};
#[derive(Debug, Serialize, Deserialize, Clone)]
#[allow(dead_code)]
pub struct DomainsConfig {
    #[serde(rename = "domain")]
    pub domain_name: String,
}
