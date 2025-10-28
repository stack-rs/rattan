use std::{sync::Arc, time::Duration};

use bandwidth::Bandwidth;
use criterion::{measurement::WallTime, BenchmarkGroup, BenchmarkId};
use human_bandwidth::binary_system::format_binary_bandwidth;
use rand::random_range;
use rattan_core::cells::{
    bandwidth::{
        queue::{InfiniteQueue, InfiniteQueueConfig},
        BwCell, BwCellConfig, BwType,
    },
    Cell as _, Egress as _, Ingress as _, Packet, StdPacket,
};
use tokio::{
    runtime::Handle,
    time::{Instant, Interval},
};

use crate::{utils::clock, MTU};

type Cell<P> = BwCell<P, InfiniteQueue<P>>;

fn init<P: Packet + Sync>(bandwidth: Bandwidth, handle: &Handle) -> Cell<P> {
    let config = BwCellConfig::<P, InfiniteQueue<P>>::new(
        Some(bandwidth),
        Some(InfiniteQueueConfig::new()),
        Some(BwType::LinkLayer),
    );
    crate::utils::create_cell(config.into_factory(), handle).expect("Failed to create the cell")
}

async fn test<P: Packet + Sync>(cell: &mut Cell<P>, clock: &mut Interval, total: usize) {
    let send = |sender: Arc<<Cell<P> as rattan_core::cells::Cell<P>>::IngressType>| async move {
        let mut sent = 0;
        while sent < total {
            let size = random_range(14..=MTU.min(total - sent));
            let size = if total - (sent + size) < 14 {
                if total - sent > MTU {
                    (size - 14).max(14)
                } else {
                    total - sent
                }
            } else {
                size
            };
            let packet = rand::random_iter().take(size).collect::<Vec<_>>();
            let packet = P::from_raw_buffer(&packet, Instant::now());
            // println!("Sent: {sent} bytes");
            sent += packet.l2_length();
            sender.enqueue(packet).expect("Failed to send the packet");
        }
        // println!("Finish send: {sent} bytes");
    };
    async fn recv<'a, P: Packet + Sync>(
        receiver: &'a mut <Cell<P> as rattan_core::cells::Cell<P>>::EgressType,
        total: usize,
        clock: &'a mut Interval,
    ) {
        let mut received = 0;
        while received < total {
            if let Some(packet) = receiver.dequeue().await {
                // println!("Received: {received} bytes");
                received += packet.l2_length();
            } else {
                // println!("Tick");
                clock.tick().await;
            }
        }
        // println!("Finished recv: {received} bytes");
    }
    tokio::join!(
        send(cell.sender().clone()),
        recv(cell.receiver(), total, clock)
    );
}

pub fn run(group: &mut BenchmarkGroup<WallTime>, handle: &Handle) {
    for bandwidth in [1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096]
        .into_iter()
        .map(|mibps| Bandwidth::from_bps(mibps * 1024 * 1024))
    {
        let size = (bandwidth.as_bps() / 8) as u64;
        group
            .warm_up_time(Duration::from_secs(1))
            .measurement_time(Duration::from_secs(32))
            .sample_size(30)
            .throughput(criterion::Throughput::Bytes(size))
            .bench_with_input(
                BenchmarkId::new("Bandwidth", format_binary_bandwidth(bandwidth)),
                &bandwidth,
                |b, &delay| {
                    b.to_async(handle).iter_custom(|batch_size| async move {
                        let mut cell = init::<StdPacket>(delay, handle);
                        let mut clock = clock();
                        let start = std::time::Instant::now();
                        test(&mut cell, &mut clock, (batch_size * size) as usize).await;
                        start.elapsed()
                    })
                },
            );
    }
}
