use axum::{Json, http::StatusCode, response::IntoResponse};

#[derive(Debug)]
pub struct ApiError {
    pub(crate) status: StatusCode,
    pub(crate) message: String,
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

    /// Convenience constructor for 404 Not Found.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }

    /// Convenience constructor for 409 Conflict.
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }

    /// Convenience constructor for 422 Unprocessable Entity.
    pub fn unprocessable(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, message)
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

#[cfg(test)]
mod tests {
    use http_body_util::BodyExt;

    use super::*;

    /// Helper to convert an ApiError into its response parts.
    async fn response_parts(err: ApiError) -> (StatusCode, serde_json::Value) {
        let response = err.into_response();
        let status = response.status();
        let body = response.into_body();
        let bytes = body.collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        (status, json)
    }

    #[tokio::test]
    async fn into_response_returns_json_error_body() {
        let err = ApiError::not_found("VM not found");
        let (status, json) = response_parts(err).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(json["error"], "VM not found");
    }

    #[tokio::test]
    async fn into_response_internal_error() {
        let err = ApiError::internal("something broke");
        let (status, json) = response_parts(err).await;

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(json["error"], "something broke");
    }

    #[tokio::test]
    async fn into_response_conflict() {
        let err = ApiError::conflict("resource busy");
        let (status, json) = response_parts(err).await;

        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(json["error"], "resource busy");
    }

    #[tokio::test]
    async fn into_response_unprocessable() {
        let err = ApiError::unprocessable("bad input");
        let (status, json) = response_parts(err).await;

        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(json["error"], "bad input");
    }

    #[tokio::test]
    async fn from_anyhow_obfuscates_error_message() {
        let original = anyhow::anyhow!("secret database connection string leaked");
        let api_err: ApiError = original.into();
        let (status, json) = response_parts(api_err).await;

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        // Client must NOT see the original error message
        assert_eq!(json["error"], "Internal server error");
    }
}
