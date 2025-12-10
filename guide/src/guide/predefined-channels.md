# Predefined Cells and Templated Channels

The templated channels consist of a set of cells defined in the `rattan-core` library, which you can use to quickly set up a network path for testing.
You can use these channels through the `rattan link` subcommand with just a few command line arguments.

For now, we only provide a simple bidirectional channel, with each direction comprising three cells: `bandwidth`, `delay` and `loss`.
This channel is suitable for most common network emulation scenarios.

You can configure the parameters of each cell by passing the arguments to the subcommand.

## Usage

For example, to create a channel with 10Mbps bandwidth, infinite queue, 50ms latency and 1% loss rate in both directions, you can run the following command:

```bash
rattan link --uplink-bandwidth 10Mbps --uplink-delay 50ms --uplink-loss 0.1 \
--downlink-bandwidth 10Mbps --downlink-delay 50ms --downlink-loss 0.1
```

Then you will be prompted to a shell with the network path set up.

### An Illustration Example of Testing

We are going to illustrate a simple example of testing network performance using `rattan link` and `iperf3`.
Before testing, don't remember to disable your firewall to allow `iperf3` traffic, or allow from `10.0.0.0/8`,
as Rattan is using that as the default IP addresses for the emulated network.

As the `rattan link` subcommand runs in the `Compatible Mode` (which is similar to [mahimahi](https://github.com/ravinet/mahimahi)),
we should fist launch an `iperf3` server in one terminal on the host (outside of rattan):

```bash
iperf3 -s -1
```

After that, we can launch the `rattan link` command in another terminal to set up the network path:

```bash
rattan link --uplink-bandwidth 12Mbps --uplink-delay 50ms --downlink-bandwidth 12Mbps --downlink-delay 50ms
```

In this way, we create a link with 12Mbps bandwidth, 100ms RTT and no loss.

Now it will prompt you to a shell. If you are using bash or specify the shell to use when invoking rattan with `rattan link -s bash`,
you will get the following prompt in your terminal indicating that you are now in the rattan environment:

```bash
[rattan] user@hostname:
```

Similar to mahimahi, we have an environment variable `RATTAN_BASE` containing the IP address of the other end of the link.

So in the shell, we can just run the following iperf3 client command to test the network performance:

```bash
iperf3 -c $RATTAN_BASE -t 10
```

### Available Options

For detailed usage, you can run `rattan link --help` to see the available cells and options:

```txt
Run a templated channel with command line arguments

Usage: rattan link [OPTIONS] [-- <COMMAND>...]

Arguments:
  [COMMAND]...
          Command to run. Only used when in compatible mode

Options:
      --uplink-loss <Loss>
          Uplink packet loss

      --downlink-loss <Loss>
          Downlink packet loss

      --generate
          Generate config file instead of running a instance

          If this flag is set, the program will only generate the config to stdout and exit.

      --uplink-delay <Delay>
          Uplink delay

      --downlink-delay <Delay>
          Downlink delay

      --left-stdout
          Used in isolated mode only. If set, stdout of left is passed to output of this program

      --right-stdout
          Used in isolated mode only. If set, stdout of rihgt is passed to output of this program

      --uplink-bandwidth <Bandwidth>
          Uplink bandwidth

      --left-stderr
          Used in isolated mode only. If set, stderr of left is passed to output of this program

      --uplink-trace <File>
          Uplink trace file

      --right-stderr
          Used in isolated mode only. If set, stderr of right is passed to output of this program

      --uplink-queue <Queue Type>
          Uplink queue type

          [possible values: infinite, droptail, drophead, codel]

      --generate-path <File>
          Generate config file to the specified path instead of stdout

      --uplink-queue-args <JSON>
          Uplink queue arguments

      --downlink-bandwidth <Bandwidth>
          Downlink bandwidth

      --packet-log <File>
          The file to store compressed packet log (overwrite config) (default: None)

      --downlink-trace <File>
          Downlink trace file

      --file-log
          Enable logging to file

      --downlink-queue <Queue Type>
          Downlink queue type

          [possible values: infinite, droptail, drophead, codel]

      --file-log-path <File>
          File log path, default to $CACHE_DIR/rattan/core.log

      --downlink-queue-args <JSON>
          Downlink queue arguments

      --no-nat
          Disable NAT in compatible mode

  -s, --shell <SHELL>
          Shell used to run if no command is specified. Only used when in compatible mode

          [default: default]
          [possible values: default, sh, bash, zsh, fish]

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```
