/// This test need to be run as root (CAP_NET_ADMIN)
/// CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER='sudo -E' cargo test veth -- --ignored --nocapture
use anyhow::anyhow;
use futures::stream::TryStreamExt;
use rtnetlink::{Handle, LinkMessageBuilder, LinkUnspec, LinkVeth};
use std::net::{IpAddr, Ipv4Addr};
use tokio::{runtime, task};

pub async fn get_link_index(handle: &Handle, name: &str) -> anyhow::Result<u32> {
    Ok(handle
        .link()
        .get()
        .match_name(name.into())
        .execute()
        .try_next()
        .await?
        .ok_or(anyhow!("{} is not found", name))?
        .header
        .index)
}

pub struct VethCell {
    pub handle: Handle,
    pub index: u32,
    pub name: String,
}

impl VethCell {
    async fn enable(&mut self) -> anyhow::Result<()> {
        let message = LinkMessageBuilder::<LinkUnspec>::new()
            .index(self.index)
            .up()
            .build();
        Ok(self.handle.link().set(message).execute().await?)
    }

    async fn disable(&mut self) -> anyhow::Result<()> {
        let message = LinkMessageBuilder::<LinkUnspec>::new()
            .index(self.index)
            .down()
            .build();
        Ok(self.handle.link().set(message).execute().await?)
    }

    async fn set_l2_addr(&mut self, address: &[u8]) -> anyhow::Result<()> {
        let message = LinkMessageBuilder::<LinkUnspec>::new()
            .index(self.index)
            .address(address.into())
            .build();
        Ok(self.handle.link().set(message).execute().await?)
    }

    async fn set_l3_addr(&mut self, address: IpAddr, prefix: u8) -> anyhow::Result<()> {
        Ok(self
            .handle
            .address()
            .add(self.index, address, prefix)
            .execute()
            .await?)
    }
}

pub struct VethCellPair {
    left: VethCell,
    right: VethCell,
}

impl VethCellPair {
    pub async fn new(left_name: &str, right_name: &str) -> anyhow::Result<Self> {
        let (connection, handle, _) = rtnetlink::new_connection().unwrap();
        tokio::spawn(connection);

        let veth_left = left_name.to_string();
        let veth_right = right_name.to_string();

        let message = LinkMessageBuilder::<LinkVeth>::new(left_name, right_name).build();
        handle.link().add(message).execute().await?;

        let left_index = get_link_index(&handle, &veth_left).await?;
        let right_index = get_link_index(&handle, &veth_right).await?;

        Ok(VethCellPair {
            left: VethCell {
                handle: handle.clone(),
                index: left_index,
                name: veth_left,
            },
            right: VethCell {
                handle: handle.clone(),
                index: right_index,
                name: veth_right,
            },
        })
    }

    pub async fn enable(&mut self) -> anyhow::Result<()> {
        self.left.enable().await?;
        self.right.enable().await?;
        Ok(())
    }

    pub async fn disable(&mut self) -> anyhow::Result<()> {
        self.left.disable().await?;
        self.right.disable().await?;
        Ok(())
    }

    pub async fn set_l2_addr(&mut self, left_addr: &[u8], right_addr: &[u8]) -> anyhow::Result<()> {
        self.left.set_l2_addr(left_addr).await?;
        self.right.set_l2_addr(right_addr).await?;
        Ok(())
    }

    pub async fn set_l3_addr(
        &mut self,
        left_addr: IpAddr,
        left_prefix: u8,
        right_addr: IpAddr,
        right_prefix: u8,
    ) -> anyhow::Result<()> {
        self.left.set_l3_addr(left_addr, left_prefix).await?;
        self.right.set_l3_addr(right_addr, right_prefix).await?;
        Ok(())
    }
}

impl Drop for VethCellPair {
    fn drop(&mut self) {
        println!("drop veth pair {}/{}", self.left.name, self.right.name);
        let (handle, index, if_name) = (&self.left.handle, self.left.index, &self.left.name);

        let res = task::block_in_place(move || {
            runtime::Handle::current()
                .block_on(async move { handle.link().del(index).execute().await })
        });

        if let Err(e) = res {
            eprintln!("failed to delete veth pair: {e:?} (you may need to delete it manually with 'sudo ip link del {if_name}')");
        }
    }
}

#[test]
#[ignore]
fn veth_test() {
    let rt = runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let mut veth_pair = VethCellPair::new("test-veth-left", "test-veth-right")
            .await
            .unwrap();

        veth_pair.left.enable().await.unwrap();
        veth_pair.right.enable().await.unwrap();

        veth_pair
            .set_l2_addr(
                &[0x38, 0x7e, 0x58, 0xe7, 0x87, 0x2b],
                &[0x38, 0x7e, 0x58, 0xe7, 0x87, 0x2c],
            )
            .await
            .unwrap();
        veth_pair
            .set_l3_addr(
                IpAddr::V4(Ipv4Addr::new(192, 168, 55, 1)),
                24,
                IpAddr::V4(Ipv4Addr::new(192, 168, 55, 2)),
                25,
            )
            .await
            .unwrap();

        veth_pair.enable().await.unwrap();

        veth_pair.disable().await.unwrap();
    });
}
