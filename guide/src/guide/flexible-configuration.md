# Flexible Configuration

Generally, our specialized network emulations necessitate a different number of cells or more complex channels, and they may even require the configuration of routes, such as a multi-path network path with ISP QoS policies. Our CLI tool achieves flexibility and configurability through the use of TOML config files. The configuration file primarily consists of six sections.

We provide an example config file [config.example.toml](https://github.com/stack-rs/rattan/blob/main/config.example.toml) as reference. You can just copy and modify it to suit your needs.

For detailed usage, you can run `rattan config --help` to see the available cells and options:

```txt
Run the instance according to the config

Usage: rattan run [OPTIONS] --config <Config File>

Options:
  -c, --config <Config File>        Use config file to run a instance
      --left [<LEFT_COMMAND>...]    Command to run in left ns. Can be used in compatible and isolated mode
      --generate                    Generate config file instead of running a instance
      --right [<RIGHT_COMMAND>...]  Command to run in right ns. Only used in isolated mode
      --generate-path <File>        Generate config file to the specified path instead of stdout
      --packet-log <File>           The file to store compressed packet log (overwrite config) (default: None)
      --file-log                    Enable logging to file
      --file-log-path <File>        File log path, default to $CACHE_DIR/rattan/core.log
  -h, --help                        Print help (see more with '--help')
  -V, --version                     Print version
```

## General Section

This section determines how to run the rattan itself in general.

- `packet_log`. Now we only have this option in the section, which is used to enable compressed packet log and specify the file to store the records. The meta data of flows will be stored in the file of name `packet_log`.flows

## Environment Section

This section determines how to build the environment.

- `mode`. You can configure rattan to function in `Compatible` mode or `Isolated` mode. The `ns-left` side is always a new network namespace. The difference of the two modes lies in whether `ns-right` side is the host network namespace (the namespace run rattan) or a new network namespace. The default is `Compatible`.
- `left_veth_count` and `right_veth_count`. You can configure the number of veth pairs to create in the `ns-left` (i.e., between `ns-left` and `ns-rattan`) and `ns-right` (i.e., between `ns-right` and `ns-rattan`) network namespaces. The default is 1.

## HTTP Section

This section configures Rattan's HTTP control server.

- `enable`. You can enable or disable the HTTP control server. The default is `false`.
- `port`. You can configure the port number of the HTTP control server. The default is 8086.

## Cells Section

This section describes the emulation cells. It is a table of cells, where each cell has a unique string ID, defined like `[cells.<ID>]`.

Each cell **MUST** have a "type" field, which determines the cell type and cell configuration fields.
Possible cell types:

- `Bw`: Fixed bandwidth
- `BwReplay`: Bandwidth trace replayer
- `Delay`: Fixed delay
- `DelayPerPacket`: Delay specified per-packet
- `Loss`: Loss pattern
- `Custom`: Custom cell, only specify the cell ID used in link section, and the cell must be built by the user

For example, the following is a cell configuration for a fixed bandwidth cell:

```toml
[cells.up_1]
type = "Bw"
bw_type = "NetworkLayer"
bandwidth = "1Gbps 200Mbps"
queue = "DropTail"
queue_config = { packet_limit = 60 }
```

For detailed parameters, you can check the [documentation of `rattan-core`](https://docs.rs/rattan-core/latest/rattan-core) or the example config file [config.example.toml](https://github.com/stack-rs/rattan/blob/main/config.example.toml).

## Links Section

This section describes how to link the cells.

"left" and "right" are preserved cell IDs, which respectively represent
the `ns-left` side veth (named `vL1-R`) and the `ns-right` side veth (named `vR1-L`).
But if a network namespace has more than one veth NICs, `ns-left` side for example, "left1", "left2" etc. will be defined.

The other cell IDs are defined by user in the cells section.

Each line describes a one-way link from a cell egress to a cell ingress.

For example, to emulate a network topology with two network paths like following:

```txt
+-------+  -------->  shadow --->  up_1  --->  up_2  ---> +-------+
| left1 | <==+           ^                                | right |
+-------+    }= router <-|------- down_2 <--- down_1 <--- +-------+
| left2 | <==+           |
+-------+  --------------+

Note: The double line "<===" from router is connected in Cells Section
```

You can configure the link section like this:

```toml
[links]
left1 = "shadow"
left2 = "shadow"
shadow = "up_1"
up_1 = "up_2"
up_2 = "right"
right = "down_1"
down_1 = "down_2"
down_2 = "router"
# We do not need to link the Router Cell to other cells anymore, as we should do it in Cells Section
```

## Resource Section

This section describes how rattan use system resource

The section is preserved for future use but not used now.

- `cores`. The CPU cores that rattan is allowed to use.

## Commands Section

This section describes the commands to run in rattan.

When in `Compatible` mode, only the `left` and `shell` options are used,
and the commands will run in the `ns-left` network namespace.

When in `Isolated` mode, only the left and right options are used,
and the commands will run in the `ns-left` and `ns-right` network namespace, respectively.

- `left` and `right`. The commands to run in the `ns-left` and `ns-right` network namespace, respectively. Should be an array of strings.
- `shell`. The shell used to run if no command is specified. Only used when in compatible mode. The default is `Default` (i.e., the default shell defined in environment variable `$SHELL`).
