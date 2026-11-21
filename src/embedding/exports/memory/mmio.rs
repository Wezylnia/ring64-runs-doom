use super::{BRIDGE, Core};
use crate::memory::{Access, MmioOperand};
use crate::x86::context::{AccessFailure, Segment};

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "mmio")]
unsafe extern "C" {
    #[link_name = "read"]
    fn device_read(address: u64, width: u32) -> u64;
    #[link_name = "write"]
    fn device_write(address: u64, width: u32, value: u64);
}

#[cfg(not(target_arch = "wasm32"))]
unsafe fn device_read(_: u64, _: u32) -> u64 {
    panic!("MMIO provider requires Wasm embedding")
}

#[cfg(not(target_arch = "wasm32"))]
unsafe fn device_write(_: u64, _: u32, _: u64) {
    panic!("MMIO provider requires Wasm embedding")
}

#[unsafe(no_mangle)]
pub extern "C" fn core_mmio_effect_active() -> i32 {
    BRIDGE
        .with_borrow(|bridge| i32::from(bridge.core.as_ref().is_some_and(|core| core.mmio_effect)))
}

impl Core {
    fn mmio_move(
        &mut self,
        address: u64,
        width: u32,
        access: Access,
        value: u64,
    ) -> Result<MmioOperand, AccessFailure> {
        if let Some(paging) = self.paging.as_ref() {
            let state = unsafe { &*self.state.get() };
            let memory = crate::x86::paging::access::Memory::new(
                &mut self.image,
                paging,
                self.context.cpl(),
                state.flags,
                &mut self.paging_work,
                &state.protection_keys,
            )
            .with_cache(&self.translation_cache);
            return if access == Access::Write {
                memory.write_move(address, width, value)
            } else {
                memory.read_move(address, width)
            };
        }
        if self.image.scalar_mmio(address, width) {
            return Ok(MmioOperand::Device(address));
        }
        if access == Access::Write {
            self.image
                .write(address, width, value)
                .map(|()| MmioOperand::Ram(value))
        } else {
            self.image.read(address, width).map(MmioOperand::Ram)
        }
        .map_err(AccessFailure::Linear)
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn core_mmio_operand(descriptor: u32, offset: u64, value: u64, segment: u32) -> i32 {
    let width = descriptor >> 2;
    if !matches!(width, 1 | 2 | 4 | 8) {
        return -1;
    }
    let Some(segment) = Segment::ALL.get(segment as usize).copied() else {
        return -1;
    };
    let access = if descriptor & 1 == 0 {
        Access::Read
    } else {
        Access::Write
    };
    let mask = u64::MAX >> (64 - width * 8);
    let prepared = BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        core.paging_failure = None;
        let Some(address) = core.segment_address(offset, width, segment, access, false) else {
            return Err(core.memory_failure_reason());
        };
        let outcome = match core.mmio_move(address, width, access, value & mask) {
            Ok(outcome) => outcome,
            Err(failure) => {
                core.access_failure(failure);
                return Err(core.memory_failure_reason());
            }
        };
        core.mmio_effect = matches!(outcome, MmioOperand::Device(_));
        Ok((address, outcome))
    });
    let (linear, outcome) = match prepared {
        Ok(prepared) => prepared,
        Err(reason) => return reason,
    };
    // A provider exception must not retain a borrow of the bridge.
    let value = match outcome {
        MmioOperand::Ram(value) => value,
        MmioOperand::Device(address) => unsafe {
            if access == Access::Write {
                device_write(address, width * 8, value & mask);
                value
            } else {
                device_read(address, width * 8) & mask
            }
        },
    };
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        core.mmio_effect = false;
        if descriptor & 2 != 0 {
            core.debug_operand(linear, width, access);
        }
        if access == Access::Read {
            unsafe {
                let state = &mut *core.state.get();
                state.vector_scratch[..8].copy_from_slice(&value.to_le_bytes());
            };
        }
        if core.image.active_changed() { 2 } else { 1 }
    })
}
