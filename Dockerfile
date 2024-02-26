FROM ubuntu:22.04

RUN apt-get update && apt-get install -y iptables && rm -rf /var/lib/apt/lists/*

COPY ./target/release/rattan-cli /usr/local/bin/rattan-cli

# ENV PACKET_DUMP_FILE=/tmp/packet_dump.pcap

ENTRYPOINT ["/usr/local/bin/rattan-cli", "--docker", "-d", "20ms"]
