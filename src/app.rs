use std::sync::Arc;

use axum::{
    Router,
    routing::{get, post},
};
use tower_http::{catch_panic::CatchPanicLayer, trace::TraceLayer};

use crate::{capacity::Capacity, handlers};

pub struct AppState {
    pub capacity: Capacity,
}

pub fn create_app(capacity: Capacity) -> Router {
    let state = Arc::new(AppState { capacity });

    Router::new()
        .route("/internal/hc", get(handlers::healthcheck::healthcheck))
        .route("/vm", post(handlers::vm::create))
        .route("/vm/{id}", get(handlers::vm::get))
        .route("/vm/{id}", post(handlers::vm::delete))
        .layer(TraceLayer::new_for_http())
        .layer(CatchPanicLayer::new())
        .with_state(state)
}
