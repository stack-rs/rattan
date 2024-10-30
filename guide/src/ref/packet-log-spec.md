# Packet Log Spec

```text
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|       LH.length       | LH.ty.|   GPH.length  |GPH.ac.|GPH.ty.|
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                          GP.timestamp                         |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|           GP.length           |       PRH.length      |PRH.ty.|
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                          PRT (custom)                         |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

Generated with [protocol](https://github.com/luismartingarcia/protocol), where:

* `LH` is short for log entry header
* `GPH` is short for general packet entry header
* `GP` is short for general packet entry (the body part)
* `PRH` is short for protocol entry header
* `PRT` is short for protocol entry (the body part)
* `ty.` is short for type
* `ac.` is short for action

```bash
protocol "LH.length:12,LH.ty.:4,GPH.length:8,GPH.ac.:4,GPH.ty.:4,GP.timestamp:32,GP.length:16,PRH.length:12,PRH.ty.:4,PRT (custom):32"
```

We currently provide TCP log spec (a variant of the protocol entry body part, i.e. `PRT (custom)`):

```text
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                          tcp.flow_id                          |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                            tcp.seq                            |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                            tcp.ack                            |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|             ip.id             |         ip.frag_offset        |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|          ip.checksum          |   tcp.flags   |    padding    |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+

```

```bash
protocol "tcp.flow_id:32,tcp.seq:32,tcp.ack:32,ip.id:16,ip.frag_offset:16,ip.checksum:16,tcp.flags:8,padding:8"
```
