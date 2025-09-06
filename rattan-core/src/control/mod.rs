#[cfg(feature = "http")]
pub mod http;

use std::{
    fmt::Debug,
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
};

use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, instrument, warn};

use crate::{
    core::RattanState,
    error::{Error, RattanCoreError, RattanOpError},
};

#[cfg(feature = "serde")]
use crate::cells::JsonControlInterface;
#[cfg(feature = "serde")]
use std::collections::HashMap;

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, PartialEq)]
pub enum RattanNotify {
    Start,
    FirstPacket,
}

#[derive(Clone)]
pub enum RattanOp {
    SendNotify(RattanNotify),
    QueryState,
    #[cfg(feature = "serde")]
    AddControlInterface(String, Arc<dyn JsonControlInterface>),
    #[cfg(feature = "serde")]
    ConfigCell(String, serde_json::Value),
}

impl Debug for RattanOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RattanOp::SendNotify(notify) => write!(f, "SendNotify({notify:?})"),
            RattanOp::QueryState => write!(f, "QueryState"),
            #[cfg(feature = "serde")]
            RattanOp::AddControlInterface(id, _) => write!(f, "AddControlInterface({id})"),
            #[cfg(feature = "serde")]
            RattanOp::ConfigCell(id, v) => write!(f, "ConfigCell({id}, {v:?})"),
        }
    }
}

#[derive(Clone)]
pub enum RattanOpResult {
    SendNotify,
    QueryState(RattanState),
    #[cfg(feature = "serde")]
    AddControlInterface(Option<Arc<dyn JsonControlInterface>>),
    #[cfg(feature = "serde")]
    ConfigCell,
}

impl Debug for RattanOpResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RattanOpResult::SendNotify => write!(f, "SendNotify"),
            RattanOpResult::QueryState(state) => write!(f, "QueryState({state:?})"),
            #[cfg(feature = "serde")]
            RattanOpResult::AddControlInterface(_) => write!(f, "AddControlInterface"),
            #[cfg(feature = "serde")]
            RattanOpResult::ConfigCell => write!(f, "ConfigCell"),
        }
    }
}

#[derive(Clone)]
pub struct RattanOpEndpoint {
    op_tx: mpsc::UnboundedSender<(RattanOp, oneshot::Sender<Result<RattanOpResult, Error>>)>,
}

impl RattanOpEndpoint {
    pub fn new(
        op_tx: mpsc::UnboundedSender<(RattanOp, oneshot::Sender<Result<RattanOpResult, Error>>)>,
    ) -> Self {
        Self { op_tx }
    }

    pub async fn exec(&self, op: RattanOp) -> Result<RattanOpResult, Error> {
        let (res_tx, res_rx) = oneshot::channel();
        self.op_tx
            .send((op, res_tx))
            .map_err(|tokio::sync::mpsc::error::SendError((op, _))| {
                RattanOpError::SendOpError(op)
            })?;
        res_rx.await.map_err(|_| RattanOpError::RecvOpResError)?
    }
}

pub(crate) struct RattanController {
    op_rx: mpsc::UnboundedReceiver<(RattanOp, oneshot::Sender<Result<RattanOpResult, Error>>)>,
    notify_tx: broadcast::Sender<RattanNotify>,
    cancel_token: CancellationToken,
    state: Arc<AtomicU8>,
    #[cfg(feature = "serde")]
    control_interfaces: HashMap<String, Arc<dyn JsonControlInterface>>,
}

impl RattanController {
    pub fn new(
        op_rx: mpsc::UnboundedReceiver<(RattanOp, oneshot::Sender<Result<RattanOpResult, Error>>)>,
        notify_tx: broadcast::Sender<RattanNotify>,
        cancel_token: CancellationToken,
        state: Arc<AtomicU8>,
    ) -> Self {
        Self {
            op_rx,
            notify_tx,
            cancel_token,
            state,
            #[cfg(feature = "serde")]
            control_interfaces: HashMap::new(),
        }
    }

    fn handle_op(&mut self, op: RattanOp) -> Result<RattanOpResult, Error> {
        match op {
            RattanOp::SendNotify(notify) => {
                if notify == RattanNotify::Start
                    && self.state.load(Ordering::Relaxed) != RattanState::Spawned as u8
                {
                    Err(RattanCoreError::SendNotifyError(format!(
                        "Rattan state is {:?} instead of Spawned when sending Start notify",
                        RattanState::from(self.state.load(Ordering::Relaxed))
                    ))
                    .into())
                } else {
                    self.notify_tx
                        .send(notify.clone())
                        .map(|count| {
                            if count <= 1 {
                                warn!("No receiver for rattan notify {notify:?}");
                            }
                            self.state
                                .store(RattanState::Running as u8, Ordering::Relaxed);
                            RattanOpResult::SendNotify
                        })
                        .map_err(|tokio::sync::broadcast::error::SendError(notify)| {
                            RattanCoreError::SendNotifyError(format!(
                                "Failed to send notify, {notify:?}"
                            ))
                            .into()
                        })
                }
            }
            RattanOp::QueryState => Ok(RattanOpResult::QueryState(
                self.state.load(Ordering::Relaxed).into(),
            )),
            #[cfg(feature = "serde")]
            RattanOp::AddControlInterface(id, control_interface) => {
                Ok(RattanOpResult::AddControlInterface(
                    self.control_interfaces.insert(id, control_interface),
                ))
            }
            #[cfg(feature = "serde")]
            RattanOp::ConfigCell(id, payload) => match self.control_interfaces.get(&id) {
                Some(control_interface) => control_interface.config_cell(payload),
                None => Err(RattanCoreError::UnknownIdError(id).into()),
            }
            .map(|_| RattanOpResult::ConfigCell),
        }
    }

    #[instrument(name = "RattanController", level = "error", skip_all)]
    pub async fn run(mut self) {
        loop {
            tokio::select! {
                op = self.op_rx.recv() => {
                    match op {
                        Some((op, res_tx)) => {
                            debug!(?op, "Received Op");
                            let res = self.handle_op(op);
                            debug!(?res, "Op result");
                            if let Err(e) = res_tx.send(res) {
                                warn!("Failed to send back result: {:?}", e);
                            }
                        }
                        None => {
                            info!("RattanController exited");
                            break;
                        }
                    }
                }
                _ = self.cancel_token.cancelled() => {
                    info!("RattanController cancelled");
                    break
                }
            }
        }
    }
}
