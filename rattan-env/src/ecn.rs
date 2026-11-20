//! Explicit Congestion Notification (ECN) helpers.
//!
//! These functions operate in place on raw Ethernet II frames (as returned by
//! [`Packet::as_slice`](super::Packet::as_slice)), so that AQMs can read the ECN
//! codepoint and mark packets with Congestion Experienced (CE) instead of dropping them.
//!
//! # References
//!
//! - RFC 3168 (The Addition of ECN to IP):
//!   <https://www.rfc-editor.org/info/rfc3168>
//! - RFC 9331 (ECN Protocol for L4S):
//!   <https://www.rfc-editor.org/info/rfc9331>
//! - RFC 1624 (Incremental Internet Checksum):
//!   <https://www.rfc-editor.org/info/rfc1624>
//! - Linux kernel `IP_ECN_set_ce()` / `IP6_ECN_set_ce()`:
//!   [`include/net/inet_ecn.h`](https://github.com/torvalds/linux/blob/master/include/net/inet_ecn.h)
//!
//! # Supported frames
//!
//! Only untagged Ethernet II frames carrying IPv4 or IPv6 are recognized. VLAN-tagged
//! frames, other EtherTypes, truncated headers, and frames whose IP version nibble does
//! not match the EtherType are treated as non-IP: [`get_ecn`] returns `None` and
//! [`set_ce`] leaves the frame untouched.

pub use etherparse::IpEcn;

const ETHERNET_HEADER_LEN: usize = 14;
const ETHER_TYPE_OFFSET: usize = 12;
const ETHER_TYPE_IPV4: u16 = 0x0800;
const ETHER_TYPE_IPV6: u16 = 0x86DD;

/// Minimal IPv4 header length: Version/IHL ... Destination Address.
const IPV4_MIN_HEADER_LEN: usize = 20;
/// Offset of the DSCP/ECN byte (formerly TOS) within the IPv4 header.
const IPV4_TOS_OFFSET: usize = 1;
/// Offset of the header checksum within the IPv4 header.
const IPV4_CHECKSUM_OFFSET: usize = 10;
/// The ECN field of IPv6 lives in the low 2 bits of the Traffic Class, which straddles
/// bytes 0 and 1 of the header. Byte 1 holds the low nibble of the Traffic Class in its
/// high nibble, so the ECN bits are bits 5..4 of byte 1.
const IPV6_ECN_BYTE_OFFSET: usize = 1;
const IPV6_ECN_SHIFT: u8 = 4;
/// IPv6 has no header checksum, but the fixed header must be complete.
const IPV6_HEADER_LEN: usize = 40;

const ECN_MASK: u8 = IpEcn::MAX_U8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IpVersion {
    V4,
    V6,
}

/// Returns the IP version of the frame if it is an untagged Ethernet II frame carrying a
/// complete IPv4 or IPv6 fixed header whose version nibble matches the EtherType.
fn ip_version(frame: &[u8]) -> Option<IpVersion> {
    let ether_type = u16::from_be_bytes([
        *frame.get(ETHER_TYPE_OFFSET)?,
        *frame.get(ETHER_TYPE_OFFSET + 1)?,
    ]);
    let ip_header = frame.get(ETHERNET_HEADER_LEN..)?;
    let version = *ip_header.first()? >> 4;
    match ether_type {
        ETHER_TYPE_IPV4 if version == 4 && ip_header.len() >= IPV4_MIN_HEADER_LEN => {
            Some(IpVersion::V4)
        }
        ETHER_TYPE_IPV6 if version == 6 && ip_header.len() >= IPV6_HEADER_LEN => {
            Some(IpVersion::V6)
        }
        _ => None,
    }
}

fn ecn_from_bits(bits: u8) -> IpEcn {
    match bits & ECN_MASK {
        0b00 => IpEcn::NotEct,
        0b01 => IpEcn::Ect1,
        0b10 => IpEcn::Ect0,
        _ => IpEcn::CongestionExperienced,
    }
}

/// Reads the ECN codepoint of an Ethernet II frame.
///
/// Returns `None` if the frame does not carry an IPv4 or IPv6 packet.
pub fn get_ecn(frame: &[u8]) -> Option<IpEcn> {
    let bits = match ip_version(frame)? {
        IpVersion::V4 => frame[ETHERNET_HEADER_LEN + IPV4_TOS_OFFSET],
        IpVersion::V6 => frame[ETHERNET_HEADER_LEN + IPV6_ECN_BYTE_OFFSET] >> IPV6_ECN_SHIFT,
    };
    Some(ecn_from_bits(bits))
}

/// Marks an Ethernet II frame with ECN Congestion Experienced (CE), in place.
///
/// Mirrors the semantics of the Linux kernel's `INET_ECN_set_ce()`:
///
/// | Codepoint before | Frame after | Return value |
/// |------------------|-------------|--------------|
/// | Not-ECT          | unchanged   | `false`      |
/// | ECT(0) / ECT(1)  | CE          | `true`       |
/// | CE               | unchanged   | `true`       |
/// | not IPv4/IPv6    | unchanged   | `false`      |
///
/// A `false` return means the transport is not ECN-capable, so an AQM that wanted to
/// signal congestion should drop the packet instead.
///
/// For IPv4, the header checksum is updated incrementally (RFC 1624) rather than
/// recomputed, so a frame that arrived with a wrong checksum keeps it wrong.
pub fn set_ce(frame: &mut [u8]) -> bool {
    let Some(version) = ip_version(frame) else {
        return false;
    };
    let ecn_byte_index = ETHERNET_HEADER_LEN
        + match version {
            IpVersion::V4 => IPV4_TOS_OFFSET,
            IpVersion::V6 => IPV6_ECN_BYTE_OFFSET,
        };
    let shift = match version {
        IpVersion::V4 => 0,
        IpVersion::V6 => IPV6_ECN_SHIFT,
    };

    let old_byte = frame[ecn_byte_index];
    match ecn_from_bits(old_byte >> shift) {
        IpEcn::NotEct => return false,
        IpEcn::CongestionExperienced => return true,
        IpEcn::Ect0 | IpEcn::Ect1 => {}
    }
    let new_byte = old_byte | (ECN_MASK << shift);
    frame[ecn_byte_index] = new_byte;

    if version == IpVersion::V4 {
        let checksum_index = ETHERNET_HEADER_LEN + IPV4_CHECKSUM_OFFSET;
        let old_checksum = u16::from_be_bytes([frame[checksum_index], frame[checksum_index + 1]]);
        // The TOS byte is the low byte of the 16-bit word made of Version/IHL and TOS.
        let new_checksum =
            incremental_checksum_update(old_checksum, u16::from(old_byte), u16::from(new_byte));
        frame[checksum_index..checksum_index + 2].copy_from_slice(&new_checksum.to_be_bytes());
    }
    true
}

/// Updates an Internet checksum after one 16-bit word of the covered data changed from
/// `old_word` to `new_word`, using equation 3 of RFC 1624: `HC' = ~(~HC + ~m + m')`.
fn incremental_checksum_update(checksum: u16, old_word: u16, new_word: u16) -> u16 {
    let sum = u32::from(!checksum) + u32::from(!old_word) + u32::from(new_word);
    // Fold the carries back in (one's complement addition).
    let sum = (sum & 0xFFFF) + (sum >> 16);
    let sum = (sum & 0xFFFF) + (sum >> 16);
    !(sum as u16)
}

#[cfg(test)]
mod tests {
    use etherparse::{IpDscp, IpNumber, Ipv4Header, Ipv6FlowLabel, Ipv6Header};

    use super::*;
    use crate::{common::Packet, env::standard::StdPacket};

    const ALL_ECN: [IpEcn; 4] = [
        IpEcn::NotEct,
        IpEcn::Ect1,
        IpEcn::Ect0,
        IpEcn::CongestionExperienced,
    ];

    fn ethernet_header(ether_type: u16) -> Vec<u8> {
        let mut frame = vec![0x02, 0, 0, 0, 0, 0x01, 0x02, 0, 0, 0, 0, 0x02];
        frame.extend_from_slice(&ether_type.to_be_bytes());
        frame
    }

    fn ipv4_header(ecn: IpEcn, dscp: u8, identification: u16) -> Ipv4Header {
        let mut header =
            Ipv4Header::new(100, 64, IpNumber::TCP, [10, 1, 1, 1], [10, 2, 1, 1]).unwrap();
        header.dscp = IpDscp::try_new(dscp).unwrap();
        header.ecn = ecn;
        header.identification = identification;
        header.header_checksum = header.calc_header_checksum();
        header
    }

    fn ipv4_frame(header: &Ipv4Header) -> Vec<u8> {
        let mut frame = ethernet_header(ETHER_TYPE_IPV4);
        frame.extend_from_slice(&header.to_bytes());
        frame.extend_from_slice(&[0xAB; 100]);
        frame
    }

    fn ipv6_frame(ecn: IpEcn, dscp: u8, flow_label: u32) -> Vec<u8> {
        let header = Ipv6Header {
            traffic_class: (dscp << 2) | ecn.value(),
            flow_label: Ipv6FlowLabel::try_new(flow_label).unwrap(),
            payload_length: 100,
            next_header: IpNumber::TCP,
            hop_limit: 64,
            source: [0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
            destination: [0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2],
        };
        let mut frame = ethernet_header(ETHER_TYPE_IPV6);
        frame.extend_from_slice(&header.to_bytes());
        frame.extend_from_slice(&[0xAB; 100]);
        frame
    }

    fn parse_ipv4(frame: &[u8]) -> Ipv4Header {
        Ipv4Header::from_slice(&frame[ETHERNET_HEADER_LEN..])
            .unwrap()
            .0
    }

    fn parse_ipv6(frame: &[u8]) -> Ipv6Header {
        Ipv6Header::from_slice(&frame[ETHERNET_HEADER_LEN..])
            .unwrap()
            .0
    }

    #[test]
    fn test_get_ecn_ipv4() {
        for ecn in ALL_ECN {
            // A DSCP with all bits set must not leak into the ECN field.
            for dscp in [0, 0b10_1110, IpDscp::MAX_U8] {
                let frame = ipv4_frame(&ipv4_header(ecn, dscp, 0));
                assert_eq!(get_ecn(&frame), Some(ecn), "dscp {dscp:#b}");
            }
        }
    }

    #[test]
    fn test_get_ecn_ipv6() {
        for ecn in ALL_ECN {
            for dscp in [0, 0b10_1110, IpDscp::MAX_U8] {
                let frame = ipv6_frame(ecn, dscp, 0xF_FFFF);
                assert_eq!(get_ecn(&frame), Some(ecn), "dscp {dscp:#b}");
            }
        }
    }

    #[test]
    fn test_set_ce_ipv4_codepoints() {
        for ecn in ALL_ECN {
            let original = ipv4_frame(&ipv4_header(ecn, 0b10_1110, 0x1234));
            let mut frame = original.clone();
            let marked = set_ce(&mut frame);
            match ecn {
                IpEcn::NotEct => {
                    assert!(!marked);
                    assert_eq!(frame, original, "Not-ECT frame must not be modified");
                }
                IpEcn::CongestionExperienced => {
                    assert!(marked);
                    assert_eq!(frame, original, "CE frame must not be modified");
                }
                IpEcn::Ect0 | IpEcn::Ect1 => {
                    assert!(marked);
                    let header = parse_ipv4(&frame);
                    assert_eq!(header.ecn, IpEcn::CongestionExperienced);
                    assert_eq!(header.dscp.value(), 0b10_1110);
                    assert_eq!(header.header_checksum, header.calc_header_checksum());
                    // Only the TOS byte and the checksum may change.
                    for (i, (a, b)) in original.iter().zip(&frame).enumerate() {
                        let offset = i.wrapping_sub(ETHERNET_HEADER_LEN);
                        if offset != IPV4_TOS_OFFSET
                            && offset != IPV4_CHECKSUM_OFFSET
                            && offset != IPV4_CHECKSUM_OFFSET + 1
                        {
                            assert_eq!(a, b, "byte {i} changed");
                        }
                    }
                }
            }
        }
    }

    /// Every 16-bit value of the identification field yields a different original
    /// checksum, so together they exercise every carry case of the incremental update.
    #[test]
    fn test_set_ce_ipv4_checksum_exhaustive() {
        for ecn in [IpEcn::Ect0, IpEcn::Ect1] {
            for dscp in [0, IpDscp::MAX_U8] {
                for identification in 0..=u16::MAX {
                    let mut frame = ipv4_frame(&ipv4_header(ecn, dscp, identification));
                    assert!(set_ce(&mut frame));
                    let header = parse_ipv4(&frame);
                    assert_eq!(
                        header.header_checksum,
                        header.calc_header_checksum(),
                        "ecn {ecn:?}, dscp {dscp}, identification {identification}"
                    );
                }
            }
        }
    }

    #[test]
    fn test_set_ce_ipv4_with_options() {
        let mut header = ipv4_header(IpEcn::Ect1, 0, 7);
        header.options = [1, 1, 1, 0].as_slice().try_into().unwrap(); // NOP, NOP, NOP, EOL
        header.header_checksum = header.calc_header_checksum();
        let mut frame = ipv4_frame(&header);
        assert_eq!(get_ecn(&frame), Some(IpEcn::Ect1));
        assert!(set_ce(&mut frame));
        let header = parse_ipv4(&frame);
        assert_eq!(header.ecn, IpEcn::CongestionExperienced);
        assert_eq!(header.header_checksum, header.calc_header_checksum());
    }

    #[test]
    fn test_set_ce_ipv6() {
        for ecn in ALL_ECN {
            let original = ipv6_frame(ecn, 0b10_1110, 0xA_BCDE);
            let mut frame = original.clone();
            let marked = set_ce(&mut frame);
            assert_eq!(marked, ecn != IpEcn::NotEct);
            let header = parse_ipv6(&frame);
            let expected_ecn = if ecn == IpEcn::NotEct {
                IpEcn::NotEct
            } else {
                IpEcn::CongestionExperienced
            };
            assert_eq!(header.traffic_class & ECN_MASK, expected_ecn.value());
            assert_eq!(header.traffic_class >> 2, 0b10_1110, "DSCP changed");
            assert_eq!(header.flow_label.value(), 0xA_BCDE, "flow label changed");
            assert_eq!(
                &frame[ETHERNET_HEADER_LEN + 2..],
                &original[ETHERNET_HEADER_LEN + 2..]
            );
            assert_eq!(
                &frame[..ETHERNET_HEADER_LEN],
                &original[..ETHERNET_HEADER_LEN]
            );
        }
    }

    #[test]
    fn test_non_ip_frames_are_ignored() {
        let ipv4 = ipv4_frame(&ipv4_header(IpEcn::Ect0, 0, 0));
        let ipv6 = ipv6_frame(IpEcn::Ect0, 0, 0);

        let mut arp = ethernet_header(0x0806);
        arp.extend_from_slice(&[0x45; 28]);

        // An 802.1Q tag in front of IPv4 is not supported and must not be misparsed.
        let mut vlan = ethernet_header(0x8100);
        vlan.extend_from_slice(&[0x00, 0x01, 0x08, 0x00]);
        vlan.extend_from_slice(&ipv4[ETHERNET_HEADER_LEN..]);

        // EtherType says IPv4 but the version nibble says IPv6, and vice versa.
        let mut v4_type_v6_payload = ipv6.clone();
        v4_type_v6_payload[ETHER_TYPE_OFFSET..ETHERNET_HEADER_LEN]
            .copy_from_slice(&ETHER_TYPE_IPV4.to_be_bytes());
        let mut v6_type_v4_payload = ipv4.clone();
        v6_type_v4_payload[ETHER_TYPE_OFFSET..ETHERNET_HEADER_LEN]
            .copy_from_slice(&ETHER_TYPE_IPV6.to_be_bytes());

        let cases = [
            ("empty", Vec::new()),
            ("truncated ethernet", ipv4[..ETHER_TYPE_OFFSET + 1].to_vec()),
            ("ethernet only", ipv4[..ETHERNET_HEADER_LEN].to_vec()),
            (
                "truncated ipv4",
                ipv4[..ETHERNET_HEADER_LEN + IPV4_MIN_HEADER_LEN - 1].to_vec(),
            ),
            (
                "truncated ipv6",
                ipv6[..ETHERNET_HEADER_LEN + IPV6_HEADER_LEN - 1].to_vec(),
            ),
            ("arp", arp),
            ("vlan", vlan),
            ("ipv4 ethertype, ipv6 payload", v4_type_v6_payload),
            ("ipv6 ethertype, ipv4 payload", v6_type_v4_payload),
        ];
        for (name, original) in cases {
            assert_eq!(get_ecn(&original), None, "{name}");
            let mut frame = original.clone();
            assert!(!set_ce(&mut frame), "{name}");
            assert_eq!(frame, original, "{name} must not be modified");
        }
    }

    #[test]
    fn test_incremental_checksum_update() {
        // Example from RFC 1624, section 4: HC = 0xDD2F, m = 0x5555, m' = 0x3285.
        assert_eq!(incremental_checksum_update(0xDD2F, 0x5555, 0x3285), 0x0000);
        // Changing nothing leaves the checksum unchanged.
        assert_eq!(incremental_checksum_update(0x1234, 0xABCD, 0xABCD), 0x1234);
    }

    #[test]
    fn test_packet_trait_methods() {
        let mut packet = StdPacket::from_raw_buffer(&ipv4_frame(&ipv4_header(IpEcn::Ect1, 0, 1)));
        assert_eq!(packet.ecn(), Some(IpEcn::Ect1));
        assert!(packet.set_ce());
        assert_eq!(packet.ecn(), Some(IpEcn::CongestionExperienced));
        let header = packet.ip_hdr().unwrap();
        assert_eq!(header.header_checksum, header.calc_header_checksum());

        let mut packet = StdPacket::from_raw_buffer(&ipv4_frame(&ipv4_header(IpEcn::NotEct, 0, 1)));
        assert!(!packet.set_ce());
        assert_eq!(packet.ecn(), Some(IpEcn::NotEct));
    }
}
