# Run Server inside Rattan to Serve Public Traffic

You can find the complete code to run this example in the [examples/public-server](https://github.com/stack-rs/rattan/tree/main/examples/public-server) folder.

## Configure the Network Conditions of the Emulated Internet Path

So first, we need to create a TOML file to describe the Internet path we want to emulate.
In our current example, as we need to run a public server, we will use the `Compatible` mode of Rattan and set a typical Internet path configuration: one uplink and one downlink, each with 20 ms delay, and no packet loss. The difference is in bandwidth: the uplink is 100 Mbps with infinite queue, and the downlink is a token bucket with rate limited to 2Mbps.

For the full configuration file, we refer the readers to [examples/public-server/rattan.toml](https://github.com/stack-rs/rattan/tree/main/examples/public-server/rattan.toml)

## Write Scripts to Exchange Information for IP Forwarding

To present more clearly how to run a server inside Rattan and allow it to serve public traffic,
we have placed different functionalities in three separate scripts: [main.sh](https://github.com/stack-rs/rattan/tree/main/examples/public-server/main.sh), [inner.sh](https://github.com/stack-rs/rattan/tree/main/examples/public-server/inner.sh), and [run.sh](https://github.com/stack-rs/rattan/tree/main/examples/public-server/run.sh).
Users only need to write the command to start the server in [run.sh](https://github.com/stack-rs/rattan/tree/main/examples/public-server/run.sh) and then directly execute the [main.sh](https://github.com/stack-rs/rattan/tree/main/examples/public-server/main.sh) script.

To specify the outer port on the host and the inner port of the server inside Rattan, we can run the script with arguments:

```bash
bash main.sh --inner-port 5201 --outer-port 35201
```

For the rest of this section, we will explain how these scripts work and how they set up the IP forwarding rules on the host machine.

As an example, we serve `iperf3` server inside Rattan, which is in the [run.sh](https://github.com/stack-rs/rattan/tree/main/examples/public-server/run.sh) script.
You can also modify this script to run your own server.

What inside the script is very simple: `iperf3 -s -1`

Now we have to setup the IP forwarding rules to forward traffic from a public port on the host machine to the inner port inside Rattan's network namespace.

We have created a pair of scripts, [main.sh](https://github.com/stack-rs/rattan/tree/main/examples/public-server/main.sh) and [inner.sh](https://github.com/stack-rs/rattan/tree/main/examples/public-server/inner.sh),
to get the internal IP of Rattan so to set forwarding rules on the host machine.

We use temporary files named with random UUIDs as a way to pass messages, created by the [main.sh](https://github.com/stack-rs/rattan/tree/main/examples/public-server/main.sh), which polls the content to obtain the IP reported from [inner.sh](https://github.com/stack-rs/rattan/tree/main/examples/public-server/inner.sh). Then we use the following command to create forwarding rules:

```bash
sudo iptables -t nat -A PREROUTING -p tcp --dport $OUTER_PORT -j DNAT --to-destination "$INNER_IP:$INNER_PORT"
```

This will forward the traffic from 0.0.0.0:$OUTER_PORT to $INNER_IP:$INNER_PORT

Our scripts will clean the forwarding rules on exit and you can also delete the rule manually with the following command:

```bash
sudo iptables -t nat -D PREROUTING -p tcp --dport $OUTER_PORT -j DNAT --to-destination "$INNER_IP:$INNER_PORT"
```
