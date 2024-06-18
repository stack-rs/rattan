use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::debug;

use crate::{
    control::{RattanOp, RattanOpEndpoint, RattanOpResult},
    error::{Error, RattanOpError},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct HttpConfig {
    pub enable: bool,
    pub port: u16,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            enable: false,
            port: 8086,
        }
    }
}

#[derive(Clone)]
struct ControlState {
    op_endpoint: RattanOpEndpoint,
}

pub struct HttpControlEndpoint {
    state: ControlState,
}

impl HttpControlEndpoint {
    pub fn new(op_endpoint: RattanOpEndpoint) -> Self {
        Self {
            state: ControlState { op_endpoint },
        }
    }

    pub fn router(&self) -> Router {
        Router::new()
            .route("/notify", post(send_notify))
            .route("/state", get(query_state))
            .route("/config/:id", post(config_device))
            .with_state(self.state.clone())
    }
}

async fn send_notify(
    State(state): State<ControlState>,
    Json(notify): Json<serde_json::Value>,
) -> Result<(), Error> {
    let notify = serde_json::from_value(notify)?;
    let res = state.op_endpoint.exec(RattanOp::SendNotify(notify)).await?;
    match res {
        RattanOpResult::SendNotify => Ok(()),
        _ => Err(RattanOpError::MismatchOpResError.into()),
    }
}

async fn query_state(State(state): State<ControlState>) -> Result<Json<Value>, Error> {
    let res = state.op_endpoint.exec(RattanOp::QueryState).await?;
    match res {
        RattanOpResult::QueryState(state) => Ok(Json(json!({"state": state}))),
        _ => Err(RattanOpError::MismatchOpResError.into()),
    }
}

async fn config_device(
    Path(id): Path<String>,
    State(state): State<ControlState>,
    Json(config): Json<serde_json::Value>,
) -> Result<(), Error> {
    debug!("Config device (id={}): {:?}", id, config);
    let config = serde_json::from_value(config)?;
    let res = state
        .op_endpoint
        .exec(RattanOp::ConfigDevice(id, config))
        .await?;
    match res {
        RattanOpResult::ConfigDevice => Ok(()),
        _ => Err(RattanOpError::MismatchOpResError.into()),
    }
}
