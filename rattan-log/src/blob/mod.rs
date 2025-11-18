use binread::BinRead;
use bitfield::{BitRange, BitRangeMut};
#[derive(Debug, Clone, Copy, BinRead, PartialEq, Eq, Default)]
#[repr(transparent)]
pub struct RelativePointer(u32);

impl RelativePointer {
    pub fn new() -> Self {
        Self(0)
    }

    pub fn set_length(&mut self, value: u8) {
        self.0.set_bit_range(31, 24, value);
    }

    pub fn get_length(&self) -> u8 {
        self.0.bit_range(31, 24)
    }

    // Max 16MiB. Relative to the `chunk ref offset base`.
    pub fn set_offset(&mut self, value: u32) {
        self.0.set_bit_range(23, 0, value);
    }

    pub fn get_offset(&self) -> u32 {
        self.0.bit_range(23, 0)
    }
}
