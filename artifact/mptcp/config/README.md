# The four multipath scenarios

`scenario-a.toml` to `scenario-d.toml` are the scenarios §5.2 calls A to D. Each describes a pair of emulated paths between a sender and a receiver.

| | Path 1 | Path 2 | Shared bottleneck |
| :--- | :--- | :--- | :--- |
| A | 60 Mbps, 35 ms mean delay, 0.1% loss | 90 Mbps, 45 ms mean delay, no loss | 75 Mbps |
| B | as A | as A | none |
| C | 60 Mbps mean trace, high variance, shallow buffer, 40 ms delay | 60 Mbps mean trace, low variance, deep buffer, delay switching every 2 s | none |
| D | as C, but delay switching between 22 and 56.5 ms every 10 ms | as C | none |

In A and B all bandwidth stages use BDP-sized drop-tail queues. C and D have no loss.

The `delay-*.json` and `trace-*.json` files contain the delay patterns and bandwidth traces used by these scenarios.
