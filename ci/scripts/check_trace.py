#!/usr/bin/env python3
"""Check iperf3 throughput against the capacity of a Mahimahi trace.

Each line of a Mahimahi trace is a millisecond timestamp with one chance to
send a 1500 byte packet, and the trace loops when it ends.

Usage: check_trace.py TRACE IPERF_JSON MIN_UTIL MAX_UTIL
Exits with 1 if the utilization is outside [MIN_UTIL, MAX_UTIL].
"""
import json
import sys

PACKET_BITS = 1500 * 8


def trace_capacity_bits(path, seconds):
    with open(path) as f:
        stamps = [int(line) for line in f if line.strip()]
    period = stamps[-1]
    end_ms = seconds * 1000
    total = 0
    offset = 0
    while offset < end_ms:
        for t in stamps:
            if offset + t >= end_ms:
                break
            total += 1
        offset += period
    return total * PACKET_BITS


def main():
    trace, iperf_json = sys.argv[1], sys.argv[2]
    min_util, max_util = float(sys.argv[3]), float(sys.argv[4])

    try:
        with open(iperf_json) as f:
            data = json.load(f)
    except (OSError, ValueError) as e:
        print(f"FAIL: no usable iperf3 output ({e})")
        sys.exit(1)
    if "error" in data:
        print(f"FAIL: iperf3 error: {data['error']}")
        sys.exit(1)

    intervals = data.get("intervals", [])
    if not intervals:
        print("FAIL: iperf3 reported no intervals")
        sys.exit(1)
    seconds = len(intervals)
    got = sum(iv["sum"]["bytes"] * 8 for iv in intervals)
    cap = trace_capacity_bits(trace, seconds)
    if cap == 0:
        print("FAIL: trace has no capacity in the test window")
        sys.exit(1)

    util = got / cap
    ok = min_util <= util <= max_util
    print(
        f"{'PASS' if ok else 'FAIL'}: capacity {cap / seconds / 1e6:.2f} Mbps, "
        f"iperf3 {got / seconds / 1e6:.2f} Mbps, utilization {util:.1%} "
        f"(allowed {min_util:.0%}-{max_util:.0%}, {seconds} s)"
    )
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
