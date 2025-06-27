#!/usr/bin/env python3
import struct
import sys
import argparse
from datetime import datetime

def create_pcapng_header():
    # Section Header Block (SHB)
    shb = bytes.fromhex(
        "0a0d0d0a"  # Magic
        "1c000000"  # Block length (28)
        "4d3c2b1a"  # Byte-order magic
        "0100"      # Major version
        "0000"      # Minor version
        "ffffffffffffffff"  # Section length (unspecified)
        "1c000000"  # Block length (28)
    )
    
    # Interface Description Block (IDB)
    idb = bytes.fromhex(
        "01000000"  # Block type
        "14000000"  # Block length (20)
        "0100"      # LinkType (Ethernet)
        "0000"      # Reserved
        "0000ffff"  # SnapLen (65535)
        "14000000"  # Block length (20)
    )
    
    return shb + idb

def parse_log_timestamp(raw_bytes):
    timestamp = struct.unpack(">I", raw_bytes)[0]
    
    ONE_YEAR_IN_US = 365 * 24 * 3600 * 1_000_000
    if timestamp > ONE_YEAR_IN_US:
        return timestamp % ONE_YEAR_IN_US
    
    return timestamp & 0x7FFFFFFF

def create_epb_block(timestamp_us, orig_len, first_ts=None):
    rel_time = timestamp_us if first_ts is None else timestamp_us - first_ts
    rel_time = max(0, rel_time)  

    ts_sec, ts_nsec = divmod(rel_time, 1_000_000)
    ts_nsec *= 1_000

    block = bytearray()
    block.extend(struct.pack("<I", 0x00000006))  # Block type (EPB)
    block.extend(struct.pack("<I", 32))          # Initial block length
    block.extend(struct.pack("<I", 0))           # Interface ID
    block.extend(struct.pack("<I", ts_sec))      # Timestamp seconds
    block.extend(struct.pack("<I", ts_nsec))     # Timestamp nanoseconds
    block.extend(struct.pack("<I", 0))           # Captured length
    block.extend(struct.pack("<I", orig_len))    # Original length
    block.extend(struct.pack("<I", 32))          # Final block length
    
    return bytes(block)

def convert_log_to_pcapng(input_path, output_path):
    with open(input_path, "rb") as f_in, open(output_path, "wb") as f_out:
        f_out.write(create_pcapng_header())
        
        packet_count = 0
        first_timestamp = None
        
        while True:
            header = f_in.read(36)
            if len(header) < 36:
                break
            
            try:
                timestamp = parse_log_timestamp(header[4:8]) 
                orig_len = struct.unpack(">H", header[10:12])[0] 

                if first_timestamp is None:
                    first_timestamp = timestamp

                epb = create_epb_block(timestamp, orig_len, first_timestamp)
                f_out.write(epb)
                packet_count += 1
                
            except Exception as e:
                continue

def main():
    parser = argparse.ArgumentParser(description='Convert log file to pcapng format')
    parser.add_argument('input', help='Input log file path')
    parser.add_argument('output', help='Output pcapng file path')
    
    args = parser.parse_args()
    
    convert_log_to_pcapng(args.input, args.output)

if __name__ == "__main__":
    main()