# Write a New Cell

In Rattan, a "cell" is a unit that emulates various network effects for your network path, including delay, packet loss, reordering, and so forth.
As the fundamental building block of Rattan, cells follow the actor concurrency programming model. Each cell runs as an self-contained coroutine that processes events through several well-defined asynchronous interfaces (i.e., `Ingress`, `Egress`, `ControlInterface`).

Users can combine different cells to emulate a variety of network scenarios. We've bundled several built-in cells in Rattan to support common network effects, including the `Delay` cell, `Loss` cell, among others.

However, there may be instances where users wish to implement additional effects or need to cater to specific requirements within a cell. In such cases, they can follow this guide to create their own custom cell.

## Basic Structure

A cell in Rattan is primarily composed of the following components: [`Ingress`](#ingress), [`Egress`](#egress), and [`ControlInterface`](#controlinterface).
To implement your own cell, you need to implement these three traits, along with an additional [`Cell`](#cell) trait to combine them together.

In the design of Rattan, each `Cell` receives packets via `Ingress` and sends packets via `Egress`. The `Cell` settings can be adjusted at runtime through the `ControlInterface`. The Rattan core handles the forwarding of packets from the `Egress` of one `Cell` to the `Ingress` of another.

In the following sections, we will use a `DropPacketCell` as an example, which drops one packet every `loss_interval` packets, to illustrate the basic structure and implementation process of a cell.

### Ingress

The `Ingress` is used to receive packets, which are dequeued by other cells and forwarded by the rattan core.
It serves as the entry point for our cell, hence the need for us to implement an `enqueue` method to pass forwarded packets into the cell.

Typically, we use a channel to forward packets within the cell. As such, we can incorporate an `UnboundedSender` member within `Ingress` and use its `send` method to transmit packets (similarly, we can include an `UnboundedReceiver` member within `Egress` and use its `recv` method to receive packets).

A single `Ingress` is shared across multiple forwarding threads to allow flexible composition, so it can only have an immutable reference to itself in the `enqueue` method and must implement the `Clone` trait. It is worth attention that mutable operations on packets are possible within the method.

Here is an example of an `Ingress` implementation, which we set the timestamp of the packet to the current time before sending it to the `Egress`:

```rust
pub struct DropPacketCellIngress<P>
where
    P: Packet,
{
    // Send packets to Egress
    ingress: mpsc::UnboundedSender<P>,
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

The `Egress` is used to dequeue packets from the `Cell`, which are then sent to another cell by the rattan core. It serves as the export of the cell.

As we mentioned above, we typically use a channel to forward packets within the cell. Therefore, we can include an `UnboundedReceiver` member within `Egress` and use its `recv` method to receive packets for further process.

The `Egress` component is designed to be exclusively held by a single thread, allowing its `dequeue` method to obtain a mutable reference to itself.
This design allows for stateful operations within this method. Network effects often necessitate maintaining mutable internal states.

For instance, the Gilbert-Elliott packet loss model requires internal state transitions along with a varying packet loss probability.

Consequently, we usually implement the majority of packet handling strategies, such as delaying or dropping a packet, and modify the internal states within the `dequeue` method.

We always include a `state` variable (typically, a `AtomicI32`) in the `Egress` struct to control the cell's state, which is informed by Rattan runtime through method `change_state`. Currently, three values are supported: 0 (drop), 1 (pass-through), and 2 (normal operation).
Also, we include a configuration receiver (the specific type depends on the way you implement trait `ControlInterface`) to receive configuration information from the `ControlInterface`.

An example is shown below:

```rust
pub struct DropPacketCellEgress<P>
where
    P: Packet,
{
    // Receive packets from Ingress
    egress: mpsc::UnboundedReceiver<P>,
    // Internal states
    inner_loss_interval: usize,
    counter: usize,
    loss_interval: Arc<AtomicUsize>,
    state: AtomicI32,
}
```

Here, `loss_interval` is used to receive the configuration information from the `ControlInterface`, `inner_loss_interval` is used to store the received parameters, `counter` is used to count the number of packets since last packet loss, and `state` is used to control the state of the cell.

For the `Egress` trait, you must implement the `dequeue` method and might override the `change_state` and `reset` method (both are blank implementations by default).
The `dequeue` method is where you implement the functional logic of your cell. For example, if you need a delay function, you can write your `dequeue` method to return a packet after a specified delay time; if you need to drop packets, you can write your `dequeue` method to return a packet or drop it with a certain probability.
The `change_state` method is used to control the state of your cell.
The `reset` method is used to reset the internal state of your cell, often useful in calibrating the internal timer, which will always be called by Rattan runtime before each run.

Here is an example of an `Egress` implementation, which drops one packet every `loss_interval` packets:

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
                // Drop
                return None;
            }
            1 => {
                // Pass-through
                return Some(packet);
            }
            _ => {}
        }
        // The control logic of your own cell
        self.counter += 1;
        self.inner_loss_interval = *self.loss_interval.load(std::sync::atomic::Ordering::Release);
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

The `ControlInterface` trait is only required when you need to modify the configuration (or exchange information) of your cell at runtime. It is used to pass configuration information to the `Egress` component.
Developers are responsible for

Developers are responsible for embedding communication mechanisms at both ends (i.e., `ControlInterface` and `Egress`).
This can be achieved using atomic types, channels, or shared memory, depending on the specific requirements of the cell.
There is also a requirement to modify `Egress` to read or listen for pertinent information.
The only method that this trait requires is the `set_config` method, which is designed to execute the desired communication mechanism or other logical code.

The example provided in this guide utilizes atomic types and is intended solely for reference purposes:

```rust
pub struct DropPacketCellControlInterface {
    loss_interval: Arc<AtomicUsize>,
}

impl ControlInterface for DropPacketCellControlInterface {
    type Config = DropPacketCellConfig;

    fn set_config(&self, config: Self::Config) -> Result<(), Error> {
        self.loss_interval.store(config.loss_interval, std::sync::atomic::Ordering::Acquire);
        Ok(())
    }
}
```

Note that the code above includes a `DropPacketCellConfig` struct, which is the configuration struct defined for the example cell.
When implementing the `ControlInterface` trait, we also need to define a struct for your cell's configuration.
This struct typically contains the parameters that you want to use within the cell.
It will also be helpful if you want to read settings or parameters from some configuration files (e.g., TOML).

```rust
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

Up until now, we have implemented the three necessary traits for creating a custom cell: `Ingress`, `Egress`, and `ControlInterface`, along with the struct for the cell's configuration information.
However, these structs are just parts of the cell. In order to make them function properly in Rattan, we need to assemble them into a single struct. This is why we need to implement the `Cell` trait.

The `Cell` trait has four methods that must be implemented: `sender`, `receiver`, `into_receiver`, and `control_interface`:

- The `sender` method returns a cloned `Arc` of the `Ingress`
- The `receiver` method returns a mutable reference to the `Egress`
- The `into_receiver` method returns the `Egress`
- The `control_interface` method returns an `Arc` of the `ControlInterface`

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
        let loss_interval = Arc::new(AtomicUsize::new(loss_interval));
        Ok(DropPacketCell {
            ingress: Arc::new(DropPacketCellIngress { ingress: rx }),
            egress: DropPacketCellEgress {
                egress: tx,
                loss_interval: loss_interval.clone(),
                inner_loss_interval: 0,
                state: AtomicI32::new(0),
                counter: 0,
            },
            control_interface: Arc::new(DropPacketCellControlInterface { loss_interval }),
        })
    }
}
```

## Integrate Cell to Rattan

In the guide above, we have created a new `Cell`. To integrate it into Rattan, you also have to implement the `CellFactory` trait, which serves as the builder function of the `Cell`.
The `CellFactory` trait is a type alias for a closure that takes a reference to a tokio runtime `Handle` and returns a `Cell` instance.
You should call the `build_cell` method of `RattanRadix` to build your cell.

It is recommended to implement a method for a struct to return the closure like the example below:

```rust
pub type DropPacketCellBuildConfig = drop_packet::DropPacketCellConfig;

impl DropPacketCellBuildConfig {
    pub fn into_factory<P: Packet>(self) -> impl CellFactory<drop_packet::DropPacketCell<P>> {
        move |handle| {
            let _guard = handle.enter();
            // Create the closure to build the cell
            drop_packet::DropPacketCell::new(self.loss_interval)
        }
    }
}
```

If you want to contribute back to the official Rattan repository or modify the source code, you can also add your build configuration type to the enum `CellBuildConfig`.
In this way, you can easily read its configuration from a TOML file through the `load_cells_config` method (which internally calls `build_cell` as well) of `RattanRadix`.
Remember to also derive the `Deserialize` and `Serialize` traits for your configuration struct in such cases.

```rust
pub enum CellBuildConfig<P: Packet> {
    /* Other cells */
    DropPacket(DropPacketCellBuildConfig),
    Custom,
}
```

Don't forget to modify `load_cells_config` in `RattanRadix` to add your cell configuration to the match statement.

Now, you can add your cell to a TOML file and use Rattan to try parsing it!
There is an example for you to refer to to write your TOML file:

```toml
# Other Sections
# ...

# ----- Cells Section -----
# DropPacket Cell Example
[cells.my_drop]
type = "DropPacket"
loss_interval = 2

# Other Cells
# ...
# ----- Cells Section End -----
```

## Test Your New Cell

This section is optional when you are writing a new cell. Tests are only required when you wish to integrate your cell into Rattan's official repository and the corresponding CLI tool.

We require every cell to have unit tests and integration tests to check the correctness of the internal cell implementations and external interactions

### Unit Tests

The goal of unit tests is to check whether the internal state of your cell transitions correctly and whether its functionality behaves as expected.
Typically, when writing unit tests, you create an instance of your cell, manually call its `enqueue` and `dequeue` methods, and observe the results to check its correctness.

To run the unit tests, you can first run `cargo nextest list --workspace` to view the list of unit tests, find the test(s) you want to run, and then run `cargo nextest run <the name of your test> --release --all-features --workspace` to execute your test(s).

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

The purpose of integration tests is to check whether your cell interacts correctly with Rattan's runtime.
When writing integration tests, you need to first create your cell in the rattan runtime, and then execute commands such as `ping` between network namespaces to observe whether your cell behaves as expected.

To run the integration tests, you can first run `cargo nextest list --workspace` to view the list of integration tests, find the test you want to run, and then run `cargo nextest run <the name of your test> --release --all-features --workspace` to execute your test.

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
        CellBuildConfig::DropPacket(DropPacketCellConfig::new(0_usize)),
    );
    config.cells.insert(
        "down_drop".to_string(),
        CellBuildConfig::DropPacket(DropPacketCellConfig::new(0_usize)),
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
