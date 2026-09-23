#!/usr/bin/env python3

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

from mininet.link import TCLink  # ty: ignore[unresolved-import]
from mininet.log import setLogLevel  # ty: ignore[unresolved-import]
from mininet.net import Mininet  # ty: ignore[unresolved-import]
from mininet.node import OVSSwitch  # ty: ignore[unresolved-import]
from mininet.topo import Topo  # ty: ignore[unresolved-import]

LIB_DIR = Path(__file__).resolve().parent

BYTES_PER_PACKET = 1500

PASSED_THROUGH = [
    "CLIENT_CORES",
    "SERVER_CORES",
    "CLIENT_BARRIER",
    "SERVER_BARRIER",
    "CCA",
    "DURATION",
]


def link_params(side: dict) -> dict:
    params: dict = {}
    if "bw_Mbps" in side:
        params["bw"] = side["bw_Mbps"]
    if "delay_ms" in side:
        params["delay"] = f"{side['delay_ms']}ms"
    if "loss" in side:
        params["loss"] = side["loss"]
    if "max_queue_pkts" in side:
        params["max_queue_size"] = int(side["max_queue_pkts"])
    elif "max_queue_bytes" in side:
        params["max_queue_size"] = -(-int(side["max_queue_bytes"]) // BYTES_PER_PACKET)
    return params


class ChainTopo(Topo):
    def build(self, links: list[dict]) -> None:  # type: ignore[override]
        self.addHost("h1")
        self.addHost("h2")

        names = {link["from"] for link in links} | {link["to"] for link in links}
        for name in sorted(names - {"h1", "h2"}):
            self.addSwitch(name)

        for link in links:
            self.addLink(
                link["from"],
                link["to"],
                cls=TCLink,
                params1=link_params(link["uplink"]),
                params2=link_params(link["downlink"]),
            )


def shell_command(env: dict[str, str], argv: list[str]) -> str:
    parts = ["env"] + [f"{k}={v}" for k, v in env.items()] + argv
    return " ".join(parts)


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: mininet-topology.py <scenario.json>", file=sys.stderr)
        return 1
    config = json.loads(Path(sys.argv[1]).read_text())

    setLogLevel("warning")
    net = Mininet(
        topo=ChainTopo(config["links"]),
        controller=None,
        switch=OVSSwitch,
        link=TCLink,
    )
    net.start()
    for switch in net.switches:
        subprocess.run(
            ["ovs-vsctl", "set-fail-mode", str(switch), "standalone"], check=True
        )

    h1, h2 = net.get("h1"), net.get("h2")

    for host, iface in ((h1, "h1-eth0"), (h2, "h2-eth0")):
        host.cmd(f"ethtool -K {iface} tso off gso off gro off")

    passed = {k: os.environ[k] for k in PASSED_THROUGH if k in os.environ}
    log_file = os.environ.get("LOG_FILE")

    server_env = dict(passed, MININET_BASE=h1.IP(), NO_WAIT="1")
    client_env = dict(passed, MININET_BASE=h2.IP())

    h2.cmd(shell_command(server_env, [str(LIB_DIR / "iperf3-server.sh")]) + " &")
    client = [str(LIB_DIR / "iperf3-client.sh")]
    if log_file:
        client.append(log_file)
    print(h1.cmd(shell_command(client_env, client)))

    net.stop()
    return 0


if __name__ == "__main__":
    sys.exit(main())
