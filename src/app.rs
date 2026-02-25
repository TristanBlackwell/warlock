use std::{collections::HashMap, sync::Arc};

use axum::{
    routing::{delete, get, post},
    Router,
};
use firecracker_rs_sdk::instance::Instance;
use tokio::sync::Mutex;
use tower_http::{catch_panic::CatchPanicLayer, trace::TraceLayer};
use uuid::Uuid;

use crate::{capacity::Capacity, handlers};

pub struct AppState {
    pub capacity: Capacity,
    pub vms: Mutex<HashMap<Uuid, Instance>>,
}

pub fn create_app(capacity: Capacity) -> (Router, Arc<AppState>) {
    let state = Arc::new(AppState {
        capacity,
        vms: Mutex::new(HashMap::new()),
    });

    let router = Router::new()
        .route("/internal/hc", get(handlers::healthcheck::healthcheck))
        .route("/vm", post(handlers::vm::create))
        .route("/vm/{id}", get(handlers::vm::get))
        .route("/vm/{id}", delete(handlers::vm::delete))
        .layer(TraceLayer::new_for_http())
        .layer(CatchPanicLayer::new())
        .with_state(state.clone());

    (router, state)
}
