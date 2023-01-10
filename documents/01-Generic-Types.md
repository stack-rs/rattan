# Components and Traits (Incomplete)

Notice: This document is still at a very early stage and will be revised during the further development. 

We use Rust's trait system to describe interfaces of Rattan components and wrappers to external resources. Code here is written for illustration even without compilation. It will be replaced with real code when they are eventually implemented.

## External Utility
### Time Type (`T`)
We use a generic type `T` to add a abstraction layer for time utilities. Any time-related functions should be accessed via this abstraction layer to implement time-independent code.

```rust
use core::marker::PhantomData;

pub trait TimeProvider {
    type Time;
    type Duration;

    fn current() -> Self::Time;
    fn sleep(d: Self::Duration);
}

struct CodelQueue<T> 
where
    T: TimeProvider
{
    phantom: PhantomData<T>
}

impl<T> CodelQueue<T>
where
    T: TimeProvider
{
    fn next_event() -> T::Time {
        panic!("")
    }
}
```
## Packet and Routing 
### Packet Type (`P`)
We use a generic type `P` to represent the packet type and a series of trait to mark its capability.

```rust
pub trait HasIngress {}
pub trait HasEgress {}
pub trait HasIngressL3Address {}
pub trait HasEgressL3Address {}
```
We can declare requirements on the packet type in the trait bound of `P` on types depending on `P`.

```rust
struct SimpleRoutingTable<P> 
where
    P: HasIngress + HasEgress
{}

struct QosRoutingTable<P> 
where
    P: HasIngress + HasEgress + HasIngressL3Address + HasEgressL3Address
{}
```

### Routing Table Type (`RT`)

We use a generic type `RT` to represent a routing table that decides the next device for every ingress packet. Currently routing tables are static, it remains unclear whether run-time routing update is necessary for channel simulation.

```rust
pub trait RoutingTable<P, D> {
    fn next(p: &P) -> D;
}
```

## Device and Router
###  Device Type (`D`)

We use a generic type `S` to denote devices participate in the emulation.
```rust
pub trait DeviceInterface<P> {
    fn read() -> P;
    fn write(packet: P);
}

pub trait PollInterface<E> 
where 
    E: Poll<Self>,
    Self: Sized
{
    fn add_to_poller(&self, poller: &mut E) {
        poller.add(self);
    }
}
```

### Poller Type (`E`)
We use a generic type `E` to denote a poller that monitors multiple devices and gets nonified when one of them are avialble for read or write. 

```rust
pub trait Poll<D> {
    fn add(&self, d: &D);
}

struct Veth {}
struct Epoll {}

impl Poll<Veth> for Epoll
{
    fn add(&self, d: &Veth) {}
}

impl<E, D> PollInterface<E> for D where E: Poll<D> {}
```

### Router Type (`R`)
Router is the core type of Rattan and glue all components.

```rust
pub trait Router {
    fn loop();
}
```

## Resource Management and Isolation
### Resource Type (`Q`)
We use the generic type `Q` to denote a RAII-style wrapper around raw hardware resource, which is associated with a native descriptor `RawDescriptorType`. In *nix systems following the "everything is a file" philosophy, `RawDescriptorType` is the file descriptor.

```rust
pub trait Resource {
    type RawDescriptorType;
    bool Setup();
    bool Teardown();
}
```

### Isolator Type (`S`)
We use a generic type `S` to denote a platform-specific isolator working on operating system resource.

```rust
struct VethInterface;
struct Namespace;

pub trait Isolatable<S> {}
impl<S, T> Isolatable<S> for T where S: Isolate<T> {}

pub trait Isolate<Q> {
    fn isolate(&self, resource: &Q);
    fn release(&self, resource: &Q);
}

impl Isolate<VethInterface> for Namespace {
    fn isolate(&self, _resource: &VethInterface) {}
    fn release(&self, _resource: &VethInterface) {}
}

impl Namespace {
    fn isolatable<T>(&self, _resource: &T)
    where
        T: Isolatable<Namespace>,
    {}
}

fn main() {
    let namespace = Namespace {};
    let interface = VethInterface {};
    namespace.isolate(&interface);
    namespace.isolatable(&interface);
}

```