use criterion::{criterion_group, criterion_main, Criterion};

mod bandwidth;
mod delay;
mod delay_per_packet;

mod utils;

static MTU: usize = 1500;
static TICK: tokio::time::Duration = tokio::time::Duration::from_nanos(100);

fn criterion_benchmark(c: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().expect("Failed to create the runtime");
    // let mut group = c.benchmark_group("Cell Test");
    // group.sample_size(10);

    delay::run(&mut c.benchmark_group("Delay Cell"), runtime.handle());
    delay_per_packet::run(
        &mut c.benchmark_group("Delay Per-Packet Cell"),
        runtime.handle(),
    );
    bandwidth::run(&mut c.benchmark_group("Bandwidth Cell"), runtime.handle());

    // group.finish();
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
