# Write a New Cell

Following this guide, you can write a custom cell yourself.

## Basic Structure

We have some necessary structs and traits which you must impl for your own cell. We take a cell which loses one packet in every `n` packets (`n` can be set by users freely) as example to explain basic structure of a custom cell.

- `CellIngress`

  `CellIngress` helps enqueue packets into your cell. These packets will be waiting for the call of `dequeue` function of `CellEgress`.

  `Clone` trait and `Ingress` trait must be implemented for `CellIngress`. We can just implement a `CellIngress` struct for our custom cell just like this: 

  ```rust
  pub struct LosePacketCellIngress<P>
  where
      P: Packet,
  {
      ingress: mpsc::UnboundedSender<P>,          // Send packets to CellEgress
  }

  impl<P> Clone for LosePacketCellIngress<P>
  where
      P: Packet,
  {
      fn clone(&self) -> Self {
          Self {
              ingress: self.ingress.clone(),
          }
      }
  }

  impl<P> Ingress<P> for LosePacketCellIngress<P>
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

- `CellEgress`

  `CellEgress` is the core of our custom cell. It determines how packets will leave our cell (`dequeue`). We can add our limit to this part (mainly to `dequeue`) to achieve expected effect. `Egress` trait must be implemented for `CellEgress`.

  A simple example for `CellEgress`:

  ```rust
  pub struct LosePacketCellEgress<P>
  where
      P: Packet,
  {
      egress: mpsc::UnboundedReceiver<P>,                    // Receive packets from CellIngress
      inner_loss_interval: Box<usize>,
      // Your own parameters...
      counter: usize,                                        // Count number of packets since last packet loss
      // Your own state variables...
      loss_interval: Arc<AtomicRawCell<usize>>,              // Configuration receiver
      state: AtomicI32,                                      // State of CellEgress
  }

  #[async_trait]
  impl<P> Egress<P> for LosePacketCellEgress<P>
  where
      P: Packet + Send + Sync,
  {
      async fn dequeue(&mut self) -> Option<P> {
          // Receive packets from CellIngress
          let packet = match self.egress.recv().await {
              Some(packet) => packet,
              None => return None,
          };
          match self.state.load(std::sync::atomic::Ordering::Acquire) {
              0 => {
                  return None;
              }
              1 => {
                  return Some(packet);
              }
              _ => {}
          }
          // Add control logic of your own cell here
          self.counter += 1;
          if let Some(loss_interval) = self.loss_interval.swap_null() {
              self.inner_loss_interval = loss_interval;
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

- `CellConfig`

  `CellConfig` defines the structure of configuration of your cell.

  ```rust
  #[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
  #[derive(Debug, Default, Clone)]
  pub struct LosePacketCellConfig {
      pub loss_interval: usize,
  }

  impl LosePacketCellConfig {
      pub fn new<T: Into<usize>>(loss_interval: T) -> Self {
          Self {
              loss_interval: loss_interval.into(),
          }
      }
  }
  ```

- `CellControlInterface`

  `CellControlInterface` provides a way to control inner parameters for your cell while it is running. `ControlInterface` trait must be implemented for this struct.

  ```rust
  pub struct LosePacketCellControlInterface {
      /// Stored as nanoseconds
      loss_interval: Arc<AtomicRawCell<usize>>,
  }

  impl ControlInterface for LosePacketCellControlInterface {
      type Config = LosePacketCellConfig;
  
      fn set_config(&self, config: Self::Config) -> Result<(), Error> {
          self.loss_interval.store(Box::new(config.loss_interval));
          Ok(())
      }
  }
  ```

- `Cell`

  `Cell` combinates all these four structs to make a whole cell.

  ```rust
  pub struct LosePacketCell<P: Packet> {
      ingress: Arc<LosePacketCellIngress<P>>,
      egress: LosePacketCellEgress<P>,
      control_interface: Arc<LosePacketCellControlInterface>,
  }

  impl<P> Cell<P> for LossCell<P>
  where
      P: Packet + Send + Sync + 'static,
  {
      type IngressType = LosePacketCellIngress<P>;
      type EgressType = LosePacketCellEgress<P>;
      type ControlInterfaceType = LosePacketCellControlInterface;

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

  impl<P> LosePacketCell<P>
  where
      P: Packet,
  {
      pub fn new<L: Into<usize>>(loss_interval: L) -> Result<LosePacketCell<P>, Error> {
          let loss_interval = loss_interval.into();
          let (rx, tx) = mpsc::unbounded_channel();
          let loss_interval = Arc::new(AtomicRawCell::new(Box::new(loss_interval)));
          Ok(LosePacketCell {
              ingress: Arc::new(LosePacketCellIngress { ingress: rx }),
              egress: LosePacketCellEgress {
                  egress: tx,
                  loss_interval: Arc::clone(&loss_interval),
                  inner_loss_interval: Box::default(),
                  state: AtomicI32::new(0),
                  counter: 0,
              },
              control_interface: Arc::new(LosePacketCellControlInterface { loss_interval }),
          })
      }
  }
  ```

## Add Configuration to Your New Cell

- TODO: How to add configuration support to a new cell(In tests and in .toml files)

## Test Your New Cell

- TODO: Introduction

### Unit Tests

- TODO: Two examples of unit tests; How to run unit tests;

### Integration Tests

- TODO: An example of integration test; How to run integration tests;
