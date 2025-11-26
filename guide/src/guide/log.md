# Rattan Packet Log

This specification defines the packet log file of rattan `.rtl` file, of which this crate gives an example for encoding 
and decoding.

Last updated on: 2025-11-24

## Log mode

There are currently 3 support packet-log modes.

``` rust
pub enum PacketLogMode {
    CompactTCP,
    RawIP,
    RawTCP,
}
```

### Compact
For the `Compact` mode, like `CompactTCP`, a subset of fields in the headers of packets (of the designated protocol, TCP in this case) will be recorded to form a compact representation (`Packet Log Entries`).
In such mode, (i.e., when `--packet-log` flag is set), Rattan will generate 2 files during runtime:

1. `xxxxx.flow`, which contains flow level metadata info.
2. `xxxxx.oldrtl`, which contains the `Packet Log Entries`.  Each entry here contains some fields from header, along with some packet-level meta data (e.g. timestamp, length) and is related to a certain flow recorded as above.

We provide `rattan convert` subcommand to use above log info to (partially) recover a `.pcapng` file that can be read by standard tools such as wireshark.

### Raw

In such modes, the raw headers (L4 for `RawTCP` and L3 + L4 for `RawIP`) of the packets of the designated protocol is recorded, and Rattan generates 3 log files during runtime.

1. `xxxxx.flow`.  Same as above.
2. `xxxxx.raw`. A un-structed file, that contains the raw, unaligned packet headers continously.
3. `xxxxx.rtl`. which contains the `Raw Packet Log Entries`. The different between these entries and the above describled counterparts (those in `oldrtl`) is that, they use a "fat pointer" (i.e., start offset and size) to the `.raw` file, pointing at the raw recorded header of the packet that is related to the entry.

The `rattan convert` subcommand is also appliable for raw entries. Given that more information of packets are recorded, it is expected that the recovered `.pcapng` file from raw logs
contains more information (such as the IP and TCP options for each packet) than that recovered from compact logs. Unevitable, enabling raw log comes with heaviour run time overhead and should be 
avoided under high-throughput scenerios.

**Note:** 
We consider `.oldrtl` and `.rtl` files as the main part of the rattan log describled here, and when using CLI tools provided by Rattan to generate or deal with rattan log, only the 
path to the `.oldrtl` or `.rtl` file is used. And Rattan expects that the other files reside in the same folder, and have different extension names as listed above. 
