# Predefined Cells and Templated Channels

The templated channels consist of a set of cells defined in the `rattan-core` library, which you can use to quickly set up a network path for testing.
You can use these channels through the `rattan link` subcommand with just a few command line arguments.

For now, we only provide a simple bidirectional channel, with each direction comprising three cells: `bandwidth`, `delay` and `loss`.

You can configure the parameters of each cell by passing the arguments to the subcommand.

For example, to create a channel with 10Mbps bandwidth, infinite queue, 50ms latency and 1% loss rate in both directions, you can run the following command:

```bash
rattan link uplink-bandwidth 10Mbps uplink-delay 50ms uplink-loss 0.1 downlink-bandwidth 10Mbps downlink-delay 50ms downlink-loss 0.1
```

Then you will be prompted to a shell with the network path set up.

For detailed usage, you can run `rattan link --help` to see the available cells and options:

```txt
Run a templated channel with command line arguments

Usage: rattan link [OPTIONS] [-- <COMMAND>...]

Arguments:
  [COMMAND]...  Command to run. Only used when in compatible mode

Options:
      --uplink-loss <Loss>              Uplink packet loss
      --downlink-loss <Loss>            Downlink packet loss
      --generate                        Generate config file instead of running a instance
      --uplink-delay <Delay>            Uplink delay
      --downlink-delay <Delay>          Downlink delay
      --generate-path <File>            Generate config file to the specified path instead of stdout
      --packet-log <File>               The file to store compressed packet log (overwrite config) (default: None)
      --uplink-bandwidth <Bandwidth>    Uplink bandwidth
      --file-log                        Enable logging to file
      --uplink-trace <File>             Uplink trace file
      --file-log-path <File>            File log path, default to $CACHE_DIR/rattan/core.log
      --uplink-queue <Queue Type>       Uplink queue type [possible values: infinite, droptail, drophead, codel]
      --uplink-queue-args <JSON>        Uplink queue arguments
      --downlink-bandwidth <Bandwidth>  Downlink bandwidth
      --downlink-trace <File>           Downlink trace file
      --downlink-queue <Queue Type>     Downlink queue type [possible values: infinite, droptail, drophead, codel]
      --downlink-queue-args <JSON>      Downlink queue arguments
  -s, --shell <SHELL>                   Shell used to run if no command is specified. Only used when in compatible mode [default: default] [possible values: default, sh, bash, zsh, fish]
  -h, --help                            Print help (see more with '--help')
  -V, --version                         Print version
```
