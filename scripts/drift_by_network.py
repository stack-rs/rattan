#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "tdigest>=0.5.2",
# ]
# ///

# The detailed spec of a TCP Log Entry:  (in compact-tcp mode)
#  0                   1                   2                   3
#  0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
# +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
# |       LH.length       | LH.ty.|   GPH.length  |GPH.ac.|GPH.ty.|
# +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
# |                          GP.timestamp                         |
# +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
# |           GP.length           |       PRH.length      |PRH.ty.|
# +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
# |                          tcp.flow_id                          |
# +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
# |                            tcp.seq                            |
# +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
# |                            tcp.ack                            |
# +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
# |             ip.id             |            ip.frag            |
# +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
# |          ip.checksum          |   tcp.flags   |  tcp.dataofs  |
# +-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+

# the ip.frag field is used to store the time drift in microseconds
from pathlib import Path
import sys
import os
import re
import json

from tdigest import TDigest

uuid_re = re.compile(
    r"[0-9a-fA-F]{8}-"
    r"[0-9a-fA-F]{4}-"
    r"[0-9a-fA-F]{4}-"
    r"[0-9a-fA-F]{4}-"
    r"[0-9a-fA-F]{12}"
)

def iter_uuid_dirs(root: Path):
    for dirpath, dirnames, _ in os.walk(root):
        for d in dirnames:
            if uuid_re.fullmatch(d):
                yield os.path.join(dirpath, d)


global_stats = {}

def record_stat(stats, path):
    global global_stats
    
    FIELDS_TO_STAT = ["send", "drift", "recv"]
    
    pattern = r"bw(\d+)_delay(\d+)_buf(\d+)_loss([\d.]+)"
    m = re.search(pattern, path)
    
    if m:
        network = m.groups()
        network = "_".join(network)
        print(network)
    else:
        print(f"WARNING: could not parse network from path: {path}")
        return
        
    if network not in global_stats:
        global_stats[network] = {}
        for stat_name in FIELDS_TO_STAT:
            global_stats[network][stat_name] = TDigest()
    
    for stat_name in FIELDS_TO_STAT:
        stat : TDigest = stats[stat_name]
        global_stats[network][stat_name] += stat
        print(stat_name, global_stats[network][stat_name].n)
    



def parse_field(x: str):
    if x.startswith("*"):
        return None
    try:
        return int(x)
    except Exception as _:
        return x

def load_line(line: str):
    FIELDS = ("direction", "ingress_time", "send", "delay", "drift", "recv")
    return {
            field: value
            for field, value in zip(FIELDS, map(parse_field, line.strip().split()))
            if value is not None
        }
        

def main(path: Path):
    for artifact_dir in iter_uuid_dirs(path):
        print(artifact_dir)
        log_file = os.path.join(artifact_dir, "drift.log")
        
        FIELDS_TO_STAT = ["send", "drift", "recv"]
        stat = {}
        for field in FIELDS_TO_STAT:
            stat[field] = TDigest()
        
        try:
            with open(log_file, "r") as f:
                for line in f:
                    line = load_line(line)
                    for field in FIELDS_TO_STAT:
                        if field in line:
                            stat[field].update(line[field])
        except FileNotFoundError:
            print(f"{log_file} not found")
            continue
    
        try:
            result_file = os.path.join(artifact_dir, "./result/result.json")
            with open(result_file, "r") as f:
                app_result = json.load(f)
                stat["result"] = app_result 
        except Exception as e:
            print("Failed to get app result for", artifact_dir, e)
    
        record_stat(stat, artifact_dir)

        # break

if __name__ == "__main__":
    
    # folder = "/home/lethe/data/202603_rattan_time_drift/result_260315_static/artifacts/bw524288_delay25_buf400000_loss0.001/"
    folder = sys.argv[1]
    main(Path(folder))
    
    for network, stats in global_stats.items():
        for stat_name, stat in stats.items():
            print(network, stat_name, stat.n)
            stats[stat_name] = stat.to_dict()
            
    json.dump(global_stats, open("global_stats.json", "w"))
    print("Saved global_stats.json")
