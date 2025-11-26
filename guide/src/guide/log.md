# Rattan Packet Log

This specification defines the packet log file of rattan `.rtl` file, of which this crate gives an example for encoding 
and decoding.

Last updated on: 2025-11-24




## File

The `.rtl` file is composed of `chunk`s.

`chunk`' s size shoule be a multiple of 4096B, and is 4096B-aligned.

The chunk start with a `chunk_prologue`, whose detailed specification is listed as follows:

```
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
 +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
 |       LH.length       | LH.ty.|      CH.length        | CH.ty.|
 +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
 |                     log    version                            |
 +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
 |                         data  length                          |
 +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
 |                        chunk  length                          |
 +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
 |                   offset (lower 32bits)                       |
 +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
 |                   offset (upper 32bits)                       |
 +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
 |                          reserved                             |
 +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
 |                          reserved                             |
 +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

The current log version (defined in this file) is `0x20251120`.
`data length` :  the total length of other parts in this chunk in Bytes
`chunk length`:  Suppose this chunk starts at offset `x`, the next chunk,
if it exists, starts at offset `x` + `chunk length`.


And there are 2 defined chunks:

- Metadata chunk
  -  `CH.ty` = 0
  -  `offset` is 0, if this is last Metadata chunk. or the offset of next Metadata chunk.
- Log Entry chunk
  -  `CH.ty` = 1
  -  `offset` is used for Raw log entries, which will be describled there.
  
The log file should start with a Metadata chunk.
And all the Log Entry chunks should be placed continously.


### Metadata chunk

The metadata chunks contains all the infomation needed, to process any single log entry chunk.

Now, it contains all the
