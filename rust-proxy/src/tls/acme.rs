//! Certificate operations shared by the ACME client and the renewal task.
//!
//! The trait exists so the renewal task can be exercised without touching the
//! network. The on-demand HTTP issuance endpoint was removed together with the
//! rest of the control-plane API: certificates are now obtained at startup and
//! renewed by the background task only.
use crate::vojo::app_config::AcmeConfig;
use crate::vojo::app_error::AppError;

pub trait LetsEncryptActions: Send + Sync {
    async fn obtain_certificate(&self, acme: &AcmeConfig) -> Result<(String, String), AppError>;
}
