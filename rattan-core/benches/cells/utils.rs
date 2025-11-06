use rattan_core::{
    cells::{Cell, CellState, Egress, Packet},
    core::CellFactory,
    error::Error,
};
use tokio::runtime::Handle;

use crate::TICK;

pub fn create_cell<C: Cell<P>, P: Packet, Config: CellFactory<C>>(
    config: Config,
    handle: &Handle,
) -> Result<C, Error> {
    let mut cell = config(handle)?;
    cell.receiver().change_state(CellState::Normal);
    Ok(cell)
}

pub fn clock() -> tokio::time::Interval {
    tokio::time::interval(TICK)
}
