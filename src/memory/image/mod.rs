#![cfg_attr(
    any(guest_memory64, parallel_memory64),
    allow(
        clippy::useless_conversion,
        reason = "backing offsets widen from u32 only in the memory64 build"
    )
)]

use std::cell::Cell;
use std::rc::Rc;

#[cfg(guest_memory64)]
pub(crate) mod guest_windows;
#[cfg(guest_memory64)]
mod memory64;
#[cfg(guest_memory64)]
pub(crate) use memory64::GuestSpan;
mod storage;
pub(super) use storage::BackingOffset;

use super::address::{physical_span_failure, span_failure};
use super::validity::Validity;

#[derive(Clone, Copy, Debug)]
pub struct MemoryLimits {
    pub max_backing_bytes: BackingOffset,
    pub max_backings: u32,
    pub max_mappings: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Permissions {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
}

#[derive(Clone, Debug)]
pub struct MappingInput {
    pub base: u64,
    pub backing: String,
    pub offset: BackingOffset,
    pub length: u32,
    pub permissions: Permissions,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryError {
    Invalid(&'static str),
    Capacity(&'static str),
}

impl MemoryError {
    pub fn message(self) -> &'static str {
        match self {
            Self::Invalid(message) | Self::Capacity(message) => message,
        }
    }
}

#[derive(Debug)]
pub struct Image {
    pub(super) backings: Vec<Backing>,
    pub(super) mappings: Vec<Mapping>,
    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    pub(super) mmio: Vec<super::MmioRange>,
    limits: MemoryLimits,
    pub(super) physical: bool,
    pub(super) validity: Validity,
    pub(super) last_mapping: Cell<usize>,
    pub(super) previous_mapping: Cell<usize>,
    #[cfg(all(feature = "parallel", any(target_arch = "wasm32", test)))]
    pub(super) shared: Option<super::parallel::SharedLayout>,
    #[cfg(parallel_memory64)]
    pub(super) narrow_shared_ram: bool,
    #[cfg(all(feature = "parallel", any(target_arch = "wasm32", test)))]
    pub(super) shared_rmw: Option<super::parallel::SharedHeld>,
}

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub(crate) struct DirectSpan {
    pub(crate) base: u64,
    pub(crate) length: u32,
    pub(crate) host: u32,
}

#[derive(Debug)]
pub(super) struct Backing {
    pub(super) id: String,
    #[cfg(not(guest_memory64))]
    pub(super) bytes: Vec<u8>,
    #[cfg(guest_memory64)]
    extent: memory64::Extent,
    pub(super) incarnation: Rc<()>,
    pub(super) has_sources: bool,
    #[cfg(all(feature = "parallel", any(target_arch = "wasm32", test)))]
    pub(super) shared: Option<super::parallel::SharedBacking>,
}

#[derive(Clone, Debug)]
pub(super) struct Mapping {
    pub(super) base: u64,
    pub(super) backing: usize,
    pub(super) offset: BackingOffset,
    pub(super) length: u32,
    pub(super) permissions: Permissions,
}

impl Mapping {
    pub(super) fn end(&self) -> u128 {
        u128::from(self.base) + u128::from(self.length)
    }
}

impl Image {
    pub fn new(
        code_base: u64,
        code: Vec<u8>,
        ram_base: u64,
        ram: Vec<u8>,
    ) -> Result<Self, &'static str> {
        let total = code
            .len()
            .checked_add(ram.len())
            .and_then(|length| BackingOffset::try_from(length).ok())
            .ok_or("combined image exceeds wasm32 input size")?;
        let mappings = vec![
            MappingInput {
                base: code_base,
                backing: "code".into(),
                offset: 0,
                length: u32::try_from(code.len()).map_err(|_| "code exceeds wasm32 size")?,
                permissions: Permissions {
                    read: true,
                    write: false,
                    execute: true,
                },
            },
            MappingInput {
                base: ram_base,
                backing: "ram".into(),
                offset: 0,
                length: u32::try_from(ram.len()).map_err(|_| "ram exceeds wasm32 size")?,
                permissions: Permissions {
                    read: true,
                    write: true,
                    execute: false,
                },
            },
        ];
        Self::mapped(
            vec![("code".into(), code), ("ram".into(), ram)],
            mappings,
            MemoryLimits {
                max_backing_bytes: total,
                max_backings: 2,
                max_mappings: 2,
            },
        )
        .map_err(MemoryError::message)
    }

    pub fn mapped(
        backings: Vec<(String, Vec<u8>)>,
        mappings: Vec<MappingInput>,
        limits: MemoryLimits,
    ) -> Result<Self, MemoryError> {
        Self::mapped_in_plane(backings, mappings, limits, false)
    }

    pub fn physical_mapped(
        backings: Vec<(String, Vec<u8>)>,
        mappings: Vec<MappingInput>,
        limits: MemoryLimits,
    ) -> Result<Self, MemoryError> {
        Self::mapped_in_plane(backings, mappings, limits, true)
    }

    fn mapped_in_plane(
        backings: Vec<(String, Vec<u8>)>,
        mappings: Vec<MappingInput>,
        limits: MemoryLimits,
        physical: bool,
    ) -> Result<Self, MemoryError> {
        if limits.max_backing_bytes == 0 || limits.max_backings == 0 || limits.max_mappings == 0 {
            return Err(MemoryError::Invalid(
                "memory limits must be positive wasm32 integers",
            ));
        }
        if backings.len() > limits.max_backings as usize
            || mappings.len() > limits.max_mappings as usize
        {
            return Err(MemoryError::Capacity(
                "initial memory record capacity exceeded",
            ));
        }
        let mut image = Self {
            backings: Vec::new(),
            mappings: Vec::new(),
            #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
            mmio: Vec::new(),
            limits,
            physical,
            validity: Validity::default(),
            last_mapping: Cell::new(usize::MAX),
            previous_mapping: Cell::new(usize::MAX),
            #[cfg(all(feature = "parallel", any(target_arch = "wasm32", test)))]
            shared: None,
            #[cfg(parallel_memory64)]
            narrow_shared_ram: false,
            #[cfg(all(feature = "parallel", any(target_arch = "wasm32", test)))]
            shared_rmw: None,
        };
        for (id, bytes) in backings {
            image.add_backing(id, bytes)?;
        }
        image.set_mappings(mappings)?;
        Ok(image)
    }

    pub(crate) fn check_backing(&self, id: &str, length: BackingOffset) -> Result<(), MemoryError> {
        if id.is_empty() || self.backings.iter().any(|backing| backing.id == id) {
            return Err(MemoryError::Invalid(
                "backing name must be nonempty and unique",
            ));
        }
        if length == 0 {
            return Err(MemoryError::Invalid("backing bytes must be nonempty"));
        }
        let total: u64 = self
            .backings
            .iter()
            .map(|backing| u64::from(backing.logical_length()))
            .sum();
        if self.backings.len() >= self.limits.max_backings as usize
            || u64::from(length) > u64::from(self.limits.max_backing_bytes) - total
        {
            return Err(MemoryError::Capacity("backing capacity exceeded"));
        }
        Ok(())
    }

    pub fn add_backing(&mut self, id: String, bytes: Vec<u8>) -> Result<(), MemoryError> {
        #[cfg(all(feature = "parallel", any(target_arch = "wasm32", test)))]
        if self.shared.is_some() {
            return Err(MemoryError::Invalid(
                "attached RAM requires a shared backing extent",
            ));
        }
        #[cfg(guest_memory64)]
        {
            self.add_memory64_backing(id, bytes.len() as u64, &bytes)
        }
        #[cfg(not(guest_memory64))]
        {
            self.check_backing(
                &id,
                BackingOffset::try_from(bytes.len())
                    .map_err(|_| MemoryError::Invalid("backing exceeds wasm32"))?,
            )?;
            self.backings
                .try_reserve(1)
                .map_err(|_| MemoryError::Capacity("backing allocation refused"))?;
            self.backings.push(Backing {
                id,
                bytes,
                incarnation: Rc::new(()),
                has_sources: false,
                #[cfg(all(feature = "parallel", any(target_arch = "wasm32", test)))]
                shared: None,
            });
            Ok(())
        }
    }

    #[cfg(guest_memory64)]
    pub(crate) fn add_memory64_backing(
        &mut self,
        id: String,
        length: u64,
        initial: &[u8],
    ) -> Result<(), MemoryError> {
        self.check_backing(&id, length)?;
        if initial.len() as u64 > length {
            return Err(MemoryError::Invalid("initial bytes exceed backing length"));
        }
        self.backings
            .try_reserve(1)
            .map_err(|_| MemoryError::Capacity("backing allocation refused"))?;
        self.backings.push(Backing {
            id,
            extent: memory64::Extent::new(length, initial, self.limits.guest_arena_alignment())?,
            incarnation: Rc::new(()),
            has_sources: false,
        });
        Ok(())
    }

    pub fn remove_backing(&mut self, id: &str) -> Result<(), MemoryError> {
        let index = self
            .backings
            .iter()
            .position(|backing| backing.id == id)
            .ok_or(MemoryError::Invalid("unknown backing name"))?;
        if self.mappings.iter().any(|mapping| mapping.backing == index) {
            return Err(MemoryError::Invalid("cannot remove a mapped backing"));
        }
        self.backings.remove(index);
        for mapping in &mut self.mappings {
            if mapping.backing > index {
                mapping.backing -= 1;
            }
        }
        Ok(())
    }

    pub(crate) fn mapping_capacity(&self) -> usize {
        let capacity = self.limits.max_mappings as usize;
        #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
        let capacity = capacity - self.mmio.len();
        capacity
    }

    pub fn set_mappings(&mut self, inputs: Vec<MappingInput>) -> Result<(), MemoryError> {
        if inputs.len() > self.mapping_capacity() {
            return Err(MemoryError::Capacity("mapping capacity exceeded"));
        }
        let mut mappings = Vec::new();
        mappings
            .try_reserve_exact(inputs.len())
            .map_err(|_| MemoryError::Capacity("mapping allocation refused"))?;
        for input in inputs {
            let backing = self
                .backings
                .iter()
                .position(|backing| backing.id == input.backing)
                .ok_or(MemoryError::Invalid("mapping refers to unknown backing"))?;
            if input.length == 0
                || self.mapping_span_failure(input.base, u64::from(input.length))
                || u64::from(input.offset)
                    .checked_add(u64::from(input.length))
                    .is_none_or(|end| end > u64::from(self.backings[backing].logical_length()))
            {
                return Err(MemoryError::Invalid(if self.physical {
                    "mapping must be a nonempty 52-bit physical backing slice"
                } else {
                    "mapping must be a nonempty canonical nonwrapping backing slice"
                }));
            }
            #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
            if self.overlaps_mmio(input.base, input.length) {
                return Err(MemoryError::Invalid("RAM mapping overlaps MMIO"));
            }
            mappings.push(Mapping {
                base: input.base,
                backing,
                offset: input.offset,
                length: input.length,
                permissions: input.permissions,
            });
        }
        mappings.sort_unstable_by_key(|mapping| mapping.base);
        if mappings
            .windows(2)
            .any(|pair| pair[0].end() > u128::from(pair[1].base))
        {
            return Err(MemoryError::Invalid("guest mappings overlap"));
        }
        self.validity.mapping_change(&self.backings, &mappings);
        self.mappings = mappings;
        self.last_mapping.set(usize::MAX);
        self.previous_mapping.set(usize::MAX);
        self.refresh_watchers();
        Ok(())
    }

    pub(super) fn mapping_span_failure(&self, address: u64, length: u64) -> bool {
        if self.physical {
            physical_span_failure(address, length).is_some()
        } else {
            span_failure(address, length).is_some()
        }
    }

    pub(super) fn locate(&self, address: u64) -> Option<&Mapping> {
        locate(&self.mappings, address)
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(crate) fn direct_read_span(&self, address: u64, width: u32) -> Option<DirectSpan> {
        if self.physical {
            return None;
        }
        self.readable_direct_span(address, width)
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(crate) fn physical_direct_read_pointer(&self, address: u64, width: u32) -> Option<u32> {
        let span = self.physical_direct_read_span(address, width)?;
        Some(span.host + (address - span.base) as u32)
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(crate) fn physical_direct_read_span(&self, address: u64, width: u32) -> Option<DirectSpan> {
        if !self.physical {
            return None;
        }
        self.readable_direct_span(address, width)
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    fn readable_direct_span(&self, address: u64, width: u32) -> Option<DirectSpan> {
        let mapping = &self.mappings[self.locate_index(address)?];
        self.readable_direct_span_in(mapping, address, width)
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    fn readable_direct_span_in(
        &self,
        mapping: &Mapping,
        address: u64,
        width: u32,
    ) -> Option<DirectSpan> {
        let relative = address - mapping.base;
        if !mapping.permissions.read || u64::from(width) > u64::from(mapping.length) - relative {
            return None;
        }
        // The installed mapping covers canonical addresses and a stable backing slice.
        let backing = &self.backings[mapping.backing];
        #[cfg(guest_memory64)]
        {
            guest_windows::span(
                mapping,
                backing.extent.base + mapping.offset,
                address,
                width,
            )
        }
        #[cfg(not(guest_memory64))]
        Some(DirectSpan {
            base: mapping.base,
            length: mapping.length,
            host: backing.host_pointer(mapping.offset) as u32,
        })
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(crate) fn physical_direct_copy_spans(
        &mut self,
        source: u64,
        destination: u64,
        width: u32,
    ) -> Option<(DirectSpan, DirectSpan)> {
        if !self.physical {
            return None;
        }
        let mapping = &self.mappings[self.locate_index(source)?];
        let read_span = self.readable_direct_span_in(mapping, source, width)?;
        if destination.wrapping_sub(read_span.base) <= u64::from(read_span.length - width) {
            if !self.direct_mapping_writable(mapping, destination - mapping.base, width) {
                return None;
            }
            #[cfg(not(guest_memory64))]
            let read_span = DirectSpan {
                host: self.backings[mapping.backing].host_pointer_mut(mapping.offset) as u32,
                ..read_span
            };
            return Some((read_span, read_span));
        }
        Some((read_span, self.writable_direct_span(destination, width)?))
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(crate) fn direct_write_span(&mut self, address: u64, width: u32) -> Option<DirectSpan> {
        if self.physical {
            return None;
        }
        self.writable_direct_span(address, width)
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(crate) fn physical_direct_write_span(
        &mut self,
        address: u64,
        width: u32,
    ) -> Option<DirectSpan> {
        if !self.physical {
            return None;
        }
        self.writable_direct_span(address, width)
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    fn writable_direct_span(&mut self, address: u64, width: u32) -> Option<DirectSpan> {
        let (mapping, _) = self.writable_direct_mapping(address, width)?;
        #[cfg(guest_memory64)]
        {
            guest_windows::span(
                mapping,
                self.backings[mapping.backing].extent.base + mapping.offset,
                address,
                width,
            )
        }
        #[cfg(not(guest_memory64))]
        {
            let (base, length, offset, backing) = (
                mapping.base,
                mapping.length,
                mapping.offset,
                mapping.backing,
            );
            Some(DirectSpan {
                base,
                length,
                host: self.backings[backing].host_pointer_mut(offset) as u32,
            })
        }
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(crate) fn direct_write_vector(&mut self, address: u64, bytes: &[u8; 16]) -> bool {
        if self.physical {
            return false;
        }
        let Some((mapping, relative)) = self.writable_direct_mapping(address, 16) else {
            return false;
        };
        let backing = mapping.backing;
        let start = mapping.offset + relative as BackingOffset;
        self.backings[backing].write_bytes(start, bytes);
        true
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    pub(crate) fn physical_direct_write<const WIDTH: usize>(
        &mut self,
        address: u64,
        value: [u8; WIDTH],
    ) -> Option<()> {
        if !self.physical {
            return None;
        }
        let (mapping, relative) = self.writable_direct_mapping(address, WIDTH as u32)?;
        let backing = mapping.backing;
        let index = mapping.offset + relative as BackingOffset;
        self.backings[backing].write_bytes(index, &value);
        Some(())
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(crate) fn physical_direct_write_ready(&self, address: u64, width: u32) -> bool {
        self.physical && self.writable_direct_mapping(address, width).is_some()
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    #[inline(always)]
    pub(crate) fn physical_read_mapped_byte_for_store(
        &mut self,
        address: u64,
        write_ready: impl FnOnce() -> bool,
    ) -> Option<(u8, Option<usize>)> {
        if !self.physical {
            return None;
        }
        let mapping = &self.mappings[self.locate_index(address)?];
        if !mapping.permissions.read {
            return None;
        }
        let relative = address - mapping.base;
        let backing = mapping.backing;
        let index = mapping.offset + relative as BackingOffset;
        let value = self.backings[backing].read_array::<1>(index)[0];
        // The generated store consumes this address within the same guarded entry.
        let store = if write_ready() && self.direct_mapping_writable(mapping, relative, 1) {
            #[cfg(guest_memory64)]
            {
                guest_windows::byte_token(self.backings[backing].extent.base + index)
                    .map(|token| token as usize)
            }
            #[cfg(not(guest_memory64))]
            {
                Some(
                    self.backings[backing]
                        .host_pointer_mut(index)
                        .expose_provenance(),
                )
            }
        } else {
            None
        };
        Some((value, store))
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    fn writable_direct_mapping(&self, address: u64, width: u32) -> Option<(&Mapping, u64)> {
        let mapping = &self.mappings[self.locate_index(address)?];
        let relative = address - mapping.base;
        self.direct_mapping_writable(mapping, relative, width)
            .then_some((mapping, relative))
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    #[inline(always)]
    fn direct_mapping_writable(&self, mapping: &Mapping, relative: u64, width: u32) -> bool {
        // Writable aliases of code must use the path that publishes invalidation.
        mapping.permissions.write
            && u64::from(width) <= u64::from(mapping.length) - relative
            && !self.backings[mapping.backing].has_sources
            && !self
                .mappings
                .iter()
                .any(|alias| alias.backing == mapping.backing && alias.permissions.execute)
    }

    #[inline(always)]
    pub(super) fn locate_index(&self, address: u64) -> Option<usize> {
        let cached = self.last_mapping.get();
        if let Some(mapping) = self.mappings.get(cached)
            && address >= mapping.base
            && address - mapping.base < u64::from(mapping.length)
        {
            return Some(cached);
        }
        self.locate_secondary(address)
    }

    #[inline(never)]
    fn locate_secondary(&self, address: u64) -> Option<usize> {
        let previous = self.previous_mapping.get();
        if let Some(mapping) = self.mappings.get(previous)
            && address >= mapping.base
            && address - mapping.base < u64::from(mapping.length)
        {
            return Some(previous);
        }
        self.locate_uncached(address)
    }

    #[cold]
    fn locate_uncached(&self, address: u64) -> Option<usize> {
        let index = locate_index(&self.mappings, address)?;
        self.previous_mapping.set(self.last_mapping.get());
        self.last_mapping.set(index);
        Some(index)
    }
}

pub(super) fn locate(mappings: &[Mapping], address: u64) -> Option<&Mapping> {
    locate_index(mappings, address).map(|index| &mappings[index])
}

pub(super) fn locate_index(mappings: &[Mapping], address: u64) -> Option<usize> {
    let index = mappings
        .partition_point(|mapping| mapping.base <= address)
        .checked_sub(1)?;
    (u128::from(address) < mappings[index].end()).then_some(index)
}

impl Backing {
    #[inline(always)]
    fn logical_length(&self) -> BackingOffset {
        #[cfg(all(feature = "parallel", any(target_arch = "wasm32", test)))]
        if let Some(backing) = self.shared {
            return backing.length;
        }
        #[cfg(not(guest_memory64))]
        {
            self.bytes.len() as BackingOffset
        }
        #[cfg(guest_memory64)]
        {
            self.extent.length
        }
    }
}

#[cfg(test)]
mod tests;
