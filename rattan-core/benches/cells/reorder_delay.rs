use std::time::Duration;

use criterion::{measurement::WallTime, BenchmarkGroup, BenchmarkId};
use rand::random_range;
use rattan_core::cells::{
    reorder_delay::{
        delay::{DelayGenerator, LogNormalLawDelayGenerator, NormalLawDelayGenerator},
        ReorderDelayCell, ReorderDelayCellConfig,
    },
    Cell as _, Egress as _, Ingress as _, Packet, StdPacket,
};
use tokio::{runtime::Handle, time::Interval};

use crate::{utils::clock, MTU};

fn init<P: Packet + Sync, D: DelayGenerator + Send + Sync + 'static>(
    delay: D,
    handle: &Handle,
) -> ReorderDelayCell<P, D> {
    let config = ReorderDelayCellConfig::new(delay);
    crate::utils::create_cell(config.into_factory(), handle).expect("Failed to create the cell")
}

async fn test<P: Packet + Sync, D: DelayGenerator + Send + Sync + 'static>(
    cell: &mut ReorderDelayCell<P, D>,
    clock: &mut Interval,
) {
    let packet = rand::random_iter()
        .take(random_range(0..=MTU))
        .collect::<Vec<_>>();
    let packet = P::from_raw_buffer(&packet);
    // println!("Packet length: {} bytes", packet.length());
    cell.sender()
        .enqueue(packet)
        .expect("Failed to send the packet");
    // println!("Packet enqueued");
    while cell.receiver().dequeue().await.is_none() {
        // println!("Packet failed to dequeue");
        clock.tick().await;
    }
    // println!("Packet dequeued");
}

pub fn run(group: &mut BenchmarkGroup<WallTime>, handle: &Handle) {
    for delay in [0, 1, 2, 5, 10, 20, 50, 100, 200, 500, 1000]
        .into_iter()
        .map(Duration::from_millis)
    {
        group
            .warm_up_time((10 * delay).max(Duration::from_secs(1)))
            .measurement_time(
                (100 * delay)
                    .max(Duration::from_secs(5))
                    .min(Duration::from_secs(30)),
            )
            .sample_size(30)
            .bench_with_input(
                BenchmarkId::new("Constant", format!("{}ms", delay.as_millis())),
                &delay,
                |b, &delay| {
                    b.to_async(handle).iter_custom(|batch_size| async move {
                        let mut cell = init::<StdPacket, Duration>(delay, handle);
                        let mut clock = clock();
                        let start = std::time::Instant::now();
                        for _i in 0..batch_size {
                            test(&mut cell, &mut clock).await;
                        }
                        start.elapsed()
                    })
                },
            )
            .bench_with_input(
                BenchmarkId::new(
                    "Normal Law",
                    format!(
                        "{}ms±{}",
                        delay.as_millis(),
                        if delay.as_millis() / 10 > 1 {
                            format!("{}", delay.as_millis() / 10)
                        } else {
                            format!("{:.1}", delay.as_millis() as f64 / 10.0)
                        }
                    ),
                ),
                &delay,
                |b, &delay| {
                    b.to_async(handle).iter_custom(|batch_size| async move {
                        let mut cell = init::<StdPacket, NormalLawDelayGenerator>(
                            NormalLawDelayGenerator::new(delay, delay / 10)
                                .expect("Failed to create the delay"),
                            handle,
                        );
                        let mut clock = clock();
                        let start = std::time::Instant::now();
                        for _i in 0..batch_size {
                            test(&mut cell, &mut clock).await;
                        }
                        start.elapsed()
                    })
                },
            )
            .bench_with_input(
                BenchmarkId::new(
                    "Log-Normal Law",
                    format!(
                        "{}ms±{}",
                        delay.as_millis(),
                        if delay.as_millis() / 10 > 1 {
                            format!("{}", delay.as_millis() / 10)
                        } else {
                            format!("{:.1}", delay.as_millis() as f64 / 10.0)
                        }
                    ),
                ),
                &delay,
                |b, &delay| {
                    b.to_async(handle).iter_custom(|batch_size| async move {
                        let mut cell = init::<StdPacket, LogNormalLawDelayGenerator>(
                            LogNormalLawDelayGenerator::new(delay, delay / 10)
                                .expect("Failed to create the delay"),
                            handle,
                        );
                        let mut clock = clock();
                        let start = std::time::Instant::now();
                        for _i in 0..batch_size {
                            test(&mut cell, &mut clock).await;
                        }
                        start.elapsed()
                    })
                },
            );
        if delay != std::time::Duration::ZERO {
            group
                .bench_with_input(
                    BenchmarkId::new(
                        "Normal Law",
                        format!(
                            "{}ms±{}",
                            delay.as_millis(),
                            if delay.as_millis() / 5 > 1 {
                                format!("{}", delay.as_millis() / 5)
                            } else {
                                format!("{:.1}", delay.as_millis() as f64 / 5.0)
                            }
                        ),
                    ),
                    &delay,
                    |b, &delay| {
                        b.to_async(handle).iter_custom(|batch_size| async move {
                            let mut cell = init::<StdPacket, NormalLawDelayGenerator>(
                                NormalLawDelayGenerator::new(delay, delay / 5)
                                    .expect("Failed to create the delay"),
                                handle,
                            );
                            let mut clock = clock();
                            let start = std::time::Instant::now();
                            for _i in 0..batch_size {
                                test(&mut cell, &mut clock).await;
                            }
                            start.elapsed()
                        })
                    },
                )
                .bench_with_input(
                    BenchmarkId::new(
                        "Log-Normal Law",
                        format!(
                            "{}ms±{}",
                            delay.as_millis(),
                            if delay.as_millis() / 5 > 1 {
                                format!("{}", delay.as_millis() / 5)
                            } else {
                                format!("{:.1}", delay.as_millis() as f64 / 5.0)
                            }
                        ),
                    ),
                    &delay,
                    |b, &delay| {
                        b.to_async(handle).iter_custom(|batch_size| async move {
                            let mut cell = init::<StdPacket, LogNormalLawDelayGenerator>(
                                LogNormalLawDelayGenerator::new(delay, delay / 5)
                                    .expect("Failed to create the delay"),
                                handle,
                            );
                            let mut clock = clock();
                            let start = std::time::Instant::now();
                            for _i in 0..batch_size {
                                test(&mut cell, &mut clock).await;
                            }
                            start.elapsed()
                        })
                    },
                );
        };
    }
}
