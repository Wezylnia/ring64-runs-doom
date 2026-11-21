use super::super::image::Backing;
#[cfg(target_arch = "wasm32")]
use super::super::{Access, Fault};
use super::super::{Image, MemoryError};
use super::ram::{SharedBacking, SharedExclusive, SharedLayout};
use super::{gather, lane, scatter};

#[cfg(target_arch = "wasm32")]
pub(crate) enum SharedDword {
    Direct { address: u32, bytes: u32 },
    Loaded(u32),
}

impl Image {
    pub(crate) fn parallel_attached(&self) -> bool {
        self.shared.is_some()
    }

    pub(crate) fn shared_exclusive(&self) -> Option<SharedExclusive> {
        self.shared.map(|_| SharedExclusive::acquire())
    }

    pub(crate) fn bind_shared_layout(
        &mut self,
        ram_base: u32,
        ram_capacity: u32,
        guard_base: u32,
        watch_base: u32,
        extents: &[(u32, u64)],
    ) -> Result<(), MemoryError> {
        if self.shared.is_some() || extents.len() != self.backings.len() {
            return Err(MemoryError::Invalid(
                "shared RAM attachment does not match backings",
            ));
        }
        let layout = SharedLayout::new(ram_base, ram_capacity, guard_base, watch_base)?;
        let mut prepared = Vec::with_capacity(extents.len());
        for (backing, &(offset, incarnation)) in self.backings.iter().zip(extents) {
            let record = SharedBacking {
                offset,
                incarnation,
                length: backing.bytes.len() as u32,
            };
            layout.check_backing(prepared.iter().copied(), record)?;
            prepared.push(record);
        }
        for (backing, record) in self.backings.iter_mut().zip(prepared) {
            backing.shared = Some(record);
            backing.bytes = Vec::new();
        }
        self.shared = Some(layout);
        Ok(())
    }

    pub(crate) fn add_shared_backing(
        &mut self,
        id: String,
        length: u32,
        offset: u32,
        incarnation: u64,
    ) -> Result<(), MemoryError> {
        let layout = self
            .shared
            .ok_or(MemoryError::Invalid("no shared RAM attachment"))?;
        self.check_backing(&id, length as usize)?;
        let record = SharedBacking {
            offset,
            incarnation,
            length,
        };
        layout.check_backing(
            self.backings.iter().filter_map(|backing| backing.shared),
            record,
        )?;
        self.backings
            .try_reserve(1)
            .map_err(|_| MemoryError::Capacity("backing allocation refused"))?;
        self.backings.push(Backing {
            id,
            bytes: Vec::new(),
            incarnation: std::rc::Rc::new(()),
            has_sources: false,
            shared: Some(record),
        });
        Ok(())
    }

    pub(crate) fn parallel_invalidate_range(&mut self, absolute: u32, length: u32) {
        let end = u64::from(absolute) + u64::from(length);
        for backing in &self.backings {
            let Some(shared) = backing.shared else {
                continue;
            };
            let first = absolute.max(shared.offset);
            let last = end.min(u64::from(shared.offset) + u64::from(shared.length));
            if u64::from(first) < last {
                self.validity.write(
                    &backing.incarnation,
                    first - shared.offset,
                    (last - u64::from(first)) as u32,
                );
            }
        }
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn shared_direct_lane(
        &self,
        address: u64,
        width: u32,
        access: Access,
        physical: bool,
        rmw: bool,
    ) -> Result<Option<u32>, Fault> {
        let Some(layout) = self.shared else {
            return Ok(None);
        };
        assert!(matches!(width, 1 | 2 | 4 | 8));
        let index = if physical {
            self.preflight::<true>(address, width, access, rmw, true)?
        } else {
            self.preflight::<false>(address, width, access, rmw, true)?
        };
        let mapping = &self.mappings[index];
        let relative = (address - mapping.base) as u32;
        if width > mapping.length - relative {
            return Ok(None);
        }
        let backing = self.backings[mapping.backing]
            .shared
            .expect("shared backing binding");
        let absolute = backing.offset + mapping.offset + relative;
        if (absolute & 7) + width > 8
            || !layout.unpromoted(absolute)
            || access == Access::Write && layout.watched(absolute, width)
        {
            return Ok(None);
        }
        Ok(Some(absolute))
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn shared_vector_dword<const SPAN: bool>(
        &self,
        address: u64,
    ) -> Result<SharedDword, Fault> {
        let index = self.preflight::<false>(address, 4, Access::Read, false, true)?;
        let mut bytes = [0; 4];
        Ok(
            match self.shared_read_after_preflight::<true, SPAN>(address, &mut bytes, index) {
                Some((address, bytes)) => SharedDword::Direct { address, bytes },
                None => SharedDword::Loaded(u32::from_le_bytes(bytes)),
            },
        )
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn shared_dword_window(
        &self,
        address: u64,
        segment_limit: Option<u32>,
    ) -> Option<(u64, [u32; 3])> {
        let layout = self.shared?;
        let mapping = &self.mappings[self.locate_index(address)?];
        if !mapping.permissions.read {
            return None;
        }
        let base = mapping.base;
        let length = if let Some(segment_limit) = segment_limit {
            let segment_length = (u64::from(segment_limit) + 1).checked_sub(base)?;
            segment_length.min(u64::from(mapping.length))
        } else {
            // A physical mapping can cover bytes in the linear canonical hole.
            super::super::address::span_failure(base, u64::from(mapping.length))
                .is_none()
                .then_some(u64::from(mapping.length))?
        };
        let limit = u32::try_from(length.checked_sub(3)?).ok()?;
        if address - base >= u64::from(limit) {
            return None;
        }
        let backing = self.backings[mapping.backing].shared?;
        let host = backing.offset + mapping.offset;
        Some((base, [limit, host, layout.guard_bias()]))
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn shared_byte_store_span(
        &self,
        address: u64,
        segment_limit: Option<u32>,
    ) -> Option<(u32, u32)> {
        let layout = self.shared?;
        let index = self
            .preflight::<false>(address, 1, Access::Write, false, true)
            .ok()?;
        let mapping = &self.mappings[index];
        let relative = (address - mapping.base) as u32;
        let backing = self.backings[mapping.backing].shared?;
        let absolute = backing.offset + mapping.offset + relative;
        let mut bytes = (mapping.length - relative).min(layout.byte_store_room(absolute)?);
        if let Some(limit) = segment_limit {
            bytes = u64::from(bytes).min((u64::from(limit) + 1).checked_sub(address)?) as u32;
        }
        super::super::address::check_span(address, bytes, Access::Write).ok()?;
        (bytes != 0).then_some((absolute, bytes))
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn begin_shared_guarded_rmw(&mut self, address: u64, width: u32) -> bool {
        let Some(layout) = self.shared else {
            return false;
        };
        assert!(self.shared_rmw.is_none(), "nested resolved parallel RMW");
        if !matches!(width, 1 | 2 | 4 | 8 | 16) || width == 16 && address & 15 != 0 {
            return false;
        }
        let Ok(index) = self.preflight::<false>(address, width, Access::Write, true, true) else {
            return false;
        };
        let mut offsets = [0; 16];
        let offsets = &mut offsets[..width as usize];
        self.offsets(address, offsets, index);
        layout.begin_rmw(offsets, &mut self.shared_rmw)
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn end_shared_guarded_rmw(&mut self) {
        self.shared_rmw.take().expect("no resolved parallel RMW");
    }

    // Form exact canonical coordinates after architectural access admission.
    fn offsets(&self, address: u64, offsets: &mut [u32], mut index: usize) {
        for (current, offset) in (u128::from(address)..).zip(offsets) {
            if self.mappings[index].end() <= current {
                index += 1;
            }
            let mapping = &self.mappings[index];
            let backing = self.backings[mapping.backing]
                .shared
                .expect("shared backing binding");
            *offset = backing.offset + mapping.offset + (current as u64 - mapping.base) as u32;
        }
    }

    fn pieces(
        &self,
        address: u64,
        length: usize,
        mut index: usize,
        mut visit: impl FnMut(u32, usize, usize),
    ) {
        let mut current = u128::from(address);
        let mut done = 0;
        while done < length {
            let mapping = &self.mappings[index];
            let backing = self.backings[mapping.backing]
                .shared
                .expect("shared backing binding");
            let absolute = backing.offset + mapping.offset + (current as u64 - mapping.base) as u32;
            let remainder = (mapping.end() - current) as usize;
            let count = (length - done)
                .min(remainder)
                .min((8 - (absolute & 7)) as usize);
            visit(absolute, done, count);
            done += count;
            current += count as u128;
            if current == mapping.end() {
                index += 1;
            }
        }
    }

    pub(in crate::memory) fn shared_read(&self, address: u64, bytes: &mut [u8], index: usize) {
        let direct = self.shared_read_after_preflight::<false, false>(address, bytes, index);
        debug_assert!(direct.is_none());
    }

    // A returned canonical byte address leaves the dword load to generated Wasm.
    fn shared_read_after_preflight<const NATIVE_DWORD: bool, const SPAN: bool>(
        &self,
        address: u64,
        bytes: &mut [u8],
        index: usize,
    ) -> Option<(u32, u32)> {
        let Some(layout) = self.shared else {
            unreachable!()
        };
        if bytes.len() <= 8 && (address & 63) + bytes.len() as u64 <= 64 {
            if bytes.len() == 4 && self.shared_rmw.is_none() {
                let mapping = &self.mappings[index];
                let relative = (address - mapping.base) as u32;
                if 4 <= mapping.length - relative {
                    let backing = self.backings[mapping.backing]
                        .shared
                        .expect("shared backing binding");
                    let absolute = backing.offset + mapping.offset + relative;
                    if (absolute & 63) <= 60 {
                        if layout.read_contiguous_dword::<NATIVE_DWORD>(absolute, bytes) {
                            let bytes = if SPAN && absolute & 3 == 0 {
                                (mapping.length - relative)
                                    .min(64 - (absolute & 63))
                                    .min(64 - (address as u32 & 63))
                            } else {
                                4
                            };
                            return Some((absolute, bytes));
                        }
                        return None;
                    }
                }
            }
            let mut offsets = [0; 8];
            let offsets = &mut offsets[..bytes.len()];
            self.offsets(address, offsets, index);
            layout.access(offsets, false, self.shared_rmw.as_ref(), |_| {
                gather(&layout, offsets, bytes);
                Some(())
            });
            return None;
        }
        self.pieces(address, bytes.len(), index, |absolute, done, count| {
            let offsets = std::array::from_fn::<_, 8, _>(|i| lane(absolute) + i as u32);
            let start = (absolute & 7) as usize;
            let offsets = &offsets[start..start + count];
            layout.access(offsets, false, self.shared_rmw.as_ref(), |_| {
                gather(&layout, offsets, &mut bytes[done..done + count]);
                Some(())
            });
        });
        None
    }

    pub(in crate::memory) fn shared_write(&mut self, address: u64, bytes: &[u8], index: usize) {
        let Some(layout) = self.shared else {
            unreachable!()
        };
        if bytes.len() <= 8 && (address & 63) + bytes.len() as u64 <= 64 {
            let mut offsets = [0; 8];
            let offsets = &mut offsets[..bytes.len()];
            self.offsets(address, offsets, index);
            layout.access(offsets, true, self.shared_rmw.as_ref(), |held| {
                scatter(&layout, offsets, bytes, held).then_some(())
            });
        } else {
            self.pieces(address, bytes.len(), index, |absolute, done, count| {
                let offsets = std::array::from_fn::<_, 8, _>(|i| lane(absolute) + i as u32);
                let start = (absolute & 7) as usize;
                let offsets = &offsets[start..start + count];
                layout.access(offsets, true, self.shared_rmw.as_ref(), |held| {
                    scatter(&layout, offsets, &bytes[done..done + count], held).then_some(())
                });
            });
        }
        // Local translated owners need the same invalidation as before, even for identity writes.
        let mut current = u128::from(address);
        let last = current + bytes.len() as u128;
        let mut position = index;
        while current < last {
            let mapping = &self.mappings[position];
            let backing = &self.backings[mapping.backing];
            let offset = mapping.offset + (current as u64 - mapping.base) as u32;
            let count = (last.min(mapping.end()) - current) as u32;
            if backing.has_sources {
                self.validity.write(&backing.incarnation, offset, count);
            }
            current += u128::from(count);
            position += 1;
        }
    }
}

impl Image {
    pub(crate) fn mapped_shared(
        backings: Vec<(String, u32, u32, u64)>,
        mappings: Vec<super::super::MappingInput>,
        limits: super::super::MemoryLimits,
        physical: bool,
        [ram_base, ram_capacity, guard_base, watch_base]: [u32; 4],
    ) -> Result<Self, MemoryError> {
        let mut image = if physical {
            Self::physical_mapped(Vec::new(), Vec::new(), limits)?
        } else {
            Self::mapped(Vec::new(), Vec::new(), limits)?
        };
        image.bind_shared_layout(ram_base, ram_capacity, guard_base, watch_base, &[])?;
        for (id, length, offset, incarnation) in backings {
            image.add_shared_backing(id, length, offset, incarnation)?;
        }
        image.set_mappings(mappings)?;
        Ok(image)
    }
}
