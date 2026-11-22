use crate::x86::exception::EventState;
use std::mem::{offset_of, size_of};

// Private argument to the read-span import, outside the x86 segment IDs.
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(crate) const PREFIX_DWORD_READ_SEGMENT: u32 = 16;
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(crate) const PREFIX_INDEXED_READ_SEGMENT: u32 = 17;
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(crate) const INDEXED_SCALAR_READ: u32 = 1 << 31;
// The low bit of an indexed write page tag also certifies a readable backing.
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(crate) const INDEXED_WRITE_READABLE: u64 = 1;
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(crate) const INDEXED_PAGED_READ_GUARD: u32 = 1 << 31;
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(crate) const NEWEST_FETCH_GUARD: u32 = 1 << 30;
// The guard also refreshed indexed data authority in this paging context.
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(crate) const NEWEST_FETCH_INDEXED: u64 = 2;
// Numeric controls admit SSE while this executable proof is current.
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(crate) const NEWEST_FETCH_SSE: u64 = 4;
#[cfg(not(feature = "parallel"))]
pub(crate) const MXCSR_EXCEPTION_MASKS: u32 = 0x1f80;

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[derive(Default)]
#[repr(C)]
pub(crate) struct NewestFetch {
    pub(crate) tag: u64,
    pub(crate) id: u64,
    pub(crate) table: u32,
    pub(crate) source_invalid: u32,
    pub(crate) cache_changed: u32,
    pub(crate) slot: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct RawPagingSource {
    pub address: u64,
    pub has_address: u32,
    pub reserved: u32,
}

impl RawPagingSource {
    pub fn decode(self) -> Result<Option<u64>, &'static str> {
        if self.reserved != 0 || self.has_address > 1 || self.has_address == 0 && self.address != 0
        {
            return Err("invalid page fault source transport");
        }
        if self.has_address != 0 {
            if !crate::memory::canonical48(self.address) {
                return Err("noncanonical page fault source address");
            }
            Ok(Some(self.address))
        } else {
            Ok(None)
        }
    }

    pub fn encode(address: Option<u64>) -> Self {
        Self {
            address: address.unwrap_or(0),
            has_address: address.is_some().into(),
            reserved: 0,
        }
    }

    pub fn attach(self, event: &mut EventState) -> Result<(), &'static str> {
        let address = self.decode()?;
        if let EventState::Pending(pending) = event {
            if (pending.vector == 14) != address.is_some() {
                return Err("page fault address requires matching vector fourteen");
            }
            pending.page_fault_address = address;
        } else if address.is_some() {
            return Err("page fault address requires pending exception");
        }
        Ok(())
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() != size_of::<Self>() {
            return Err("page fault source input length differs");
        }
        let word = |offset| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let raw = Self {
            address: u64::from_le_bytes(
                bytes[offset_of!(Self, address)..offset_of!(Self, address) + 8]
                    .try_into()
                    .unwrap(),
            ),
            has_address: word(offset_of!(Self, has_address)),
            reserved: word(offset_of!(Self, reserved)),
        };
        raw.decode()?;
        Ok(raw)
    }

    pub fn descriptor() -> serde_json::Value {
        serde_json::json!({"bytes":size_of::<Self>(),"address":offset_of!(Self,address),"hasAddress":offset_of!(Self,has_address),"reserved":offset_of!(Self,reserved)})
    }
}
