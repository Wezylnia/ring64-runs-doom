use serde_json::Value;

use super::*;
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
fn name(value: &Value) -> Result<&str, MemoryError> {
    value
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(invalid)
}
fn array(value: &Value) -> Result<&Vec<Value>, MemoryError> {
    value.as_array().ok_or_else(invalid)
}
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
            offset: word(&input["offset"])?,
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

struct InitialImage {
    image: Image,
    paging: Option<crate::x86::paging::State>,
    fast_system: crate::x86::fast_system::State,
    context: ExecutionContext,
    physical_controls: bool,
}

fn initial_image(input: &[u8], metadata_len: u32) -> Result<InitialImage, MemoryError> {
    let metadata_len = metadata_len as usize;
    let config = json(input.get(..metadata_len).ok_or_else(invalid)?)?;
    let caps = &config["memoryLimits"];
    let limits = MemoryLimits {
        max_backing_bytes: word(&caps["maxBackingBytes"])?,
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
        Ok::<_, MemoryError>(total + u64::from(word(&input["length"])?))
    })?;
    if total > u64::from(limits.max_backing_bytes) {
        return Err(MemoryError::Capacity("backing byte capacity exceeded"));
    }
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    let shared = config.get("shared").map(|shared| {
        ["ramBase", "ramCapacity", "guardBase", "watchBase"].map(|field| word(&shared[field]))
    });
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    let payload_bytes = if shared.is_some() { 0 } else { total };
    #[cfg(not(all(feature = "parallel", target_arch = "wasm32")))]
    let payload_bytes = total;
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
        let length = word(&description["length"])? as usize;
        let id = name(&description["id"])?;
        backings.push((id.into(), copy(&input[cursor..cursor + length])?));
        cursor += length;
    }
    macro_rules! image {
        ($standalone:ident, $physical:literal) => {{
            #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
            if let Some([ram_base, ram_capacity, guard_base, watch_base]) = shared {
                shared_image(
                    inputs,
                    maps,
                    limits,
                    $physical,
                    [ram_base?, ram_capacity?, guard_base?, watch_base?],
                )
            } else {
                Image::$standalone(backings, maps, limits)
            }
            #[cfg(not(all(feature = "parallel", target_arch = "wasm32")))]
            Image::$standalone(backings, maps, limits)
        }};
    }
    match config.get("memoryModel") {
        None | Some(Value::String(_))
            if config.get("memoryModel").is_none_or(|v| v == "linear") =>
        {
            if config.get("paging").is_some() || config.get("fastSystem").is_some() {
                return Err(invalid());
            }
            image!(mapped, false).map(|image| InitialImage {
                image,
                paging: None,
                fast_system: Default::default(),
                context: Default::default(),
                physical_controls: false,
            })
        }
        Some(Value::String(model)) if model == "ia32e-paged" => {
            let state =
                super::paging::parse_bank(&config["paging"]).map_err(MemoryError::Invalid)?;
            state.validate().map_err(MemoryError::Invalid)?;
            let fast_system = config
                .get("fastSystem")
                .map(super::fast_system::parse_bank)
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
                .map(super::fast_system::parse_bank)
                .transpose()
                .map_err(MemoryError::Invalid)?
                .unwrap_or_default();
            let context = ExecutionContext::from_json_profile(&config["context"], true)
                .map_err(MemoryError::Invalid)?;
            let state =
                super::paging::parse_bank(&config["paging"]).map_err(MemoryError::Invalid)?;
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
    }
}

#[cfg(all(feature = "parallel", target_arch = "wasm32"))]
fn shared_image(
    inputs: &[Value],
    maps: Vec<MappingInput>,
    limits: MemoryLimits,
    physical: bool,
    layout: [u32; 4],
) -> Result<Image, MemoryError> {
    let mut backings = Vec::new();
    backings
        .try_reserve_exact(inputs.len())
        .map_err(|_| MemoryError::Capacity("shared backing allocation refused"))?;
    for row in inputs {
        backings.push((
            name(&row["id"])?.into(),
            word(&row["length"])?,
            word(&row["offset"])?,
            row["incarnation"]
                .as_str()
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or_else(invalid)?,
        ));
    }
    Image::mapped_shared(backings, maps, limits, physical, layout)
}

#[unsafe(no_mangle)]
pub extern "C" fn core_init_mapped(entry: u64, metadata_len: u32) -> i32 {
    BRIDGE.with_borrow_mut(|bridge| {
        let InitialImage {
            image,
            paging,
            fast_system,
            context,
            physical_controls,
        } = match initial_image(&bridge.input, metadata_len) {
            Ok(image) => image,
            Err(error) => return memory_error(bridge, error),
        };
        bridge.core = Some(Core {
            task_work: None,
            io_present: false,
            io_configured: false,
            io_effect: false,
            image,
            fast_system,
            paging,
            physical_controls,
            paging_work: Default::default(),
            translation_cache: Default::default(),
            active_owner: None,
            paging_failure: None,
            pdpte_load_fault: None,
            paging_system: None,
            state: Box::new(UnsafeCell::new(RunState {
                rip: entry,
                ..RunState::default()
            })),
            fault: None,
            divide_failed: false,
            decode_fault: None,
            context,
            context_version: 0,
            segment_fault: None,
            alignment_fault: None,
            alignment_check: None,
            simd_exception: None,
            numeric_fault: None,
            numeric_form: None,
            general_protection: None,
            protection_fault: None,
            system_memory_fault: None,
            debug_exception: None,
            translation_owners: Default::default(),
            debug_candidate: 0,
        });
        bridge.info = serde_json::to_vec(&RunState::descriptor()).unwrap();
        0
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
            image.check_backing(id, input.len())?;
            image.add_backing(id.into(), copy(input)?)
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
pub extern "C" fn core_validity(owner: u64) -> i32 {
    BRIDGE.with_borrow(|bridge| i32::from(bridge.core.as_ref().unwrap().owner_alive(owner)))
}

#[unsafe(no_mangle)]
pub extern "C" fn core_publish(owner: u64) -> i32 {
    BRIDGE.with_borrow_mut(|bridge| i32::from(bridge.core.as_mut().unwrap().publish_owner(owner)))
}

#[unsafe(no_mangle)]
pub extern "C" fn core_activate(owner: u64) -> i32 {
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        i32::from(core.activate_owner(owner))
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_release(owner: u64) {
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        core.translation_owners.release(owner);
        core.image.release(owner);
    });
}
