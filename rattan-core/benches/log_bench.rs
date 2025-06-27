#[cfg(test)]
mod tests {
    use super::*;
    use test::Bencher;

    #[bench]
    fn bench_log_entry_header_new(b: &mut Bencher) {
        b.iter(|| LogEntryHeader::new());
    }

    #[bench]
    fn bench_log_entry_header_set_get_type(b: &mut Bencher) {
        let mut header = LogEntryHeader::new();
        b.iter(|| {
            header.set_type(5);
            test::black_box(header.get_type());
        });
    }

    #[bench]
    fn bench_log_entry_header_set_get_length(b: &mut Bencher) {
        let mut header = LogEntryHeader::new();
        b.iter(|| {
            header.set_length(1024);
            test::black_box(header.get_length());
        });
    }

    #[bench]
    fn bench_log_entry_header_as_bytes(b: &mut Bencher) {
        let mut header = LogEntryHeader::new();
        header.set_type(3);
        header.set_length(512);
        b.iter(|| test::black_box(header.as_bytes()));
    }

    #[bench]
    fn bench_general_pkt_header_new(b: &mut Bencher) {
        b.iter(|| GeneralPktHeader::new());
    }

    #[bench]
    fn bench_general_pkt_header_set_get_all(b: &mut Bencher) {
        let mut header = GeneralPktHeader::new();
        b.iter(|| {
            header.set_type(2);
            header.set_pkt_action(1);
            header.set_length(128);
            test::black_box(header.get_type());
            test::black_box(header.get_pkt_action());
            test::black_box(header.get_length());
        });
    }

    #[bench]
    fn bench_protocol_header_new(b: &mut Bencher) {
        b.iter(|| ProtocolHeader::new());
    }

    #[bench]
    fn bench_protocol_header_set_get_all(b: &mut Bencher) {
        let mut header = ProtocolHeader::new();
        b.iter(|| {
            header.set_type(4);
            header.set_length(256);
            test::black_box(header.get_type());
            test::black_box(header.get_length());
        });
    }

    #[bench]
    fn bench_tcp_log_entry_new(b: &mut Bencher) {
        b.iter(|| TCPLogEntry::new());
    }

    #[bench]
    fn bench_tcp_log_entry_as_bytes(b: &mut Bencher) {
        let entry = TCPLogEntry::new();
        b.iter(|| test::black_box(entry.as_bytes()));
    }

    #[bench]
    fn bench_tcp_log_entry_from_bytes(b: &mut Bencher) {
        let entry = TCPLogEntry::new();
        let bytes = entry.as_bytes();
        b.iter(|| test::black_box(TCPLogEntry::from_bytes(&bytes)));
    }

    #[bench]
    fn bench_general_pkt_entry_as_bytes(b: &mut Bencher) {
        let entry = GeneralPktEntry {
            header: GeneralPktHeader::new(),
            ts: 123456789,
            pkt_length: 1500,
        };
        b.iter(|| test::black_box(entry.as_bytes()));
    }

    #[bench]
    fn bench_tcp_protocol_entry_as_bytes(b: &mut Bencher) {
        let entry = TCPProtocolEntry {
            header: ProtocolHeader::new(),
            flow_id: 42,
            seq: 1000,
            ack: 2000,
            ip_id: 123,
            ip_frag_offset: 0,
            checksum: 0xABCD,
            flags: 0b00010101,
            padding: 0,
        };
        b.iter(|| test::black_box(entry.as_bytes()));
    }

    #[bench]
    fn bench_full_log_entry_creation_and_serialization(b: &mut Bencher) {
        b.iter(|| {
            let mut entry = TCPLogEntry::new();
            entry.header.set_type(1);
            entry.general_pkt_entry.header.set_type(2);
            entry.general_pkt_entry.header.set_pkt_action(0); // Send
            entry.general_pkt_entry.ts = 123456789;
            entry.general_pkt_entry.pkt_length = 1500;
            entry.tcp_entry.header.set_type(3);
            entry.tcp_entry.flow_id = 42;
            entry.tcp_entry.seq = 1000;
            entry.tcp_entry.ack = 2000;
            test::black_box(entry.as_bytes());
        });
    }
}