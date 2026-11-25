#![cfg_attr(
    any(guest_memory64, parallel_memory64),
    allow(
        clippy::useless_conversion,
        reason = "backing offsets widen from u32 only in the memory64 build"
    )
)]

use crate::embedding::exports::*;
use serde_json::Value;

use crate::memory::{MappingInput, MemoryError, MemoryLimits, Permissions};

fn memory_error(bridge: &mut Bridge, error: MemoryError) -> i32 {
    bridge.error(error.message());
    match error {
        MemoryError::Invalid(_) => 1,
        MemoryError::Capacity(_) => 2,
    }
}

fn invalid() -> MemoryError {
    MemoryError::Invalid("invalid mapped memory input")
}
fn json(bytes: &[u8]) -> Result<Value, MemoryError> {
    serde_json::from_slice(bytes).map_err(|_| invalid())
}
fn word(value: &Value) -> Result<u32, MemoryError> {
    value
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(invalid)
}
#[cfg(any(guest_memory64, parallel_memory64))]
fn backing_size(value: &Value) -> Result<u64, MemoryError> {
    value
        .as_u64()
        .filter(|&n| n < (1 << 53))
        .ok_or_else(invalid)
}
#[cfg(not(any(guest_memory64, parallel_memory64)))]
use word as backing_size;

fn name(value: &Value) -> Result<&str, MemoryError> {
    value
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(invalid)
}
fn array(value: &Value) -> Result<&Vec<Value>, MemoryError> {
    value.as_array().ok_or_else(invalid)
}
#[cfg(not(guest_memory64))]
fn copy(bytes: &[u8]) -> Result<Vec<u8>, MemoryError> {
    let mut owned = Vec::new();
    owned
        .try_reserve_exact(bytes.len())
        .map_err(|_| MemoryError::Capacity("backing allocation refused"))?;
    owned.extend_from_slice(bytes);
    Ok(owned)
}

fn mappings(value: &Value, capacity: usize) -> Result<Vec<MappingInput>, MemoryError> {
    let inputs = array(value)?;
    if inputs.len() > capacity {
        return Err(MemoryError::Capacity("mapping capacity exceeded"));
    }
    let mut mappings = Vec::new();
    mappings
        .try_reserve_exact(inputs.len())
        .map_err(|_| MemoryError::Capacity("mapping allocation refused"))?;
    for input in inputs {
        let base = input["base"]
            .as_str()
            .and_then(|value| value.strip_prefix("0x"))
            .and_then(|value| u64::from_str_radix(value, 16).ok())
            .ok_or_else(invalid)?;
        let permissions = &input["permissions"];
        mappings.push(MappingInput {
            base,
            backing: name(&input["backing"])?.into(),
            offset: backing_size(&input["offset"])?,
            length: word(&input["length"])?,
            permissions: Permissions {
                read: permissions["read"].as_bool().ok_or_else(invalid)?,
                write: permissions["write"].as_bool().ok_or_else(invalid)?,
                execute: permissions["execute"].as_bool().ok_or_else(invalid)?,
            },
        });
    }
    Ok(mappings)
}

fn initial_image(input: &[u8], metadata_len: u32) -> Result<InitialImage, MemoryError> {
    let metadata_len = metadata_len as usize;
    let config = json(input.get(..metadata_len).ok_or_else(invalid)?)?;
    let caps = &config["memoryLimits"];
    let limits = MemoryLimits {
        max_backing_bytes: backing_size(&caps["maxBackingBytes"])?,
        max_backings: word(&caps["maxBackings"])?,
        max_mappings: word(&caps["maxMappings"])?,
    };
    if limits.max_backing_bytes == 0 || limits.max_backings == 0 || limits.max_mappings == 0 {
        return Err(invalid());
    }
    let inputs = array(&config["backings"])?;
    if inputs.len() > limits.max_backings as usize {
        return Err(MemoryError::Capacity("backing count capacity exceeded"));
    }
    let maps = mappings(&config["mappings"], limits.max_mappings as usize)?;
    let total = inputs.iter().try_fold(0u64, |total, input| {
        total
            .checked_add(u64::from(backing_size(&input["length"])?))
            .ok_or_else(invalid)
    })?;
    if total > u64::from(limits.max_backing_bytes) {
        return Err(MemoryError::Capacity("backing byte capacity exceeded"));
    }
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    let shared = config.get("shared");
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    let payload_bytes = if shared.is_some() { 0 } else { total };
    #[cfg(not(any(all(feature = "parallel", target_arch = "wasm32"), guest_memory64)))]
    let payload_bytes = total;
    #[cfg(guest_memory64)]
    let payload_bytes = inputs.iter().try_fold(0u64, |sum, row| {
        sum.checked_add(u64::from(word(&row["initialLength"])?))
            .ok_or_else(invalid)
    })?;
    if payload_bytes != (input.len() - metadata_len) as u64 {
        return Err(invalid());
    }
    let mut backings = Vec::new();
    backings
        .try_reserve_exact(inputs.len())
        .map_err(|_| MemoryError::Capacity("backing allocation refused"))?;
    let mut cursor = metadata_len;
    for description in inputs {
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        if shared.is_some() {
            break;
        }
        #[cfg(not(guest_memory64))]
        let length = word(&description["length"])? as usize;
        #[cfg(guest_memory64)]
        let length = word(&description["initialLength"])? as usize;
        let id = name(&description["id"])?;
        #[cfg(not(guest_memory64))]
        backings.push((id.into(), copy(&input[cursor..cursor + length])?));
        #[cfg(guest_memory64)]
        backings.push((
            id,
            backing_size(&description["length"])?,
            &input[cursor..cursor + length],
        ));
        cursor += length;
    }
    macro_rules! image {
        ($standalone:ident, $physical:literal) => {{
            #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
            if let Some(shared) = shared {
                shared_image(inputs, maps, limits, $physical, shared)
            } else {
                Image::$standalone(backings, maps, limits)
            }
            #[cfg(not(any(all(feature = "parallel", target_arch = "wasm32"), guest_memory64)))]
            {
                Image::$standalone(backings, maps, limits)
            }
            #[cfg(guest_memory64)]
            {
                let mut image = Image::$standalone(Vec::new(), Vec::new(), limits)?;
                for (id, length, initial) in backings {
                    image.add_memory64_backing(id.into(), length, initial)?;
                }
                image.set_mappings(maps)?;
                Ok::<Image, MemoryError>(image)
            }
        }};
    }
    let initial = match config.get("memoryModel") {
        None | Some(Value::String(_))
            if config.get("memoryModel").is_none_or(|v| v == "linear") =>
        {
            if config.get("paging").is_some() || config.get("fastSystem").is_some() {
                return Err(invalid());
            }
            image!(mapped, false).map(InitialImage::linear)
        }
        Some(Value::String(model)) if model == "ia32e-paged" => {
            let state = super::paging::control::parse_bank(&config["paging"])
                .map_err(MemoryError::Invalid)?;
            state.validate().map_err(MemoryError::Invalid)?;
            let fast_system = config
                .get("fastSystem")
                .map(crate::embedding::exports::system::fast_system::parse_bank)
                .transpose()
                .map_err(MemoryError::Invalid)?
                .unwrap_or_default();
            image!(physical_mapped, true).map(|image| InitialImage {
                image,
                paging: Some(state),
                fast_system,
                context: Default::default(),
                physical_controls: false,
            })
        }
        Some(Value::String(model)) if model == "physical" => {
            let fast_system = config
                .get("fastSystem")
                .map(crate::embedding::exports::system::fast_system::parse_bank)
                .transpose()
                .map_err(MemoryError::Invalid)?
                .unwrap_or_default();
            let context = ExecutionContext::from_json_profile(&config["context"], true)
                .map_err(MemoryError::Invalid)?;
            let state = super::paging::control::parse_bank(&config["paging"])
                .map_err(MemoryError::Invalid)?;
            state
                .validate_physical_context(&context)
                .map_err(MemoryError::Invalid)?;
            image!(physical_mapped, true).map(|image| InitialImage {
                image,
                paging: Some(state),
                fast_system,
                context,
                physical_controls: true,
            })
        }
        _ => Err(invalid()),
    }?;
    match config.get("mmioRanges") {
        None => Ok(initial),
        #[cfg(feature = "parallel")]
        Some(_) => Err(MemoryError::Invalid("parallel MMIO is unsupported")),
        #[cfg(not(feature = "parallel"))]
        Some(value) => {
            let entries = array(value)?;
            if entries.len() > limits.max_mappings as usize {
                return Err(MemoryError::Capacity("mapping capacity exceeded"));
            }
            let mut ranges = Vec::new();
            ranges
                .try_reserve_exact(entries.len())
                .map_err(|_| MemoryError::Capacity("MMIO allocation refused"))?;
            for entry in entries {
                let base = entry["base"]
                    .as_str()
                    .and_then(|value| value.strip_prefix("0x"))
                    .and_then(|value| u64::from_str_radix(value, 16).ok())
                    .ok_or_else(invalid)?;
                ranges.push(crate::memory::MmioRange {
                    base,
                    length: word(&entry["length"])?,
                });
            }
            let mut initial = initial;
            initial.image.install_mmio(ranges)?;
            Ok(initial)
        }
    }
}

#[cfg(all(feature = "parallel", target_arch = "wasm32"))]
fn shared_image(
    inputs: &[Value],
    maps: Vec<MappingInput>,
    limits: MemoryLimits,
    physical: bool,
    shared: &Value,
) -> Result<Image, MemoryError> {
    let [ram_base, ram_capacity, guard_base, watch_base] =
        ["ramBase", "ramCapacity", "guardBase", "watchBase"]
            .map(|field| backing_size(&shared[field]));
    let layout = [ram_base?, ram_capacity?, guard_base?, watch_base?];
    let mut backings = Vec::new();
    backings
        .try_reserve_exact(inputs.len())
        .map_err(|_| MemoryError::Capacity("shared backing allocation refused"))?;
    for row in inputs {
        backings.push((
            name(&row["id"])?.into(),
            backing_size(&row["length"])?,
            backing_size(&row["offset"])?,
            row["incarnation"]
                .as_str()
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or_else(invalid)?,
        ));
    }
    let image = Image::mapped_shared(backings, maps, limits, physical, layout)?;
    #[cfg(parallel_memory64)]
    let image = {
        let narrow = match shared.get("narrowRam") {
            Some(Value::Bool(value)) => *value,
            None => false,
            _ => return Err(invalid()),
        };
        let signal_offset = shared.get("signalOffset").map(backing_size).transpose()?;
        image.with_shared_ram_transport(narrow, signal_offset)?
    };
    Ok(image)
}

#[unsafe(no_mangle)]
pub extern "C" fn core_init_mapped(entry: u64, metadata_len: u32) -> i32 {
    BRIDGE.with_borrow_mut(|bridge| {
        let initial = match initial_image(&bridge.input, metadata_len) {
            Ok(initial) => initial,
            Err(error) => return memory_error(bridge, error),
        };
        bridge.install_core(entry, initial)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_add_backing(metadata_len: u32) -> i32 {
    BRIDGE.with_borrow_mut(|bridge| {
        let result = (|| {
            let metadata_len = metadata_len as usize;
            let config = json(bridge.input.get(..metadata_len).ok_or_else(invalid)?)?;
            let id = name(&config["id"])?;
            let input = &bridge.input[metadata_len..];
            let image = &mut bridge.core.as_mut().unwrap().image;
            #[cfg(guest_memory64)]
            {
                image.add_memory64_backing(id.into(), backing_size(&config["length"])?, input)
            }
            #[cfg(not(guest_memory64))]
            {
                image.check_backing(id, input.len().try_into().map_err(|_| invalid())?)?;
                image.add_backing(id.into(), copy(input)?)
            }
        })();
        match result {
            Ok(()) => {
                bridge.core.as_mut().unwrap().memory_admin_changed();
                0
            }
            Err(error) => memory_error(bridge, error),
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_remove_backing() -> i32 {
    BRIDGE.with_borrow_mut(|bridge| {
        let result = (|| {
            let id = json(&bridge.input)?;
            bridge
                .core
                .as_mut()
                .unwrap()
                .image
                .remove_backing(name(&id)?)
        })();
        match result {
            Ok(()) => {
                bridge.core.as_mut().unwrap().memory_admin_changed();
                0
            }
            Err(error) => memory_error(bridge, error),
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_set_mappings() -> i32 {
    BRIDGE.with_borrow_mut(|bridge| {
        let result = (|| {
            let image = &mut bridge.core.as_mut().unwrap().image;
            let inputs = mappings(&json(&bridge.input)?, image.mapping_capacity())?;
            image.set_mappings(inputs)
        })();
        match result {
            Ok(()) => {
                bridge.core.as_mut().unwrap().memory_admin_changed();
                0
            }
            Err(error) => memory_error(bridge, error),
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_read_memory(address: u64, length: u32) -> i32 {
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_ref().unwrap();
        match if core.paging.is_some() {
            core.image.physical_read_bytes(address, length)
        } else {
            core.image.read_bytes(address, length)
        } {
            Ok(bytes) => {
                bridge.output = bytes;
                0
            }
            Err(fault) => {
                bridge.info = serde_json::to_vec(&fault_json(&fault)).unwrap();
                1
            }
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_write_memory(address: u64) -> i32 {
    BRIDGE.with_borrow_mut(|bridge| {
        let Bridge {
            core, input, info, ..
        } = bridge;
        let core = core.as_mut().unwrap();
        match if core.paging.is_some() {
            core.image.physical_write_bytes(address, input)
        } else {
            core.image.write_bytes(address, input)
        } {
            Ok(()) => {
                core.memory_admin_changed();
                0
            }
            Err(fault) => {
                *info = serde_json::to_vec(&fault_json(&fault)).unwrap();
                1
            }
        }
    })
}
