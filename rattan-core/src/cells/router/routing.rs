use crate::error::RoutingTableError;
use ipnet::IpNet;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use std::fmt::{Debug, Formatter};
use std::net::IpAddr;

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RoutingEntry {
    pub prefix: IpNet,
    pub interface_id: Option<usize>,
}

impl Debug for RoutingEntry {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self.interface_id {
            Some(i) => write!(f, "{:?} ->{}", self.prefix, i),
            None => write!(f, "{:?} drop", self.prefix),
        }
    }
}

impl RoutingEntry {
    pub fn new(prefix: IpNet, interface_id: Option<usize>) -> RoutingEntry {
        RoutingEntry {
            prefix,
            interface_id,
        }
    }
}

pub type PlainRoutingTable = Vec<RoutingEntry>;

pub trait RoutingTable:
    Sync + Send + 'static + Sized + TryFrom<PlainRoutingTable, Error = RoutingTableError>
{
    /// Remove all routing entries
    fn clear(&mut self) -> &mut Self;
    /// Add one routing entry
    fn add(&mut self, entry: RoutingEntry) -> Result<&mut Self, RoutingTableError>;
    /// Remove one routing entry by ip prefix
    fn remove(&mut self, prefix: IpNet) -> Result<&mut Self, RoutingTableError>;
    /// Replace the whole routing table
    ///
    /// If any new routing entry is illegal, the original routing table remains unchanged
    fn reset(&mut self, table: PlainRoutingTable) -> Result<&mut Self, RoutingTableError> {
        // create a new object, keep the original object for rollback
        *self = Self::try_from(table)?;
        Ok(self)
    }

    /// Match ip, return Some(interface_id) or None. No exceptions allowed.
    fn match_ip(&self, ip: IpAddr) -> Option<usize>;
    /// Get current routing table, maybe for debugging
    fn get_plain_table(&self) -> PlainRoutingTable;
}

// A simple router without any optimization
pub struct SimpleRoutingTable {
    table: PlainRoutingTable,
}

impl TryFrom<PlainRoutingTable> for SimpleRoutingTable {
    type Error = RoutingTableError;
    fn try_from(table: PlainRoutingTable) -> Result<Self, Self::Error> {
        let mut r = SimpleRoutingTable { table: vec![] };
        for entry in table {
            r.add(entry)?;
        }
        Ok(r)
    }
}

impl RoutingTable for SimpleRoutingTable {
    fn clear(&mut self) -> &mut Self {
        self.table.clear();
        self
    }

    fn add(&mut self, mut entry: RoutingEntry) -> Result<&mut Self, RoutingTableError> {
        // ensure that prefix is unique
        entry.prefix = entry.prefix.trunc();
        if self.table.iter().any(|e| e.prefix == entry.prefix) {
            Err(RoutingTableError::DuplicateEntry(format!("{entry:?}")))
        } else {
            self.table.push(entry);
            Ok(self)
        }
    }

    fn remove(&mut self, mut prefix: IpNet) -> Result<&mut Self, RoutingTableError> {
        prefix = prefix.trunc();
        match self.table.iter().position(|e| e.prefix == prefix) {
            Some(pos) => {
                self.table.remove(pos);
                Ok(self)
            }
            None => Err(RoutingTableError::EntryNotFound(format!("{prefix:?}"))),
        }
    }

    fn match_ip(&self, ip: IpAddr) -> Option<usize> {
        // match -> max prefix_len -> interface_id
        // no match -> None
        self.table
            .iter()
            .filter(|e| e.prefix.contains(&ip))
            .max_by_key(|e| e.prefix.prefix_len())
            .and_then(|e| e.interface_id)
    }

    fn get_plain_table(&self) -> PlainRoutingTable {
        self.table.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn test_routing_table<R: RoutingTable>() -> Result<(), RoutingTableError> {
        // test try_from
        let mut router = R::try_from(vec![
            RoutingEntry::new("10.0.0.123/8".parse()?, Some(0)),
            RoutingEntry::new("192.168.0.0/16".parse()?, Some(2)),
            RoutingEntry::new("192.168.2.0/23".parse()?, None),
            RoutingEntry::new("192.168.2.2/32".parse()?, Some(6)),
            RoutingEntry::new("1234::123:132:0/40".parse()?, Some(3)),
            RoutingEntry::new("1234::123:132:0/0".parse()?, Some(4)),
        ])?;

        // test remove
        router.remove("192.168.2.2/32".parse()?).unwrap();
        assert!(router.remove("192.168.2.2/32".parse()?).is_err()); // routing entry not found

        // test add
        router
            .add(RoutingEntry::new("10.0.0.0/12".parse()?, Some(1)))
            .unwrap();
        assert!(router
            .add(RoutingEntry::new("10.0.0.0/12".parse()?, Some(1)))
            .is_err()); // duplicate routing entry

        // test get_plain_table
        let mut answer = vec![
            RoutingEntry::new("10.0.0.0/8".parse()?, Some(0)),
            RoutingEntry::new("10.0.0.0/12".parse()?, Some(1)),
            RoutingEntry::new("192.168.0.0/16".parse()?, Some(2)),
            RoutingEntry::new("192.168.2.0/23".parse()?, None),
            RoutingEntry::new("1234::/40".parse()?, Some(3)),
            RoutingEntry::new("::/0".parse()?, Some(4)),
        ];
        let mut plain_table = router.get_plain_table();
        answer.sort();
        plain_table.sort();
        assert_eq!(answer, plain_table);

        // test match_ip
        assert_eq!(
            router.match_ip(IpAddr::V4(Ipv4Addr::new(10, 255, 0, 0))),
            Some(0)
        );
        assert_eq!(
            router.match_ip(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 0))),
            Some(1)
        );
        assert_eq!(
            router.match_ip(IpAddr::V4(Ipv4Addr::new(192, 168, 12, 234))),
            Some(2)
        );
        assert_eq!(
            router.match_ip(IpAddr::V4(Ipv4Addr::new(192, 168, 3, 2))),
            None
        );
        assert_eq!(
            router.match_ip(IpAddr::V4(Ipv4Addr::new(222, 222, 222, 222))),
            None
        );
        assert_eq!(
            router.match_ip(IpAddr::V6(Ipv6Addr::new(0x1234, 0, 0, 0, 0, 0, 0, 124))),
            Some(3)
        );
        assert_eq!(
            router.match_ip(IpAddr::V6(Ipv6Addr::new(0, 0, 123, 0, 12, 0, 0, 0))),
            Some(4)
        );

        // test clear
        router.clear();
        assert_eq!(
            router.match_ip(IpAddr::V4(Ipv4Addr::new(10, 255, 0, 0))),
            None
        );

        // test reset
        assert!(router
            .reset(vec![
                RoutingEntry::new("10.0.0.123/8".parse()?, Some(0)),
                RoutingEntry::new("10.0.0.0/8".parse()?, Some(0)),
            ])
            .is_err()); // duplicate routing entry
        assert_eq!(
            router.match_ip(IpAddr::V4(Ipv4Addr::new(10, 255, 0, 0))),
            None
        );
        router.reset(vec![RoutingEntry::new("10.0.0.123/8".parse()?, Some(0))])?;
        assert_eq!(
            router.match_ip(IpAddr::V4(Ipv4Addr::new(10, 255, 0, 0))),
            Some(0)
        );

        Ok(())
    }

    #[test_log::test]
    fn test_simple_router() -> Result<(), RoutingTableError> {
        test_routing_table::<SimpleRoutingTable>()
    }
}
