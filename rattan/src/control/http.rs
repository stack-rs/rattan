use std::{collections::HashMap, sync::Arc};

use crate::devices::ControlInterface;
use axum::{
    self,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use axum_macros::debug_handler;
use serde_json::{json, Value};
use tracing::info;

use super::ControlEndpoint;

pub struct HttpControlEndpoint {
    router: Option<Router>,
    state: ControlState,
}

#[derive(Clone)]
struct ControlState {
    control_interfaces: HashMap<usize, Arc<dyn HttpControlInterface>>,
}

impl ControlState {
    fn new() -> Self {
        Self {
            control_interfaces: HashMap::new(),
        }
    }
}

pub trait HttpControlInterface: Send + Sync {
    fn config_device(&self, payload: serde_json::Value) -> (StatusCode, Json<Value>);
}

impl<T> HttpControlInterface for T
where
    T: ControlInterface,
{
    fn config_device(&self, payload: serde_json::Value) -> (StatusCode, Json<Value>) {
        match serde_json::from_value(payload) {
            Ok(payload) => match self.set_config(payload) {
                Ok(_) => (StatusCode::OK, Json(json!({"status": "ok"}))),
                Err(_) => (StatusCode::BAD_REQUEST, Json(json!({"status": "fail"}))),
            },
            Err(_) => (StatusCode::BAD_REQUEST, Json(json!({"status": "fail"}))),
        }
    }
}

#[debug_handler]
async fn control_device(
    Path(id): Path<usize>,
    State(state): State<ControlState>,
    Json(config): Json<serde_json::Value>,
) -> (StatusCode, Json<Value>) {
    info!("config device: {} {:?}", id, config);
    match state.control_interfaces.get(&id) {
        Some(control_interface) => control_interface.as_ref().config_device(config),
        None => (StatusCode::NOT_FOUND, Json(json!({"status": "fail"}))),
    }
}

impl ControlEndpoint for HttpControlEndpoint {
    fn register_device(
        &mut self,
        index: usize,
        control_interface: std::sync::Arc<impl ControlInterface>,
    ) {
        self.state
            .control_interfaces
            .insert(index, control_interface);
        self.update_router();
    }
}

impl Default for HttpControlEndpoint {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpControlEndpoint {
    pub fn new() -> Self {
        Self {
            router: Some(Router::new().route(
                "/health",
                get(|| async { (StatusCode::OK, Json(json!({"status": "ok"}))) }),
            )),
            state: ControlState::new(),
        }
    }

    pub fn update_router(&mut self) {
        self.router = Some(
            Router::new()
                .route(
                    "/health",
                    get(|| async { (StatusCode::OK, Json(json!({"status": "ok"}))) }),
                )
                .route("/control/:id", post(control_device))
                .with_state(self.state.clone()),
        );
    }

    pub fn router(&self) -> Router {
        self.router.clone().unwrap()
    }
}
