use super::*;
use crate::memory::{MemoryError, SharedDword};

#[unsafe(no_mangle)]
pub extern "C" fn core_parallel_abi() -> u32 {
    2
}

fn complete(bridge: &mut Bridge, result: Result<(), MemoryError>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(error) => {
            bridge.error(error.message());
            match error {
                MemoryError::Invalid(_) => 1,
                MemoryError::Capacity(_) => 2,
            }
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn core_parallel_add_backing(length: u32, offset: u32, incarnation: u64) -> i32 {
    BRIDGE.with_borrow_mut(|bridge| {
        let Ok(id) = serde_json::from_slice::<String>(&bridge.input) else {
            return bridge.error("invalid shared backing name");
        };
        let core = bridge.core.as_mut().expect("initialized parallel core");
        let result = core
            .image
            .add_shared_backing(id, length, offset, incarnation);
        if result.is_ok() {
            core.memory_admin_changed();
        }
        complete(bridge, result)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_parallel_proof(owner: u64) -> i32 {
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_ref().expect("initialized parallel core");
        let Some(ranges) = core.image.parallel_proof(owner) else {
            return 1;
        };
        bridge.output.clear();
        for (offset, length) in ranges {
            bridge.output.extend_from_slice(&offset.to_le_bytes());
            bridge.output.extend_from_slice(&length.to_le_bytes());
        }
        0
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_parallel_invalidate(offset: u32, length: u32) {
    BRIDGE.with_borrow_mut(|bridge| {
        bridge
            .core
            .as_mut()
            .expect("initialized parallel core")
            .image
            .parallel_invalidate_range(offset, length);
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn core_parallel_admin_changed() {
    BRIDGE.with_borrow_mut(|bridge| {
        bridge
            .core
            .as_mut()
            .expect("initialized parallel core")
            .memory_admin_changed();
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn core_parallel_finish_slice() {
    BRIDGE.with_borrow_mut(|bridge| {
        // An x87 store can retire without using the preflighted destination.
        // Neither a prepared write nor its RAM grant may survive this slice.
        bridge
            .core
            .as_mut()
            .expect("initialized parallel core")
            .paging_work
            .clear();
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn core_parallel_resolve(
    address: u64,
    width: u32,
    access: u32,
    segment: u32,
) -> u32 {
    BRIDGE.with_borrow(|bridge| {
        let core = bridge.core.as_ref().expect("initialized parallel core");
        let kind = if access == 0 {
            Access::Read
        } else {
            Access::Write
        };
        let Some(address) = linear_address(core, address, width, segment, kind, access == 2) else {
            return 0;
        };
        core.image
            .shared_direct_lane(address, width, kind, false, access == 2)
            .ok()
            .flatten()
            .unwrap_or(0)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_parallel_vector_dword(offset: u64, segment: u32) -> u64 {
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().expect("initialized parallel core");
        let Some(address) = core.vector_address(offset, 4, segment, 1, Access::Read) else {
            return 0;
        };
        // Zero reports a fault. Positive returns a native byte offset, negative returns loaded bits.
        match core.image.shared_vector_dword(address) {
            Ok(SharedDword::Direct(absolute)) => (1 << 32) | u64::from(absolute),
            Ok(SharedDword::Loaded(bits)) => (1 << 63) | u64::from(bits),
            Err(fault) => {
                core.access_failure(AccessFailure::Linear(fault));
                0
            }
        }
    })
}

fn linear_address(
    core: &Core,
    offset: u64,
    width: u32,
    segment: u32,
    access: Access,
    rmw: bool,
) -> Option<u64> {
    if segment == u32::MAX {
        Some(offset)
    } else {
        core.context
            .translate(offset, width, Segment::from_id(segment), access, rmw)
            .ok()
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn core_parallel_guard(offset: u64, width: u32, segment: u32) -> i32 {
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().expect("initialized parallel core");
        let Some(address) = linear_address(core, offset, width, segment, Access::Write, true)
        else {
            return 0;
        };
        i32::from(core.image.begin_shared_guarded_rmw(address, width))
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_parallel_unguard() {
    BRIDGE.with_borrow_mut(|bridge| {
        bridge
            .core
            .as_mut()
            .expect("initialized parallel core")
            .image
            .end_shared_guarded_rmw();
    });
}
