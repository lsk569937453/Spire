use thiserror::Error;
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("Error is: {0}")]
pub struct AppError(pub String);
