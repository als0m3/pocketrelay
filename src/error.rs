use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
#[derive(Debug, Clone)]
pub struct Error {
    pub status: u16,
    pub message: String,
    pub kind: String,
    pub code: Option<String>,
    pub param: Option<String>,
}
pub type Result<T> = std::result::Result<T, Error>;
impl Error {
    pub fn new(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            kind: if status == 429 {
                "rate_limit_error"
            } else if status >= 500 {
                "api_error"
            } else {
                "invalid_request_error"
            }
            .into(),
            code: None,
            param: None,
        }
    }
    pub fn code(mut self, code: &str) -> Self {
        self.code = Some(code.into());
        self
    }
    pub fn param(mut self, param: &str) -> Self {
        self.param = Some(param.into());
        self
    }
    pub fn body(&self) -> Value {
        json!({"detail":self.message,"error":{"message":self.message,"type":self.kind,"code":self.code,"param":self.param}})
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::new(500, e.to_string())
    }
}
impl From<anyhow::Error> for Error {
    fn from(e: anyhow::Error) -> Self {
        Self::new(500, e.to_string())
    }
}
impl From<serde_json::Error> for Error {
    fn from(_: serde_json::Error) -> Self {
        Self::new(400, "Invalid JSON")
    }
}
impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Self::new(
            502,
            format!("Provider connection failed: {}", e.without_url()),
        )
    }
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let retry = self.status == 429;
        let mut response = (
            StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(self.body()),
        )
            .into_response();
        if retry {
            response
                .headers_mut()
                .insert("retry-after", "60".parse().unwrap());
        }
        response
    }
}
