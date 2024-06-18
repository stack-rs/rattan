FROM ubuntu:22.04

RUN apt-get update && apt-get install -y iptables && rm -rf /var/lib/apt/lists/*

COPY ./target/release/rattan /usr/local/bin/rattan

# ENV PACKET_DUMP_FILE=/tmp/packet_dump.pcap

ENTRYPOINT ["/usr/local/bin/rattan", "--docker", "-d", "20ms"]
