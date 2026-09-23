# Rattan artifact

Artifact for *#2689 Rattan: An Extensible and Scalable Modular Internet Path Emulator* (USENIX ATC '26), licensed under [Apache-2.0](../LICENSE).

## Quick start

The experiments run in two virtual machines, `micro` and `mptcp`. The `up.sh` script creates and configures them, including required reboots. `vagrant up` alone does not complete setup. **Keep only one VM running** to avoid competing for CPU and memory.

From the repository root on the host:

```bash
cd artifact
./setup-host.sh              # download and install time depends on network and host speed
```

**Log out and back in**, then return to `rattan/artifact/` on the host:

```bash
./up.sh micro                # ~15 min with a cached VM image, longer on the first run
vagrant ssh micro

# Inside micro
cd ~/rattan/artifact/microbench
./run-throughput.sh          # Figure 5, ~40 min
./run-complexity.sh          # Figure 6, ~1 h
./run-density.sh             # Figure 7, ~80 min
exit

# Back on the host
./fetch-results.sh micro     # save to results/micro/
vagrant halt micro
./up.sh mptcp                # ~15 min with a cached VM image, longer on the first run
vagrant ssh mptcp

# Inside mptcp
cd ~/rattan/artifact/mptcp
./run-benchmark.sh --paper   # Table 2, ~10 min
exit

# Back on the host
./fetch-results.sh mptcp     # save to results/mptcp/
vagrant halt mptcp
```

Guest setup and experiments took roughly **4 hours** on our 40-core host with VM images already downloaded. Host setup and the first guest setup take extra time for downloads, depending on network speed. Build times also vary with host performance.

## What is reproduced

`--quick` uses fewer test settings and repetitions for a shorter run. `--paper` runs the full experiment. We recommend starting with `--quick` for the microbenchmarks and using `--paper` directly for MPTCP.

| Paper item | Script | Guest | `--quick` | `--paper` |
| :--- | :--- | :--- | :--- | :--- |
| Figure 5: forwarding throughput | `microbench/run-throughput.sh` | `micro` | 40 min | 6 h |
| Figure 6: path complexity | `microbench/run-complexity.sh` | `micro` | 1 h | 5 h |
| Figure 7: emulation density | `microbench/run-density.sh` | `micro` | 80 min | 8 h+ |
| Table 2: MPTCP completion time | `mptcp/run-benchmark.sh` | `mptcp` | 5 min | 10 min |

Each experiment produces a CSV. The microbenchmarks also produce plots for Figures 5–7.

Times are approximate on our 40-core host and vary with hardware.

## Requirements

A Linux host with hardware virtualization, and:

| | For §5.1 (`micro`) | For §5.2 (`mptcp`) |
| :--- | :--- | :--- |
| Physical cores | 16 | 40 |
| Memory | 64 GiB | 16 GiB |
| Disk | 60 GB | 128 GB |

Keep these cores free of other workloads during experiments. Setup requires Internet access and installs all dependencies. No proprietary software is needed.

**Paper and artifact environments.** §5.1 used an Intel Core i7-10700F with 64 GiB RAM, Debian 13 and Linux 6.12.88. §5.2 used an Intel Xeon E5-2690 v2 with 128 GiB RAM, Ubuntu 24.04 and MPTCP v0.96. The guests use the same distributions, with Linux 6.12 for `micro` and MPTCP v0.96 based on Linux 5.4.301 for `mptcp`.

### Dependencies

| What | Version | Installed by |
| :--- | :--- | :--- |
| Rattan and the RVNIC module | this repository | `scripts/build-rattan.sh` |
| Rust | current stable via rustup | `scripts/install-rust.sh` |
| Mahimahi | pinned commit | `scripts/install-mahimahi.sh` |
| Mininet | pinned commit, plus one patch | `scripts/install-mininet.sh` |
| MPTCP v0.96 kernel | pinned commit, Linux 5.4.301 | `scripts/setup-mptcp-kernel.sh` |
| iperf3, Open vSwitch, matplotlib | the distribution's | the setup scripts |

Mahimahi, Mininet and MPTCP revisions are fixed in [scripts/versions.sh](scripts/versions.sh). Mahimahi's fork removes unused web-replay tools. The [Mininet patch](patches/mininet-raise-bwParamMax.patch) allows rates above 1 Gbps.

## Setting up

Run on the host, from the repository root:

```bash
cd artifact
./setup-host.sh              # download and install time depends on network and host speed
```

This installs libvirt, QEMU and Vagrant. **Log out and back in afterwards** for the new group permissions to take effect. If your host runs a firewall other than `ufw`, allow traffic from the guest network, `192.168.121.0/24`. Behind a proxy, export `RATTAN_AE_HTTP_PROXY` before creating a guest. This requires the `vagrant-proxyconf` plugin.

```bash
./up.sh micro     # ~15 min with a cached VM image, longer on the first run
vagrant ssh micro
```

`up.sh` installs dependencies, reboots the guest and completes the build. Repeat `./up.sh micro` to resume a failed setup. CPU placement is automatic and can be overridden with `RATTAN_AE_CPUSET_MICRO` or `RATTAN_AE_CPUSET_MPTCP` before creating the guest.

After finishing §5.1, switch to the other guest:

```bash
exit
vagrant halt micro
./up.sh mptcp     # ~15 min with a cached VM image, longer on the first run
vagrant ssh mptcp
```

**Run only one guest at a time.** Use `vagrant halt mptcp` before returning to `micro`. The repository is at `~/rattan` inside each guest.

### Minimal example

Inside `micro`, run one forwarding-throughput measurement:

```bash
cd ~/rattan/artifact/microbench
./run-throughput.sh --max-gbps 0.5 --trials 1 --emulators rattan
```

The result directory contains `figure5-throughput.csv` and `figure5-throughput.png`. At this setting, throughput should be close to 0.5 Gbps. Change `--max-gbps` to `1` to add a second bandwidth setting.

## Running the experiments

Run inside the indicated guest, choosing either `--quick` or `--paper` for each experiment. Use `--help` for available options. Run the scripts as your regular user. They call `sudo` internally and may prompt for your password.

### Figure 5: forwarding throughput (`micro`)

```bash
cd ~/rattan/artifact/microbench
./run-throughput.sh --quick  # ~40 min
./run-throughput.sh --paper  # ~6 h
```

Sweeps the bottleneck bandwidth of one bandwidth-delay path with 25 ms delay in each direction, and measures what each emulator delivers over one long-lived TCP CUBIC flow.

### Figure 6: path complexity (`micro`)

```bash
cd ~/rattan/artifact/microbench
./run-complexity.sh --quick  # ~1 h
./run-complexity.sh --paper  # ~5 h
```

Holds the target at 1 Gbps and deepens the path instead: complexity *N* is *N* chained loss, bandwidth and delay sequences, each 0% loss, 1 Gbps and 1 ms. Mahimahi may fail at high complexity, as described in the paper.

### Figure 7: emulation density (`micro`)

```bash
cd ~/rattan/artifact/microbench
./run-density.sh --quick     # ~80 min
./run-density.sh --paper     # ~8 h or more
```

Runs 8 to 512 concurrent instances of one path, each 16 Mbps with 20 ms delay in each direction carrying one TCP CUBIC flow, and reports the mean and standard deviation across instances. Two panels: static bandwidth, and a trace whose mean is 16 Mbps. Mininet has no trace replay, so it appears only in 7(a).

### Table 2: MPTCP flow completion time (`mptcp`)

```bash
cd ~/rattan/artifact/mptcp
./run-benchmark.sh --paper   # ~10 min, recommended
./run-benchmark.sh --quick   # ~5 min
```

Times a 50 MiB transfer over one MPTCP connection with a subflow on each of two emulated paths, for four path scenarios, three subflow schedulers and six congestion control algorithms. [mptcp/config/](mptcp/config/) contains the four scenarios described in §5.2. `--quick` repeats each combination 3 times and `--paper` repeats it 10 times, as in the paper.

## Reading the results

Each run saves its results, raw logs and environment information in a separate directory under `microbench/results/` or `mptcp/results/`.

For example:

```text
microbench/results/throughput-quick-<timestamp>/
├── figure5-throughput.csv     Mean throughput and standard deviation
├── figure5-throughput.png     Figure 5
├── result.json               Individual measurements and skipped logs
├── run.toml                  Environment and experiment settings
├── rattan/
│   └── tput-3000mbps/
│       ├── <trial-id>.log           Raw iperf3 output
│       └── <trial-id>.emulator.txt  Emulator output
├── mahimahi/
└── mininet/
```

The CSVs carry a mean and a standard deviation per emulator per setting. Microbenchmark throughputs are in Kbit/s, `table2-mptcp-fct.csv` is in seconds. Empty values indicate missing measurements. In the density experiment, instances that fail to produce a measurement count as zero throughput.

Following §5.1, throughput is averaged after discarding slow-start warm-up. Results include packet-header overhead corrections. Figures 5 and 6 exclude outliers. Raw logs are retained.

Absolute performance may vary with hardware and system configuration. The overall performance trends relative to the Mahimahi and Mininet baselines are generally expected to remain consistent with the paper.

Results are generated automatically. To regenerate them from saved logs, including after an interrupted run:

```bash
cd ~/rattan/artifact
./analyze/report.sh throughput "microbench/results/<run-directory>"
```

Results stay in the guest. To copy them out, run on the host:

```bash
./fetch-results.sh micro     # or mptcp, into results/<guest>/
```

## Expected warnings

- Mininet may report `HTB: quantum of class 10001 is big` at high bandwidths.
- Rattan may report `first-payload is disabled` in §5.1. This is expected.
- **Mahimahi does not finish the deepest paths, or serve every instance at the highest concurrency.** These limits are measured by Figures 6 and 7.
- `iperf3 exit status: 124` indicates a timeout, which can occur at high density.
- The `mptcp` guest may show systemd warnings with its older kernel. IPv6 is disabled. The experiments use IPv4.

## Troubleshooting

- **`vagrant` says permission denied, or cannot reach libvirt.** The group membership from `setup-host.sh` has not taken effect. Log out and back in.
- **A setup stage failed partway.** Run `./up.sh <guest>` again. Every stage skips what is already done.
- **`cannot load the rattan_vnic module`.** The guest is running a kernel the module was not built against. Rebuild with `~/rattan/artifact/scripts/build-rattan.sh micro`.
- **The `mptcp` guest booted the wrong kernel.** `uname -r` should say `5.4.301`. Run `./up.sh mptcp` again on the host.

After changing source files on the host, run `vagrant rsync micro`, then `~/rattan/artifact/scripts/build-rattan.sh micro` inside the guest. Use `mptcp` instead for the other guest.

## Project structure

```
artifact/
├── setup-host.sh              Install Vagrant, libvirt and QEMU on the host
├── up.sh                      Create and fully provision one guest
├── fetch-results.sh           Copy a guest's results onto the host
├── Vagrantfile                Guest definitions, started through up.sh
├── scripts/                   Dependency installation and builds
├── patches/                   Dependency modifications
├── microbench/                §5.1, Figures 5, 6 and 7
├── mptcp/                     §5.2, Table 2
│   ├── config/                  The four path scenarios, A to D
│   └── app/                     The transfer application
└── analyze/                   Runs into CSVs and figures
```

The emulator is in `../src/` and `../rattan-core/`. Its virtual NIC and kernel module are in `../rvnic/`. See the [user guide](../guide/src/README.md) for configuration and extension.
