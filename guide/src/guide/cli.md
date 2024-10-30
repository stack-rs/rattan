# Bundled CLI Tool

The Rattan project provides a CLI tool named `rattan` for common use cases.
The tool is built on top of the `rattan-core` library, which you can use to build your own tools.

We provide different features under different subcommands:

- [rattan link](predefined-channels.md):
Run a templated channel with command line arguments.
We include a set of predefined cells and templated channels in the tool to help you get started quickly.
You can emulate a simple network path with just a few commands.
- [rattan run](flexible-configuration.md):
Run the instance according to the configuration.
For more complex configurations or reproduction purpose, you can use the flexible configuration file to define your own cells, channels and even routes.

There are also global options that you can use to customize the behavior of the tool:

- `--generate` and `--generate-path` are used to generate a configuration file from your settings instead of running an instance.
- `--packet-log` is used to enable compressed packet log and specify the file to store the records.
- `--file-log` and `--file-log-path` are used to enable logging to a file and specify the path to store the log.

For more detailed usage, you can run `rattan --help` to see the available commands and options:

```txt
Usage: rattan [OPTIONS] <COMMAND>

Commands:
  link  Run a templated channel with command line arguments
  run   Run the instance according to the config
  help  Print this message or the help of the given subcommand(s)

Options:
      --generate              Generate config file instead of running a instance
      --generate-path <File>  Generate config file to the specified path instead of stdout
      --packet-log <File>     The file to store compressed packet log (overwrite config) (default: None)
      --file-log              Enable logging to file
      --file-log-path <File>  File log path, default to $CACHE_DIR/rattan/core.log
  -h, --help                  Print help (see more with '--help')
  -V, --version               Print version
```
