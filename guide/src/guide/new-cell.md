# Write a New Cell

Cells are components in rattan that provides various settings and restrictions for your network paths. You can simulate different network environments by combining different cells in any configuration you like. We already offer built-in cells for you to choose such as `Delay` cell and `Loss` cell. However, if you require additional features or wish to create your own unique cells, you can follow this guide to write a custom cell yourself.

## Basic Structure

A cell in rattan is primarily composed of the following components: `Ingress`, `Egress`, and `ControlInterface`. To implement your own cell, you need to implement these three traits, along with an additional `Cell` trait to combine them together. In the following explanation, we will use a `DropPacketCell` as an example, which drops one packet every `loss_interval` packets, to illustrate the basic structure and implementation process of a cell.

### Ingress

We should implement an `Ingress` to receive packets, which are dequeued by other cells and forwarded by the rattan core. The `Ingress` serves as the entry for our cell, so it is indispensable.

Implementing the `Ingress` trait requires defining an `enqueue` method, which the rattan core calls to insert packets into the cell. In this `enqueue` method, we need to call the `send` method of the `ingress` member to send the packet through the channel (similarly, the `Egress` trait has a `dequeue` method, which calls its `egress` member — an `UnboundedReceiver` — to receive packets from the channel and decide whether to remove or discard them from the cell). Additionally, since we allow multiple cells to send packets to a single cell from different threads, `Ingress` must implement the `Clone` trait and can only have an immutable reference to itself. 

Although `Ingress` can only have one immutable reference to itself, it’s important to note that mutable operations on the packets are possible within the `enqueue` method of `Ingress`.

You can refer to the example below:

```rust
pub struct DropPacketCellIngress<P>
where
    P: Packet,
{
    ingress: mpsc::UnboundedSender<P>,          // Send packets to Egress
}

impl<P> Clone for DropPacketCellIngress<P>
where
    P: Packet,
{
    fn clone(&self) -> Self {
        Self {
            ingress: self.ingress.clone(),
        }
    }
}

impl<P> Ingress<P> for DropPacketCellIngress<P>
where
    P: Packet + Send,
{
    fn enqueue(&self, mut packet: P) -> Result<(), Error> {
        packet.set_timestamp(Instant::now());
        self.ingress
            .send(packet)
            .map_err(|_| Error::ChannelError("Data channel is closed.".to_string()))?;
        Ok(())
    }
}
```

### Egress

After implementing the `Ingress`, we should also implement an `Egress` to dequeue packets from our cell and send them to other cells through the rattan core. The `Egress` serves as the export for our cell.

The struct that implements the `Egress` trait typically has the following structure: an `UnboundedReceiver` as the `egress` member, a configuration receiver (the specific type depends on the way you implement trait `ControlInterface`), a `state: AtomicI32` to represent the state of the cell, where a state value of 0 means "drop", 1 means "pass-through", and 2 means "normal operation" (i.e., you only need to perform the cell’s functionality when the `state` is 2). Additionally, it includes your cell’s configuration parameters and its internal state.

An example is shown below:

```rust
pub struct DropPacketCellEgress<P>
where
    P: Packet,
{
    egress: mpsc::UnboundedReceiver<P>,                    // Receive packets from Ingress
    inner_loss_interval: Box<usize>,
    // Your own parameters...
    counter: usize,                                        // Count number of packets since last packet loss
    // Your own inner states...
    loss_interval: Arc<AtomicRawCell<usize>>,              // Configuration receiver
    state: AtomicI32,                                      // State of Egress
}
```

You must implement the `change_state` and `dequeue` methods for the `Egress` trait. The `change_state` will be called by rattan core to control the state of your cell. You need to implement the functional logic of your cell in the `dequeue` method. For example, if you need a delay function, you can write your `dequeue` method to return a packet after a specified delay time; if you need to drop packets, you can write your `dequeue` method to return a packet or drop it with a certain probability.

It is also important to note that any modification to the internal state of your cell must be implemented within the `dequeue` method, as it is the only place where a mutable reference to `Self` is available.

```rust
#[async_trait]
impl<P> Egress<P> for DropPacketCellEgress<P>
where
    P: Packet + Send + Sync,
{
    async fn dequeue(&mut self) -> Option<P> {
        // Receive packets from Ingress
        let packet = match self.egress.recv().await {
            Some(packet) => packet,
            None => return None,
        };
        // Check state of your cell
        match self.state.load(std::sync::atomic::Ordering::Acquire) {
            0 => {
                return None;
            }
            1 => {
                return Some(packet);
            }
            _ => {}
        }
        // Add control logic of your own cell
        self.counter += 1;
        if let Some(loss_interval) = self.loss_interval.swap_null() {
            self.inner_loss_interval = loss_interval;
        }
        if *self.inner_loss_interval == 0 {
            return Some(packet);
        }
        if self.counter >= self.inner_loss_interval {
            self.counter = 0;
            None
        } else {
            Some(packet)
        }
    }

    fn change_state(&self, state: i32) {
        self.state
            .store(state, std::sync::atomic::Ordering::Release);
    }
}
```

### ControlInterface

If you wish to modify the configuration of your cell at runtime, you need to implement the `ControlInterface` trait. It should include a member that can pass configuration information to `Egress` (in the example, this is an `Arc<AtomicRawCell>`, but you can also use other methods like an `UnboundedChannel` to transmit configuration information). The `ControlInterface` trait needs to implement a method `set_config`, where you should write the logic for modifying the cell's configuration at runtime through the `ControlInterface`. The implementation of `set_config` will vary depending on the way you pass configuration.

The following example is provided for reference:

```rust
pub struct DropPacketCellControlInterface {
    loss_interval: Arc<AtomicRawCell<usize>>,
}

impl ControlInterface for DropPacketCellControlInterface {
    type Config = DropPacketCellConfig;

    fn set_config(&self, config: Self::Config) -> Result<(), Error> {
        self.loss_interval.store(Box::new(config.loss_interval));
        Ok(())
    }
}
```

Note that the code above includes a `DropPacketCellConfig` struct, which is the configuration struct defined for the example cell. When implementing the `ControlInterface` trait, you also need to define a struct for your cell’s configuration. This struct typically contains the parameters that you want to set freely within the cell. The configuration struct you define here will also be used later when you implement TOML file parsing for your cell.

```rust
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Debug, Default, Clone)]
pub struct DropPacketCellConfig {
    pub loss_interval: usize,
}

impl DropPacketCellConfig {
    pub fn new<T: Into<usize>>(loss_interval: T) -> Self {
        Self {
            loss_interval: loss_interval.into(),
        }
    }
}
```

### Cell

Up until now, we have implemented the three necessary traits for creating a custom cell: `Ingress`, `Egress`, and `ControlInterface`, along with the struct for the cell's configuration information. However, these structs are just parts of the cell. In order to make them function properly in rattan, we need to assemble them into a single struct. This is why we need to implement the `Cell` trait.

The `Cell` trait has four methods that must be implemented: `sender`, `receiver`, `into_receiver`, and `control_interface`. The `sender` method returns a copy of the `Ingress`; the `receiver` method returns a mutable reference to the `Egress`; the `into_receiver` method returns the `Egress`; and the `control_interface` method returns an `Arc` of the `ControlInterface`.

Refer to the example below:

```rust
pub struct DropPacketCell<P: Packet> {
    ingress: Arc<DropPacketCellIngress<P>>,
    egress: DropPacketCellEgress<P>,
    control_interface: Arc<DropPacketCellControlInterface>,
}

impl<P> Cell<P> for DropPacketCell<P>
where
    P: Packet + Send + Sync + 'static,
{
    type IngressType = DropPacketCellIngress<P>;
    type EgressType = DropPacketCellEgress<P>;
    type ControlInterfaceType = DropPacketCellControlInterface;

    fn sender(&self) -> Arc<Self::IngressType> {
        self.ingress.clone()
    }

    fn receiver(&mut self) -> &mut Self::EgressType {
        &mut self.egress
    }

    fn into_receiver(self) -> Self::EgressType {
        self.egress
    }

    fn control_interface(&self) -> Arc<Self::ControlInterfaceType> {
        Arc::clone(&self.control_interface)
    }
}

impl<P> DropPacketCell<P>
where
    P: Packet,
{
    pub fn new<L: Into<usize>>(loss_interval: L) -> Result<DropPacketCell<P>, Error> {
        let loss_interval = loss_interval.into();
        let (rx, tx) = mpsc::unbounded_channel();
        let loss_interval = Arc::new(AtomicRawCell::new(Box::new(loss_interval)));
        Ok(DropPacketCell {
            ingress: Arc::new(DropPacketCellIngress { ingress: rx }),
            egress: DropPacketCellEgress {
                egress: tx,
                loss_interval: Arc::clone(&loss_interval),
                inner_loss_interval: Box::default(),
                state: AtomicI32::new(0),
                counter: 0,
            },
            control_interface: Arc::new(DropPacketCellControlInterface { loss_interval }),
        })
    }
}
```

## Add Configuration to Your New Cell

In the guide above, we’ve created a new cell. If you would like to configure your custom cell using a TOML file, you can follow the steps below.

First, you need to add the code to parse your configuration file in `rattan-core/src/config`. Specifically, you need to implement the `into_factory` method for the struct you previously defined for configuration. This method should return an anonymous type that implements the `CellFactory` trait. The `CellFactory` will accept a `handle` at runtime, and will be used by rattan radix to generate the configuration struct based on the data in the `handle`, and then instantiate a `Cell` using that configuration.

Look at the example below:

```rust
pub type DropPacketCellBuildConfig = drop_packet::DropPacketCellConfig;

impl DropPacketCellBuildConfig {
    pub fn into_factory<P: Packet>(self) -> impl CellFactory<drop_packet::DropPacketCell<P>> {
        move |handle| {
            let _guard = handle.enter();
            drop_packet::DropPacketCell::new(self.loss_interval)
        }
    }
}
```

Next, you need to add your cell to `CellBuildConfig` (located in `rattan-core/src/config/mod.rs`) so that rattan radix can recognize your cell. Similarly, in the `load_cells_config` method of `RattanRadix`, you also need to add the code to build your cell.

```rust
pub enum CellBuildConfig<P: Packet> {
    Bw(BwCellBuildConfig<P>),
    BwReplay(BwReplayCellBuildConfig<P>),
    Delay(DelayCellBuildConfig),
    DelayReplay(DelayReplayCellBuildConfig),
    Loss(LossCellBuildConfig),
    LossReplay(LossReplayCellBuildConfig),
    Shadow(ShadowCellBuildConfig),
    Router(RouterCellBuildConfig),
    DropPacket(DropPacketCellBuildConfig),
    Custom,
}

CellBuildConfig::DropPacket(config) => {
    self.build_cell(id, config.into_factory())?;
}
```

Now, you can write a TOML file for your cell and use rattan to try parsing it! There is an example for you to refer to to write your TOML file: 

```toml
# ----- General Section -----
[general]
# ----- General Section End -----


# ----- Environment Section -----
[env]
mode = "Compatible"
left_veth_count = 1
# ----- Environment Section End -----


# ----- HTTP Section -----
[http]
enable = false
port = 8086
# ----- HTTP Section End -----


# ----- Cells Section -----
# Shadow Cell Example
[cells.shadow]
type = "Shadow"

# DropPacket Cell Example
[cells.drop]
type = "DropPackets"

loss_interval = 2
# ----- Cells Section End -----


# ----- Links Section -----
[links]
left = "drop"
drop = "right"
right = "shadow"
shadow = "left"
# ----- Links Section End -----

# --- Resource Section -----
[resource]
cores = [1]
# ----- Resource Section End -----

# ----- Commands Section -----
[commands]
left = ["echo", "hello world"]
shell = "Default"
```

## Test Your New Cell

To check the correctness of your cell implementation, you can write unit tests for your new cell and run them in rattan using `nextest`. If you wish to integrate the `Cell` into the rattan official repository and the corresponding CLI tool, you can also follow this guide to write integration tests for them.

### Unit Tests

The goal of unit tests is to check whether the internal state of your cell transitions correctly and whether its functionality behaves as expected. Typically, when writing unit tests, you create an instance of your cell, manually call its `enqueue` and `dequeue` methods, and observe the results to check its correctness.

To run the unit tests, you can first run `cargo nextest list --workspace` to view the list of unit tests, find the test(s) you want to run, and then run `cargo nextest run <the name of your test> --workspace` to execute your test(s).

Below are two examples of unit tests for `DropPacketCell` for reference:

```rust
#[cfg(test)]
mod tests {
    use tracing::{span, Level, info};

    use crate::cells::StdPacket;

    use super::*;

    #[test_log::test]
    fn test_drop_packet_cell() -> Result<(), Error> {
        let _span = span!(Level::INFO, "test_drop_packet_cell").entered();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _guard = rt.enter();

        info!("Creating cell with loss_interval 3");
        let cell = DropPacketCell::new(3_usize)?;
        let ingress = cell.sender();
        let mut egress = cell.into_receiver();
        egress.reset();
        egress.change_state(2);

        info!("Testing loss for drop packet cell of loss 3");
        let mut received_packets = vec![false; 100];

        for i in 0..100 {
            let mut test_packet = StdPacket::from_raw_buffer(&[0; 256]);
            test_packet.set_flow_id(i);
            ingress.enqueue(test_packet)?;
            let received = rt.block_on(async { egress.dequeue().await });

            match received {
                Some(content) => {
                    assert!(content.length() == 256);
                    received_packets[content.get_flow_id() as usize] = true;
                }
                None => {}
            }
        }
        info!("Tested loss sequence: {:?}", received_packets);
        for i in 0..100 {
            if (i + 1) % 3 == 0 {
                assert!(!received_packets[i]);
            } else {
                assert!(received_packets[i]);
            }
        }
        Ok(())
    }

    #[test_log::test]
    fn test_drop_packet_cell_config_update() -> Result<(), Error> {
        let _span = span!(Level::INFO, "test_drop_packet_cell_config_update").entered();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _guard = rt.enter();

        info!("Creating cell with loss_interval 5");
        let cell = DropPacketCell::new(5_usize)?;
        let config_changer = cell.control_interface();
        let ingress = cell.sender();
        let mut egress = cell.into_receiver();
        egress.reset();
        egress.change_state(2);

        let mut received_packets = vec![false; 100];

        info!("Testing loss for drop packet cell of loss 5");

        for i in 0..48 {
            let mut test_packet = StdPacket::from_raw_buffer(&[0; 256]);
            test_packet.set_flow_id(i);
            ingress.enqueue(test_packet)?;
            let received = rt.block_on(async { egress.dequeue().await });

            match received {
                Some(content) => {
                    assert!(content.length() == 256);
                    received_packets[content.get_flow_id() as usize] = true;
                }
                None => {}
            }
        }
        for i in 0..48 {
            if (i + 1) % 5 == 0 {
                assert!(!received_packets[i]);
            } else {
                assert!(received_packets[i]);
            }
        }
        
        info!("Changing loss_interval to 3");
        config_changer.set_config(DropPacketCellConfig::new(3_usize))?;

        for i in 48..100 {
            let mut test_packet = StdPacket::from_raw_buffer(&[0; 256]);
            test_packet.set_flow_id(i);
            ingress.enqueue(test_packet)?;
            let received = rt.block_on(async { egress.dequeue().await });

            match received {
                Some(content) => {
                    assert!(content.length() == 256);
                    received_packets[content.get_flow_id() as usize] = true;
                }
                None => {}
            }
        }
        info!("Tested loss sequence: {:?}", received_packets);
        for i in 0..=51 {
            if i % 3 == 0 {
                assert!(!received_packets[i + 48]);
            } else {
                assert!(received_packets[i + 48]);
            }
        }
        Ok(())
    }
}
```

### Integration Tests

This part is optional when you are writing a new cell. You only need to write integration tests for your cell if you wish to integrate it into the rattan official repository and the corresponding CLI tool.

The purpose of integration tests is to check whether your cell runs correctly in the rattan CLI tool. When writing integration tests, you need to first create your cell in the rattan environment, then execute commands such as `ping` between network namespaces via the CLI, and observe whether your cell behaves as expected.

To run the integration tests, you can first run `cargo nextest list --workspace` to view the list of unit tests, find the test you want to run, and then run `cargo nextest run <the name of your test> --workspace --release` to execute your test.

Here is an example of an integration test for reference:

```rust
#[instrument]
#[test_log::test]
fn test_drop_packet() {
    let mut config = RattanConfig::<StdPacket> {
        env: StdNetEnvConfig {
            mode: StdNetEnvMode::Isolated,
            client_cores: vec![1],
            server_cores: vec![3],
            ..Default::default()
        },
        ..Default::default()
    };
    config.cells.insert(
        "up_drop".to_string(),
        CellBuildConfig::DropPackets(LosePacketCellConfig::new(0_usize)),
    );
    config.cells.insert(
        "down_drop".to_string(),
        CellBuildConfig::DropPackets(LosePacketCellConfig::new(0_usize)),
    );
    config.links = HashMap::from([
        ("left".to_string(), "up_drop".to_string()),
        ("up_drop".to_string(), "right".to_string()),
        ("right".to_string(), "down_drop".to_string()),
        ("down_drop".to_string(), "left".to_string()),
    ]);
    let mut radix = RattanRadix::<AfPacketDriver>::new(config).unwrap();
    radix.spawn_rattan().unwrap();
    radix.start_rattan().unwrap();

    // Wait for AfPacketDriver to be ready
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Before set the LossCell, the average loss rate should be 0%
    {
        let _span = span!(Level::INFO, "ping_no_loss").entered();
        info!("try to ping with no loss");
        let right_ip = radix.right_ip(1).to_string();
        let left_handle = radix
            .left_spawn(None, move || {
                let handle = std::process::Command::new("ping")
                    .args([&right_ip, "-c", "20", "-i", "0.3"])
                    .stdout(std::process::Stdio::piped())
                    .spawn()
                    .unwrap();
                Ok(handle.wait_with_output())
            })
            .unwrap();
        let output = left_handle.join().unwrap().unwrap().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();

        let re = Regex::new(r"(\d+)% packet loss").unwrap();
        let loss_percentage = re
            .captures(&stdout)
            .map(|cap| cap[1].parse::<u64>())
            .unwrap()
            .unwrap();
        info!("loss_percentage: {}", loss_percentage);
        assert!(loss_percentage == 0);
    }

    // After set the loss_interval of DropPacketCell to 2, the average loss rate should be 50%
    {
        let _span = span!(Level::INFO, "ping_with_loss").entered();
        info!("try to ping with loss interval set to 2");
        radix
            .op_block_exec(RattanOp::ConfigCell(
                "up_drop".to_string(),
                serde_json::to_value(LosePacketCellConfig::new(2_usize)).unwrap(),
            ))
            .unwrap();

        let right_ip = radix.right_ip(1).to_string();
        let left_handle = radix
            .left_spawn(None, move || {
                let handle = std::process::Command::new("ping")
                    .args([&right_ip, "-c", "50", "-i", "0.3"])
                    .stdout(std::process::Stdio::piped())
                    .spawn()
                    .unwrap();
                Ok(handle.wait_with_output())
            })
            .unwrap();
        let output = left_handle.join().unwrap().unwrap().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();

        let re = Regex::new(r"(\d+)% packet loss").unwrap();
        let loss_percentage = re
            .captures(&stdout)
            .map(|cap| cap[1].parse::<u64>())
            .unwrap()
            .unwrap();
        info!("loss_percentage: {}", loss_percentage);
        assert!((loss_percentage == 50));
    }
}
```