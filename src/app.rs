use std::sync::{Arc, Mutex};

use axum::{Router, routing::get};
use tower_http::{catch_panic::CatchPanicLayer, trace::TraceLayer};

use crate::{capacity::Capacity, handlers};

pub struct AppState {
    pub capacity: Capacity,
}

pub fn create_app(capacity: Capacity) -> Router {
    let state = Arc::new(Mutex::new(AppState { capacity }));

    Router::new()
        .route("/internal/hc", get(handlers::healthcheck::healthcheck))
        .layer(TraceLayer::new_for_http())
        .layer(CatchPanicLayer::new())
        .with_state(state)
}
