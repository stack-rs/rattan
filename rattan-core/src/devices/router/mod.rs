use crate::error::{Error, RoutingTableError};
use async_trait::async_trait;
use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use std::{
    net::{IpAddr, Ipv4Addr},
    sync::{Arc, RwLock},
};
use tracing::warn;

use super::{ControlInterface, Device, Egress, Ingress, Packet};

pub mod routing;
pub use routing::*;

#[derive(Clone)]
pub struct RouterDeviceIngress<P, R>
where
    P: Packet,
    R: RoutingTable,
{
    egresses: Vec<Arc<dyn Ingress<P>>>,
    router: Arc<RwLock<R>>,
}

impl<P, R> Ingress<P> for RouterDeviceIngress<P, R>
where
    P: Packet + Send,
    R: RoutingTable,
{
    fn enqueue(&self, packet: P) -> Result<(), Error> {
        // resolve IPv4 destination address, drop packet if it fails
        let dest = match packet.ip_hdr() {
            Some(head) => Ipv4Addr::from(head.destination),
            _ => {
                warn!("Unable to resolve IPv4 destination address, packet dropped");
                return Ok(());
            }
        };

        match self.router.read().unwrap().match_ip(IpAddr::V4(dest)) {
            Some(interface_id) => {
                match self.egresses.get(interface_id) {
                    // normal forwarding
                    Some(egress) => {
                        egress.enqueue(packet)?;
                        Ok(())
                    }
                    // invalid interface (unreachable if interface_id is checked before adding to the routing table)
                    None => Err(RoutingTableError::InvalidInterfaceId(interface_id).into()),
                }
            }
            // interface_id is None, just drop
            None => Ok(()),
        }
    }
}

pub struct RouterDeviceEgress {}

#[async_trait]
impl<P> Egress<P> for RouterDeviceEgress
where
    P: Packet + Send + Sync,
{
    async fn dequeue(&mut self) -> Option<P> {
        // egress of router should not be used
        futures::future::pending().await
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Debug, Default, Clone)]
pub struct RouterDeviceConfig {
    pub egress_connections: Vec<String>,
    pub routing_table: PlainRoutingTable,
}

pub struct RouterDeviceControlInterface<R: RoutingTable> {
    interface_count: usize,
    router: Arc<RwLock<R>>,
}

impl<R> RouterDeviceControlInterface<R>
where
    R: RoutingTable,
{
    // check interface id each time an entry is added
    fn check_interface_id(&self, interface_id: Option<usize>) -> Result<(), Error> {
        if let Some(i) = interface_id {
            if i >= self.interface_count {
                return Err(RoutingTableError::InvalidInterfaceId(i).into());
            }
        }
        Ok(())
    }

    /// Remove all routing entries
    pub fn clear(&self) {
        self.router.write().unwrap().clear();
    }

    /// Add one routing entry, checking interface_id
    pub fn add(&self, entry: RoutingEntry) -> Result<(), Error> {
        self.check_interface_id(entry.interface_id)?;
        self.router.write().unwrap().add(entry)?;
        Ok(())
    }

    /// Remove one routing entry by ip prefix
    pub fn remove(&self, prefix: IpNet) -> Result<(), Error> {
        self.router.write().unwrap().remove(prefix)?;
        Ok(())
    }

    /// Get current routing table
    pub fn get_plain_table(&self) -> PlainRoutingTable {
        self.router.read().unwrap().get_plain_table()
    }
}

impl<R> ControlInterface for RouterDeviceControlInterface<R>
where
    R: RoutingTable,
{
    type Config = PlainRoutingTable;
    /// Replace the whole routing table, checking interface_id
    ///
    /// If any new routing entry is illegal, the original routing table remains unchanged
    fn set_config(&self, config: Self::Config) -> Result<(), Error> {
        for entry in &config {
            self.check_interface_id(entry.interface_id)?;
        }
        self.router.write().unwrap().reset(config)?;
        Ok(())
    }
}
pub struct RouterDevice<P: Packet, R: RoutingTable> {
    ingress: Arc<RouterDeviceIngress<P, R>>,
    egress: RouterDeviceEgress,
    control_interface: Arc<RouterDeviceControlInterface<R>>,
}

impl<P, R> RouterDevice<P, R>
where
    P: Packet,
    R: RoutingTable,
{
    pub fn new(
        egresses: Vec<Arc<dyn Ingress<P>>>,
        table: PlainRoutingTable,
    ) -> Result<RouterDevice<P, R>, Error> {
        let interface_count = egresses.len();
        let router = Arc::new(RwLock::new(R::try_from(vec![])?));
        let ingress = RouterDeviceIngress {
            egresses,
            router: router.clone(),
        };
        let control_interface = Arc::new(RouterDeviceControlInterface {
            interface_count,
            router,
        });

        // use set_config now because it checks interface_id, while R::try_from does not
        control_interface.set_config(table)?;

        Ok(RouterDevice {
            ingress: Arc::new(ingress),
            egress: RouterDeviceEgress {},
            control_interface,
        })
    }
}

impl<P, R> Device<P> for RouterDevice<P, R>
where
    P: Packet + Send + Sync + 'static,
    R: RoutingTable,
{
    type IngressType = RouterDeviceIngress<P, R>;
    type EgressType = RouterDeviceEgress;
    type ControlInterfaceType = RouterDeviceControlInterface<R>;

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

#[cfg(test)]
mod tests {
    use crate::devices::{shadow::ShadowDevice, StdPacket};
    use etherparse::{PacketBuilder, SlicedPacket, TransportSlice};
    use ipnet::IpNet;
    use rand::{thread_rng, Rng};
    use tracing::{span, Level};

    use super::*;

    // generate a UDP packet to `dest`, with `payload`
    fn generate_packet(dest: Ipv4Addr, payload: &[u8]) -> StdPacket {
        let builder = PacketBuilder::ethernet2(
            [0x38, 0x7e, 0x58, 0xe7, 1, 1],
            [0x38, 0x7e, 0x58, 0xe7, 1, 2],
        )
        .ipv4([1, 2, 3, 4], dest.octets(), 127)
        .udp(12345, 54321);
        let mut buffer = Vec::<u8>::with_capacity(builder.size(payload.len()));
        builder.write(&mut buffer, payload).unwrap();

        StdPacket::from_raw_buffer(buffer.as_slice())
    }

    // check the payload of a UDP packet
    fn test_packet(received: Option<StdPacket>, payload: &[u8]) -> bool {
        match SlicedPacket::from_ethernet(received.unwrap().as_slice())
            .unwrap()
            .transport
            .unwrap()
        {
            TransportSlice::Udp(udp) => udp.payload() == payload,
            _ => false,
        }
    }

    #[test_log::test]
    fn test_router_device() -> Result<(), Error> {
        let _span = span!(Level::INFO, "test_router_device").entered();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _guard = rt.enter();

        let ips: [IpNet; 2] = [
            "192.168.1.1/32".parse().unwrap(),
            "192.168.1.2/32".parse().unwrap(),
        ];
        let target_ip: [Ipv4Addr; 2] = [
            "192.168.1.1".parse().unwrap(),
            "192.168.1.2".parse().unwrap(),
        ];

        let shadow0 = ShadowDevice::new()?;
        let shadow1 = ShadowDevice::new()?;

        let ingresses: Vec<Arc<dyn Ingress<_>>> =
            vec![shadow0.sender().clone(), shadow1.sender().clone()];

        assert!(RouterDevice::<_, SimpleRoutingTable>::new(
            ingresses.clone(),
            vec![RoutingEntry::new(ips[0], Some(2))]
        )
        .is_err());

        let mut egresses = [shadow0.into_receiver(), shadow1.into_receiver()];
        egresses[0].reset();
        egresses[0].change_state(2);
        egresses[1].reset();
        egresses[1].change_state(2);

        let device: RouterDevice<StdPacket, SimpleRoutingTable> = RouterDevice::new(
            ingresses,
            vec![
                RoutingEntry::new(ips[0], Some(0)),
                RoutingEntry::new(ips[1], Some(1)),
            ],
        )?;
        let ingress = device.sender();

        let mut payload = [0u8; 256];
        for _ in 0..100 {
            let target = thread_rng().gen_range(0..=1);

            thread_rng().fill(&mut payload);
            let packet = generate_packet(target_ip[target], &payload);
            ingress.enqueue(packet)?;
            let received = rt.block_on(async { egresses[target].dequeue().await });
            assert!(test_packet(received, &payload));
        }
        Ok(())
    }

    #[test_log::test]
    fn test_router_control() -> Result<(), Error> {
        let _span = span!(Level::INFO, "test_router_control").entered();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _guard = rt.enter();

        let ips: [IpNet; 2] = [
            "192.168.1.1/32".parse().unwrap(),
            "192.168.1.2/32".parse().unwrap(),
        ];

        let shadow0 = ShadowDevice::new()?;
        let shadow1 = ShadowDevice::new()?;

        let ingresses: Vec<Arc<dyn Ingress<_>>> =
            vec![shadow0.sender().clone(), shadow1.sender().clone()];
        let mut egresses = [shadow0.into_receiver(), shadow1.into_receiver()];
        egresses[0].reset();
        egresses[0].change_state(2);
        egresses[1].reset();
        egresses[1].change_state(2);

        let device: RouterDevice<StdPacket, SimpleRoutingTable> =
            RouterDevice::new(ingresses, vec![RoutingEntry::new(ips[0], Some(0))])?;
        let ingress = device.sender();
        let control = device.control_interface();
        let mut payload = [0u8; 256];

        thread_rng().fill(&mut payload);
        let packet = generate_packet("192.168.1.1".parse().unwrap(), &payload);
        ingress.enqueue(packet)?;
        let received = rt.block_on(async { egresses[0].dequeue().await });
        assert!(test_packet(received, &payload));

        thread_rng().fill(&mut payload);
        let packet = generate_packet("192.168.1.2".parse().unwrap(), &payload);
        ingress.enqueue(packet)?; // should drop

        // test add
        assert!(control.add(RoutingEntry::new(ips[1], Some(2))).is_err());
        control.add(RoutingEntry::new(ips[1], Some(1))).unwrap();
        assert!(control.add(RoutingEntry::new(ips[1], Some(0))).is_err());

        thread_rng().fill(&mut payload);
        let packet = generate_packet("192.168.1.2".parse().unwrap(), &payload);
        ingress.enqueue(packet)?;
        let received = rt.block_on(async { egresses[1].dequeue().await });
        assert!(test_packet(received, &payload));

        // test remove
        control.remove(ips[0]).unwrap();
        assert!(control.remove(ips[0]).is_err());

        thread_rng().fill(&mut payload);
        let packet = generate_packet("192.168.1.1".parse().unwrap(), &payload);
        ingress.enqueue(packet)?; // should drop

        // test clear
        control.clear();

        thread_rng().fill(&mut payload);
        let packet = generate_packet("192.168.1.2".parse().unwrap(), &payload);
        ingress.enqueue(packet)?; // should drop

        // test set_config
        assert!(control
            .set_config(vec![
                RoutingEntry::new(ips[0], Some(0)),
                RoutingEntry::new(ips[1], Some(2)),
            ])
            .is_err());
        control
            .set_config(vec![
                RoutingEntry::new(ips[0], Some(1)),
                RoutingEntry::new(ips[1], Some(0)),
            ])
            .unwrap();

        thread_rng().fill(&mut payload);
        let packet = generate_packet("192.168.1.1".parse().unwrap(), &payload);
        ingress.enqueue(packet)?;
        let received = rt.block_on(async { egresses[1].dequeue().await });
        assert!(test_packet(received, &payload));

        thread_rng().fill(&mut payload);
        let packet = generate_packet("192.168.1.2".parse().unwrap(), &payload);
        ingress.enqueue(packet)?;
        let received = rt.block_on(async { egresses[0].dequeue().await });
        assert!(test_packet(received, &payload));

        Ok(())
    }
}
