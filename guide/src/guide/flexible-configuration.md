# Flexible Configuration

Generally, our specialized network emulations necessitate a different number of cells or more complex channels, and they may even require the configuration of routes, such as a multi-path network path with ISP QoS policies. Our CLI tool achieves flexibility and configurability through the use of TOML config files. The configuration file primarily consists of six sections.

We provide an example config file [config.example.toml](https://github.com/stack-rs/rattan/blob/main/config.example.toml) as reference. You can just copy and modify it to suit your needs.

## Usage

It is very straightforward to use a config file to run a rattan instance. You can run the following command:

```bash
rattan run -c config.example.toml --left=echo --left=hello
```

This will just print "hello" in the terminal as the `config.example.toml` is in `Compatible Mode`.

For large scale tests, we recommend you to use the `Isolated Mode`. Both sides of Rattan (typically one running client and one running server) are isolated to a independent network namespace.

### An Illustration Example of Testing

We are going to illustrate a simple example of testing network performance using `rattan link` and `iperf3` in `Isolated Mode` with a config file defined.
Before testing, don't remember to disable your firewall to allow `iperf3` traffic, or allow from `10.0.0.0/8`,
as Rattan is using that as the default IP addresses for the emulated network.

We should first define two scripts to use in the config file:

We could create a script `iperf3_server.sh` as follows:

```bash
#!/bin/bash
iperf3 -s -1 -J --logfile iperf3_server.log
```

And a script `iperf3_client.sh` as follows:

```bash
#!/bin/bash
sleep 1 # we sleep 1 second to wait for server to be ready
iperf3 -c $RATTAN_BASE -t 10 -J --logfile iperf3_client.log
```

We output the client and server results to json log files, as we are running in `Isolated Mode` and cannot see the terminal output directly.

After that, we can create a config file `iperf3_config.toml` as follows:

```toml
[env]
mode = "Isolated"
left_veth_count = 1
right_veth_count = 1

[links]
left = "up_loss"
up_loss = "up_bandwidth"
up_bandwidth = "up_delay"
up_delay = "right"
right = "down_loss"
down_loss = "down_bandwidth"
down_bandwidth = "down_delay"
down_delay = "left"

[cells.up_delay]
type = "Delay"
delay = "50ms"

[cells.up_bandwidth]
type = "Bw"
queue = "Infinite"
bandwidth = "12Mbps"

[cells.up_loss]
type = "Loss"
pattern = []

[cells.down_delay]
type = "Delay"
delay = "50ms"

[cells.down_bandwidth]
type = "Bw"
queue = "DropTail"
bandwidth = "12Mbps"
[cells.down_bandwidth.queue_config]
byte_limit = 125000

[cells.down_loss]
type = "Loss"
pattern = [ 0.0,]

[commands]
left = ["bash", "iperf3_client.sh"]
right = ["bash", "iperf3_server.sh"]
```

This creates 12Mbps bandwidth, 100ms RTT and no loss link between the two network namespaces.
The only difference of it with the [example of rattan link](predefined-channels.md#an-illustration-example-of-testing) is that now we define a `DropTail` queue in the downlink.

The `commands` section specifies the commands to run in the two network namespaces, in this case, the `iperf3` client and server scripts we defined before.

Now we can run the rattan instance with the config file:

```bash
rattan run -c iperf3_config.toml
```

Wait for it to finish, and then we can check the `iperf3_client.log` and `iperf3_server.log` files for the results.

If you want to see the terminal output of both sides, you should first remove the `-J --logfile xxx.log` part from both client and server side iperf3 scripts.
Then you can add the `--left-stdout` and `--right-stdout` options to `rattan run` command like this:

```bash
rattan run -c iperf3_config.toml --left-stdout --right-stdout
```

### Available Options

For detailed usage, you can run `rattan config --help` to see the available cells and options:

```txt
Run the instance according to the config

Usage: rattan run [OPTIONS] --config <Config File>

Options:
  -c, --config <Config File>
          Use config file to run a instance

      --left [<LEFT_COMMAND>...]
          Command to run in left ns. Can be used in compatible and isolated mode

      --generate
          Generate config file instead of running a instance

          If this flag is set, the program will only generate the config to stdout and exit.

      --right [<RIGHT_COMMAND>...]
          Command to run in right ns. Only used in isolated mode

      --left-stdout
          Used in isolated mode only. If set, stdout of left is passed to output of this program

      --right-stdout
          Used in isolated mode only. If set, stdout of rihgt is passed to output of this program

      --left-stderr
          Used in isolated mode only. If set, stderr of left is passed to output of this program

      --right-stderr
          Used in isolated mode only. If set, stderr of right is passed to output of this program

      --generate-path <File>
          Generate config file to the specified path instead of stdout

      --packet-log <File>
          The file to store compressed packet log (overwrite config) (default: None)

      --file-log
          Enable logging to file

      --file-log-path <File>
          File log path, default to $CACHE_DIR/rattan/core.log

      --no-nat
          Disable NAT in compatible mode

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```

## Explanation of Config Sections

### General Section

This section determines how to run the rattan itself in general.

- `packet_log`. Now we only have this option in the section, which is used to enable compressed packet log and specify the file to store the records. The meta data of flows will be stored in the file of name `packet_log`.flows

### Environment Section

This section determines how to build the environment.

- `mode`. You can configure rattan to function in `Compatible` mode or `Isolated` mode. The `ns-left` side is always a new network namespace. The difference of the two modes lies in whether `ns-right` side is the host network namespace (the namespace run rattan) or a new network namespace. The default is `Compatible`.
- `left_veth_count` and `right_veth_count`. You can configure the number of veth pairs to create in the `ns-left` (i.e., between `ns-left` and `ns-rattan`) and `ns-right` (i.e., between `ns-right` and `ns-rattan`) network namespaces. The default is 1.

### HTTP Section

This section configures Rattan's HTTP control server.

- `enable`. You can enable or disable the HTTP control server. The default is `false`.
- `port`. You can configure the port number of the HTTP control server. The default is 8086.

### Cells Section

This section describes the emulation cells. It is a table of cells, where each cell has a unique string ID, defined like `[cells.<ID>]`.

Each cell **MUST** have a "type" field, which determines the cell type and cell configuration fields.
Possible cell types:

- `Bw`: Fixed bandwidth
- `BwReplay`: Bandwidth trace replayer
- `Delay`: Fixed delay
- `DelayReplay`: Delay trace replayer
- `DelayPerPacket`: Delay specified per-packet
- `Loss`: Loss pattern
- `LossReplay`: Loss trace replayer
- `Spy`: Log all packets
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

We should state that we sync the start time of all these cell types to the earliest timestamp. The earliest timestamp is conditionally defined. It is the `FIRST_PACKET` time by default, but can be switched to the RATTAN start-up time.

The `FIRST_PACKET` is defined as the first L2 unicast packet to enter Rattan. An L2 unicast packet has a **destination** MAC address with the 8th bit (the Individual/Group bit) set to `0`.

For example, `38:7e:58:e7:02:01` is a unicast MAC address because the 8th bit (the least-significant bit of the first byte, `0x38`) is `0`. In contrast, MAC addresses like `01:00:5e:00:00:fb` or `33:33:00:00:02` are **not** unicast.

**Examples of a `FIRST_PACKET`:**

- A TCP SYN packet from the client.
- An ICMP Echo Request (Ping).

**Packets that cannot be a `FIRST_PACKET` (often generated by the Linux network stack):**

- An ICMPv6 Router Solicitation.
- An mDNS (Multicast Domain Name System) query.

This design ensures that the trace does not begin replaying until actual application traffic has started.

### Links Section

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

### Resource Section

This section describes how rattan use system resource

The section is preserved for future use but not used now.

- `cores`. The CPU cores that rattan is allowed to use.

### Commands Section

This section describes the commands to run in rattan.

When in `Compatible` mode, only the `left` and `shell` options are used,
and the commands will run in the `ns-left` network namespace.

When in `Isolated` mode, only the left and right options are used,
and the commands will run in the `ns-left` and `ns-right` network namespace, respectively.

- `left` and `right`. The commands to run in the `ns-left` and `ns-right` network namespace, respectively. Should be an array of strings.
- `shell`. The shell used to run if no command is specified. Only used when in compatible mode. The default is `Default` (i.e., the default shell defined in environment variable `$SHELL`).
