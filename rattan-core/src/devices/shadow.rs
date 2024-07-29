use super::{ControlInterface, Egress, Ingress};
use crate::devices::{Device, Packet};
use crate::error::Error;
use async_trait::async_trait;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use std::{
    fmt::Debug,
    sync::{atomic::AtomicI32, Arc},
};
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct ShadowDeviceIngress<P>
where
    P: Packet,
{
    ingress: mpsc::UnboundedSender<P>,
}

impl<P> Ingress<P> for ShadowDeviceIngress<P>
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

pub struct ShadowDeviceEgress<P: Packet> {
    egress: mpsc::UnboundedReceiver<P>,
    state: AtomicI32,
}

#[async_trait]
impl<P> Egress<P> for ShadowDeviceEgress<P>
where
    P: Packet + Send + Sync,
{
    async fn dequeue(&mut self) -> Option<P> {
        match self.state.load(std::sync::atomic::Ordering::Acquire) {
            0 => None,
            _ => self.egress.recv().await,
        }
    }

    fn change_state(&self, state: i32) {
        self.state
            .store(state, std::sync::atomic::Ordering::Release);
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Debug, Default, Clone)]
pub struct ShadowDeviceConfig {}

impl ShadowDeviceConfig {
    pub fn new() -> Self {
        Self {}
    }
}

pub struct ShadowDeviceControlInterface {}

impl ControlInterface for ShadowDeviceControlInterface {
    type Config = ShadowDeviceConfig;
    fn set_config(&self, _config: Self::Config) -> Result<(), Error> {
        Ok(())
    }
}

pub struct ShadowDevice<P: Packet> {
    ingress: Arc<ShadowDeviceIngress<P>>,
    egress: ShadowDeviceEgress<P>,
    control_interface: Arc<ShadowDeviceControlInterface>,
}

impl<P> Device<P> for ShadowDevice<P>
where
    P: Packet + Send + Sync + 'static,
{
    type IngressType = ShadowDeviceIngress<P>;
    type EgressType = ShadowDeviceEgress<P>;
    type ControlInterfaceType = ShadowDeviceControlInterface;

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

impl<P> ShadowDevice<P>
where
    P: Packet,
{
    pub fn new() -> Result<ShadowDevice<P>, Error> {
        let (rx, tx) = mpsc::unbounded_channel();
        Ok(ShadowDevice {
            ingress: Arc::new(ShadowDeviceIngress { ingress: rx }),
            egress: ShadowDeviceEgress {
                egress: tx,
                state: AtomicI32::new(0),
            },
            control_interface: Arc::new(ShadowDeviceControlInterface {}),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::StdPacket;
    use rand::{thread_rng, Rng};
    use tracing::{span, Level};

    #[test_log::test]
    fn test_shadow_device() -> Result<(), Error> {
        let _span = span!(Level::INFO, "test_shadow_device").entered();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _guard = rt.enter();

        let device = ShadowDevice::new()?;
        let ingress = device.sender();
        let mut egress = device.into_receiver();
        egress.reset();
        egress.change_state(2);

        let mut buffer = [0u8; 256];
        for _ in 0..100 {
            thread_rng().fill(&mut buffer);
            let test_packet = StdPacket::from_raw_buffer(&buffer);
            ingress.enqueue(test_packet)?;

            let received = rt.block_on(async { egress.dequeue().await });
            assert_eq!(received.unwrap().as_slice(), buffer);
        }
        Ok(())
    }
}
