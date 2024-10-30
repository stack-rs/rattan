# Flamegraph

To generate a flamegraph, you can use the `flamegraph` tool. First, install it:

```bash
cargo install flamegraph
```

Then, run the following command:

```bash
cargo flamegraph --root --example channel
```

This will generate a flamegraph, which you can open with a browser to inspect hot spot.
