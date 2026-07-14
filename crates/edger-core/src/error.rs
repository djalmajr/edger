//! Domain errors for edger-core (no I/O error variants).

use std::fmt;

use serde::{Deserialize, Serialize};

/// Core vocabulary error (pure validation/parse/domain).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoreError {
    pub code: String,
    pub message: String,
}

impl CoreError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }

    pub fn validation(field: &str, message: impl Into<String>) -> Self {
        Self::new("VALIDATION_ERROR", format!("{field}: {}", message.into()))
    }

    pub fn parse(message: impl Into<String>) -> Self {
        Self::new("PARSE_ERROR", message)
    }
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for CoreError {}

/// Isolation boundary error (implemented by edger-isolation backends).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct IsolationError {
    pub code: String,
    pub message: String,
}

impl IsolationError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

impl fmt::Display for IsolationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for IsolationError {}
