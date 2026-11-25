use std::fs::OpenOptions;
use std::io::{self, BufWriter, Write};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use etherparse::{NetSlice, SlicedPacket, TransportSlice};
use tokio::time::Instant;

use crate::cells::Packet;
use crate::radix::BASE_TS;

const BUFFER_RECORDS: usize = 8192;
const HEADER: &str = "t_ns,sojourn_ns,l3_len,ecn,proto,src_ip,dst_ip,src_port,dst_port";

struct IpFields {
    ecn: u8,
    protocol: u8,
    source: IpAddr,
    destination: IpAddr,
    ports: Option<(u16, u16)>,
}

impl IpFields {
    fn parse(frame: &[u8]) -> Option<Self> {
        let packet = SlicedPacket::from_ethernet(frame).ok()?;
        let (ecn, source, destination) = match packet.net.as_ref()? {
            NetSlice::Ipv4(ip) => {
                let h = ip.header();
                (h.ecn(), h.source_addr().into(), h.destination_addr().into())
            }
            NetSlice::Ipv6(ip) => {
                let h = ip.header();
                (h.ecn(), h.source_addr().into(), h.destination_addr().into())
            }
            _ => return None,
        };
        let ports = match packet.transport.as_ref() {
            Some(TransportSlice::Tcp(tcp)) => Some((tcp.source_port(), tcp.destination_port())),
            Some(TransportSlice::Udp(udp)) => Some((udp.source_port(), udp.destination_port())),
            _ => None,
        };
        Some(Self {
            ecn: ecn.value(),
            protocol: packet.ip_payload()?.ip_number.0,
            source,
            destination,
            ports,
        })
    }
}

struct DequeueRecord {
    t_ns: i128,
    sojourn_ns: u128,
    l3_len: usize,
    ip: Option<IpFields>,
}

/// Optional nonblocking AQM event sink. Clones share one bounded writer.
#[derive(Clone, Debug)]
pub struct QueueEventRecorder(Arc<EventLog>);

#[derive(Debug)]
struct EventLog {
    sender: Option<SyncSender<EventEntry>>,
    writer: Option<JoinHandle<io::Result<()>>>,
    dropped: AtomicU64,
    origin: Instant,
    path: PathBuf,
}

enum EventEntry {
    Record(&'static str, &'static str, DequeueRecord),
    Finish(u64),
}

impl QueueEventRecorder {
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        let (_, unix_us, origin) = *BASE_TS;
        let file = OpenOptions::new().write(true).create_new(true).open(path)?;
        let (sender, receiver) = mpsc::sync_channel(BUFFER_RECORDS);
        let writer = thread::Builder::new()
            .name("aqm-log".into())
            .spawn(move || {
                let mut out = BufWriter::with_capacity(64 * 1024, file);
                writeln!(out, "# aqm_log_version=1 base_unix_us={unix_us}")?;
                writeln!(out, "event,reason,{HEADER}")?;
                let mut records = 0u64;
                for entry in receiver {
                    match entry {
                        EventEntry::Record(event, reason, record) => {
                            write!(out, "{event},{reason},")?;
                            record.write(&mut out)?;
                            records += 1;
                        }
                        EventEntry::Finish(dropped) => {
                            out.flush()?;
                            writeln!(out, "# complete records={records} dropped={dropped}")?;
                            return out.flush();
                        }
                    }
                }
                Err(io::Error::other("AQM log closed without completion"))
            })?;
        Ok(Self(Arc::new(EventLog {
            sender: Some(sender),
            writer: Some(writer),
            dropped: AtomicU64::new(0),
            origin,
            path: path.into(),
        })))
    }

    pub fn record(
        &self,
        packet: &impl Packet,
        at: Instant,
        event: &'static str,
        reason: &'static str,
    ) {
        let t_ns = match at.checked_duration_since(self.0.origin) {
            Some(dt) => dt.as_nanos() as i128,
            None => -(self.0.origin.duration_since(at).as_nanos() as i128),
        };
        let record = DequeueRecord {
            t_ns,
            sojourn_ns: at
                .saturating_duration_since(packet.get_timestamp())
                .as_nanos(),
            l3_len: packet.l3_length(),
            ip: IpFields::parse(packet.as_slice()),
        };
        if self
            .0
            .sender
            .as_ref()
            .unwrap()
            .try_send(EventEntry::Record(event, reason, record))
            .is_err()
        {
            self.0.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

impl Drop for EventLog {
    fn drop(&mut self) {
        let dropped = self.dropped.load(Ordering::Relaxed);
        if let Some(sender) = self.sender.take() {
            let _ = sender.send(EventEntry::Finish(dropped));
        }
        if let Some(writer) = self.writer.take() {
            match writer.join() {
                Ok(Ok(())) if dropped == 0 => {}
                result => {
                    tracing::error!(path = %self.path.display(), dropped, ?result, "AQM log incomplete")
                }
            }
        }
    }
}

impl DequeueRecord {
    fn write(&self, writer: &mut impl Write) -> io::Result<()> {
        write!(writer, "{},{},{}", self.t_ns, self.sojourn_ns, self.l3_len)?;
        let Some(ip) = &self.ip else {
            return writeln!(writer, ",,,,,,");
        };
        write!(
            writer,
            ",{},{},{},{}",
            ip.ecn, ip.protocol, ip.source, ip.destination
        )?;
        match ip.ports {
            Some((source, destination)) => writeln!(writer, ",{source},{destination}"),
            None => writeln!(writer, ",,"),
        }
    }
}

pub(super) struct DequeueRecorder {
    sender: Option<SyncSender<LogEntry>>,
    writer: Option<JoinHandle<io::Result<()>>>,
    dropped: u64,
    origin: Instant,
    path: PathBuf,
}

enum LogEntry {
    Record(DequeueRecord),
    Finish { dropped: u64 },
}

impl DequeueRecorder {
    pub(super) fn open(path: &Path) -> io::Result<Self> {
        let (_, origin_unix_us, origin) = *BASE_TS;
        let file = OpenOptions::new().write(true).create_new(true).open(path)?;
        let (sender, receiver) = mpsc::sync_channel(BUFFER_RECORDS);
        let writer = thread::Builder::new()
            .name("queue-log".into())
            .spawn(move || write_log(file, receiver, origin_unix_us))?;
        Ok(Self {
            sender: Some(sender),
            writer: Some(writer),
            dropped: 0,
            origin,
            path: path.into(),
        })
    }

    pub(super) fn record(&mut self, packet: &impl Packet, start: Instant, sojourn: Duration) {
        let Some(sender) = &self.sender else { return };
        let t_ns = match start.checked_duration_since(self.origin) {
            Some(since_origin) => since_origin.as_nanos() as i128,
            None => -(self.origin.duration_since(start).as_nanos() as i128),
        };
        let record = DequeueRecord {
            t_ns,
            sojourn_ns: sojourn.as_nanos(),
            l3_len: packet.l3_length(),
            ip: IpFields::parse(packet.as_slice()),
        };
        if sender.try_send(LogEntry::Record(record)).is_err() {
            self.dropped += 1;
        }
    }
}

impl Drop for DequeueRecorder {
    fn drop(&mut self) {
        if let Some(sender) = self.sender.take() {
            // A blocking send is appropriate here, once forwarding has ended.
            let _ = sender.send(LogEntry::Finish {
                dropped: self.dropped,
            });
        }
        let Some(writer) = self.writer.take() else {
            return;
        };
        match writer.join() {
            Ok(Ok(())) if self.dropped > 0 => tracing::warn!(
                path = %self.path.display(),
                dropped = self.dropped,
                "Queue log lost records"
            ),
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::error!(path = %self.path.display(), %error, "Queue log write failed")
            }
            Err(_) => tracing::error!(path = %self.path.display(), "Queue log writer panicked"),
        }
    }
}

fn write_log(
    file: impl Write,
    receiver: Receiver<LogEntry>,
    origin_unix_us: u64,
) -> io::Result<()> {
    let mut writer = BufWriter::with_capacity(64 * 1024, file);
    writeln!(
        writer,
        "# queue_log_version=1 base_unix_us={origin_unix_us}"
    )?;
    writeln!(writer, "{HEADER}")?;
    let mut written = 0u64;
    for entry in receiver {
        match entry {
            LogEntry::Record(record) => {
                record.write(&mut writer)?;
                written += 1;
            }
            LogEntry::Finish { dropped } => {
                writer.flush()?;
                writeln!(writer, "# complete records={written} dropped={dropped}")?;
                return writer.flush();
            }
        }
    }
    Err(io::Error::other(
        "queue log closed without a final record count",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cells::{PacketHeader, StdPacket};
    use etherparse::PacketBuilder;

    #[test]
    fn event_log_clones_finish_once_and_preserve_decision_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("events.csv");
        let first = QueueEventRecorder::open(&path).unwrap();
        let second = first.clone();
        let packet = StdPacket::from_raw_buffer(&[0; 64]);
        let at = first.0.origin + Duration::from_millis(1);
        first.record(&packet, at, "arrive", "input");
        drop(first);
        second.record(&packet, at, "drop", "overlimit");
        drop(second);
        let content = std::fs::read_to_string(path).unwrap();
        assert!(content.contains("arrive,input,1000000,"));
        assert!(content.contains("drop,overlimit,1000000,"));
        assert_eq!(
            content.lines().last(),
            Some("# complete records=2 dropped=0")
        );
        assert_eq!(content.matches("# complete").count(), 1);
    }

    #[test]
    fn queue_log_flushes_packet_metadata_and_refuses_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("queue.csv");
        let mut log = DequeueRecorder::open(&path).unwrap();
        assert_eq!(
            DequeueRecorder::open(&path).err().unwrap().kind(),
            io::ErrorKind::AlreadyExists
        );

        let payload = [0u8; 16];
        let builder = PacketBuilder::ethernet2([0; 6], [1; 6])
            .ipv6([0; 16], [1; 16], 64)
            .udp(5001, 12345);
        let mut frame = Vec::new();
        builder.write(&mut frame, &payload).unwrap();
        let mut packet = StdPacket::from_raw_buffer(&frame);
        packet.as_raw_buffer()[15] |= 0x10; // IPv6 ECT(1)
        assert!(packet.set_ce());

        let start = log.origin + Duration::from_millis(1);
        log.record(&packet, start, Duration::from_micros(800));
        let non_ip = StdPacket::from_raw_buffer(&[0; 64]);
        log.record(&non_ip, start, Duration::ZERO);
        drop(log);

        let data = std::fs::read_to_string(path).unwrap();
        let lines: Vec<_> = data.lines().collect();
        assert!(lines[0].starts_with("# queue_log_version=1 base_unix_us="));
        assert_eq!(lines[1], HEADER);
        assert_eq!(
            lines[2],
            "1000000,800000,64,3,17,::,101:101:101:101:101:101:101:101,5001,12345"
        );
        assert_eq!(lines[3], "1000000,0,50,,,,,,");
        assert_eq!(lines[4], "# complete records=2 dropped=0");
    }

    #[test]
    fn queue_log_counts_saturated_buffer_without_waiting() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("queue.csv");
        let file = std::fs::File::create(&path).unwrap();
        let (sender, receiver) = mpsc::sync_channel(1);
        let mut log = DequeueRecorder {
            sender: Some(sender),
            writer: None,
            dropped: 0,
            origin: Instant::now(),
            path: path.clone(),
        };
        let packet = StdPacket::from_raw_buffer(&[0; 64]);
        log.record(&packet, log.origin, Duration::ZERO);
        log.record(&packet, log.origin, Duration::ZERO);

        log.writer = Some(thread::spawn(move || write_log(file, receiver, 0)));
        drop(log);
        let data = std::fs::read_to_string(path).unwrap();
        assert_eq!(data.lines().last(), Some("# complete records=1 dropped=1"));
    }

    #[test]
    fn queue_log_write_error_prevents_completion() {
        struct FailsOnce {
            bytes: Vec<u8>,
            failed: bool,
        }

        impl Write for FailsOnce {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                if !self.failed {
                    self.failed = true;
                    return Err(io::Error::other("test write failed"));
                }
                self.bytes.write(buf)
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut output = FailsOnce {
            bytes: Vec::new(),
            failed: false,
        };
        let (sender, receiver) = mpsc::sync_channel(1);
        sender.send(LogEntry::Finish { dropped: 0 }).unwrap();
        assert!(write_log(&mut output, receiver, 0).is_err());
        assert!(!String::from_utf8(output.bytes)
            .unwrap()
            .contains("# complete"));
    }
}
