use axum::{routing::get, Router};
use tower_http::catch_panic::CatchPanicLayer;

use crate::handlers;

pub fn create_app() -> Router {
    Router::new()
        .route("/internal/hc", get(handlers::healthcheck::healthcheck))
        .layer(CatchPanicLayer::new())
}
