use axum::{http::StatusCode, response::IntoResponse, Json};

pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    /// Creates an API error with the given status code and message.
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    /// Convenience constructor for 500 Internal Server Error.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let body = serde_json::json!({ "error": self.message });
        (self.status, Json(body)).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        tracing::error!(error = ?e, "Internal error");
        Self::internal("Internal server error")
    }
}

impl From<firecracker_rs_sdk::Error> for ApiError {
    fn from(e: firecracker_rs_sdk::Error) -> Self {
        tracing::error!(error = ?e, "Firecracker SDK error");
        Self::internal("Internal server error")
    }
}
