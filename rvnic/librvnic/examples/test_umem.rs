//! Test UMEM and rings registration with the rattan-vnic kernel module
//!
//! This test creates TWO vNIC devices (rattan0 and rattan1) sharing a single UMEM.
//! This validates the shared UMEM functionality required for packet forwarding.
//!
//! This example uses the queue-0 convenience API:
//! - `register_rings(...)`
//! - `kick_rx()`
//!
//! So it assumes the kernel module is loaded in single-queue mode:
//!   sudo insmod rattan_vnic.ko num_queues=1
//!
//! For a multiqueue setup, see:
//!   examples/test_forward_multiqueue.rs
//!
//! Run this with: sudo ./target/debug/examples/test_umem
//!
//! Prerequisites:
//!   1. Build and load the kernel module in single-queue mode:
//!      cd rvnic/kernel && make && sudo insmod rattan_vnic.ko num_queues=1
//!   2. Verify device exists:
//!      ls -la /dev/rattan-vnic

use rvnic::{Rings, RvnicDevice, UmemBuilder};

const DEV0_CHUNK_SHARE_PERCENT: u32 = 50;
const DEV1_CHUNK_SHARE_PERCENT: u32 = 100 - DEV0_CHUNK_SHARE_PERCENT;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Rattan vNIC Shared UMEM Test ===\n");
    println!("This test creates two vNICs sharing a single UMEM region.");
    println!(
        "Chunk split: dev0={}%, dev1={}%\n",
        DEV0_CHUNK_SHARE_PERCENT, DEV1_CHUNK_SHARE_PERCENT
    );

    // Step 1: Open first device (rattan0)
    println!("1. Opening first device (rattan0)...");
    let mut dev0 = RvnicDevice::open()?;
    println!("   rattan0 opened (fd={})", dev0.fd());

    // Step 2: Open second device (rattan1)
    println!("\n2. Opening second device (rattan1)...");
    let mut dev1 = RvnicDevice::open()?;
    println!("   rattan1 opened (fd={})", dev1.fd());

    // Step 3: Allocate UMEM (will be shared between both devices)
    println!("\n3. Allocating shared UMEM...");
    let umem = UmemBuilder::new()
        .chunk_size(2048)
        .headroom(128)
        .num_chunks(128) // 128 * 2048 = 256KB, enough for both devices
        .build()?;

    println!("   UMEM allocated:");
    println!("     - Address: 0x{:x}", umem.addr());
    println!("     - Length: {} bytes", umem.len());
    println!("     - Chunk size: {} bytes", umem.chunk_size());
    println!("     - Headroom: {} bytes", umem.headroom());
    println!("     - Num chunks: {}", umem.num_chunks());

    // Step 4: Register UMEM with rattan0 (owner)
    println!("\n4. Registering UMEM with rattan0 (owner)...");
    dev0.register_umem(umem)?;
    println!("   UMEM registered with rattan0");
    println!("   has_umem: {}", dev0.has_umem());

    // Step 5: Share UMEM with rattan1
    println!("\n5. Sharing UMEM with rattan1...");
    dev1.share_umem(&dev0)?;
    println!("   UMEM shared with rattan1");
    println!("   dev1.has_umem: {}", dev1.has_umem());

    // Step 6: Allocate rings for rattan0
    println!("\n6. Allocating rings for rattan0...");
    let rings0 = Rings::new()?;
    println!(
        "   Rings allocated: addr=0x{:x}, len={}",
        rings0.addr(),
        rings0.len()
    );
    let (mut fill0, _, _, _) = dev0.register_rings_for_queue(0, rings0)?;
    println!("   Rings registered with rattan0");

    // Step 7: Allocate rings for rattan1
    println!("\n7. Allocating rings for rattan1...");
    let rings1 = Rings::new()?;
    println!(
        "   Rings allocated: addr=0x{:x}, len={}",
        rings1.addr(),
        rings1.len()
    );
    let (mut fill1, _, _, _) = dev1.register_rings_for_queue(0, rings1)?;
    println!("   Rings registered with rattan1");

    // Step 8: Pre-fill FILL rings
    // Split chunks between the two devices according to the configured
    // directional chunk share. Chunks should be recycled back to the same
    // device that originally supplied them, so this split models the initial
    // per-device buffer budget.
    println!("\n8. Pre-filling FILL rings...");
    let umem = dev0.umem().unwrap();
    let chunk_size = umem.chunk_size() as u64;
    let num_chunks = umem.num_chunks();
    let dev0_chunks = num_chunks * DEV0_CHUNK_SHARE_PERCENT / 100;
    let dev1_chunks = num_chunks - dev0_chunks;

    // Device 0 gets the first portion of chunks
    let addrs0: Vec<u64> = (0..dev0_chunks).map(|i| (i as u64) * chunk_size).collect();
    let count0 = fill0.produce(&addrs0);
    println!(
        "   Pushed {} chunks to rattan0 FILL ring (configured share: {}%)",
        count0, DEV0_CHUNK_SHARE_PERCENT
    );

    // Device 1 gets the remainder
    let addrs1: Vec<u64> = (dev0_chunks..num_chunks)
        .map(|i| (i as u64) * chunk_size)
        .collect();
    let count1 = fill1.produce(&addrs1);
    println!(
        "   Pushed {} chunks to rattan1 FILL ring (configured share: {}%)",
        count1, DEV1_CHUNK_SHARE_PERCENT
    );

    println!(
        "   Initial chunk budget: dev0={} chunks, dev1={} chunks",
        dev0_chunks, dev1_chunks
    );

    // Step 9: Summary
    println!("\n9. Setup complete!");
    println!();
    println!("    Summary:");
    println!("      - rattan0: owns UMEM, {} chunks in FILL ring", count0);
    println!(
        "      - rattan1: shares UMEM, {} chunks in FILL ring",
        count1
    );
    println!();
    println!("    Both devices have rings registered and are ready.");
    println!("    Devices will be cleaned up on exit.");
    println!();
    println!("=== Test Passed ===");

    Ok(())
}
