use std::{fmt::Debug, sync::Arc};

use async_trait::async_trait;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use super::{ControlInterface, Egress, Ingress};
use crate::cells::{AtomicCellState, Cell, CellState, Packet};
use crate::check_cell_state;
use crate::error::Error;

#[derive(Clone)]
pub struct ShadowCellIngress<P>
where
    P: Packet,
{
    ingress: mpsc::UnboundedSender<P>,
}

impl<P> Ingress<P> for ShadowCellIngress<P>
where
    P: Packet + Send,
{
    fn enqueue(&self, packet: P) -> Result<(), Error> {
        self.ingress
            .send(packet)
            .map_err(|_| Error::ChannelError("Data channel is closed.".to_string()))?;
        Ok(())
    }
}

pub struct ShadowCellEgress<P: Packet> {
    egress: mpsc::UnboundedReceiver<P>,
    state: AtomicCellState,
    notify_rx: Option<tokio::sync::broadcast::Receiver<crate::control::RattanNotify>>,
    started: bool,
}

#[async_trait]
impl<P> Egress<P> for ShadowCellEgress<P>
where
    P: Packet + Send + Sync,
{
    async fn dequeue(&mut self) -> Option<P> {
        // Wait for Start notify if not started yet
        crate::wait_until_started!(self, Start);
        check_cell_state!(self.state, self.egress.recv().await?).into()
    }

    fn change_state(&self, state: CellState) {
        self.state
            .store(state, std::sync::atomic::Ordering::Release);
    }

    fn set_notify_receiver(
        &mut self,
        notify_rx: tokio::sync::broadcast::Receiver<crate::control::RattanNotify>,
    ) {
        self.notify_rx = Some(notify_rx);
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Debug, Default, Clone)]
pub struct ShadowCellConfig {}

impl ShadowCellConfig {
    pub fn new() -> Self {
        Self {}
    }
}

pub struct ShadowCellControlInterface {}

impl ControlInterface for ShadowCellControlInterface {
    type Config = ShadowCellConfig;
    fn set_config(&self, _config: Self::Config) -> Result<(), Error> {
        Ok(())
    }
}

pub struct ShadowCell<P: Packet> {
    ingress: Arc<ShadowCellIngress<P>>,
    egress: ShadowCellEgress<P>,
    control_interface: Arc<ShadowCellControlInterface>,
}

impl<P> Cell<P> for ShadowCell<P>
where
    P: Packet + Send + Sync + 'static,
{
    type IngressType = ShadowCellIngress<P>;
    type EgressType = ShadowCellEgress<P>;
    type ControlInterfaceType = ShadowCellControlInterface;

    fn sender(&self) -> Arc<Self::IngressType> {
        self.ingress.clone()
    }

    fn receiver(&mut self) -> &mut Self::EgressType {
        &mut self.egress
    }

    fn into_receiver(self) -> Self::EgressType {
        self.egress
    }

    fn control_interface(&self) -> Arc<Self::ControlInterfaceType> {
        self.control_interface.clone()
    }
}

impl<P> ShadowCell<P>
where
    P: Packet,
{
    pub fn new() -> Result<ShadowCell<P>, Error> {
        let (rx, tx) = mpsc::unbounded_channel();
        Ok(ShadowCell {
            ingress: Arc::new(ShadowCellIngress { ingress: rx }),
            egress: ShadowCellEgress {
                egress: tx,
                state: AtomicCellState::new(CellState::Drop),
                notify_rx: None,
                started: false,
            },
            control_interface: Arc::new(ShadowCellControlInterface {}),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::cells::{StdPacket, TestPacket};
    use rand::{rng, Rng};
    use tracing::{span, Level};

    #[test_log::test]
    fn test_shadow_cell() -> Result<(), Error> {
        let _span = span!(Level::INFO, "test_shadow_cell").entered();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _guard = rt.enter();

        let cell = ShadowCell::new()?;
        let ingress = cell.sender();
        let mut egress = cell.into_receiver();
        egress.reset();
        egress.change_state(CellState::Normal);

        let mut buffer = [0u8; 256];
        for _ in 0..100 {
            rng().fill(&mut buffer);
            let test_packet = TestPacket::<StdPacket>::from_raw_buffer(&buffer);
            ingress.enqueue(test_packet)?;

            let received = rt.block_on(async { egress.dequeue().await });
            let received = received.unwrap();
            assert_eq!(received.as_slice(), buffer);
            assert_eq!(received.delay(), Duration::ZERO)
        }
        Ok(())
    }
}
