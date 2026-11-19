use std::cell::RefCell;

use super::{
    State,
    cache::Cache,
    walk::{self, AccessClass, PhysicalPhase, Request},
};
use crate::memory::{Access, Image, PreparedPhysicalUpdate, PreparedPhysicalWrite, check_span};
use crate::x86::{
    access::{GuestMemory, PublicationFailure},
    alignment::Intent,
    context::AccessFailure,
    protection_keys::State as ProtectionKeys,
};

#[derive(Debug)]
struct Fragment {
    offset: u32,
    length: u32,
    request: Request,
    physical: u64,
    leaf: u64,
    dirty: bool,
    entry_width: u32,
    cache_id: Option<u64>,
    write: Option<PreparedPhysicalWrite>,
}

#[derive(Debug)]
struct Prepared {
    address: u64,
    width: u32,
    fragments: Vec<Fragment>,
    published: bool,
    fields: Vec<(u32, u32)>,
    #[cfg(all(feature = "parallel", any(target_arch = "wasm32", test)))]
    _dirty_grant: Option<crate::memory::SharedExclusive>,
}

#[derive(Debug, Default)]
pub struct AccessState {
    pending: Vec<Prepared>,
}

impl AccessState {
    pub fn clear(&mut self) {
        self.pending.clear();
    }
}

struct Storage<'a> {
    image: &'a mut Image,
    work: &'a mut AccessState,
}

pub struct Memory<'a> {
    storage: RefCell<Storage<'a>>,
    paging: &'a State,
    cache: Option<&'a Cache>,
    class: AccessClass,
    raw_ac: bool,
    alignment_controls: bool,
    keys: &'a ProtectionKeys,
}

impl<'a> Memory<'a> {
    pub fn new(
        image: &'a mut Image,
        paging: &'a State,
        cpl: u8,
        flags: u64,
        work: &'a mut AccessState,
        keys: &'a ProtectionKeys,
    ) -> Self {
        let ac = flags & (1 << 18) != 0;
        Self {
            storage: RefCell::new(Storage { image, work }),
            paging,
            cache: None,
            class: AccessClass::operand(cpl, ac),
            raw_ac: ac,
            alignment_controls: ac && paging.cr0 & 0x40001 == 0x40001,
            keys,
        }
    }

    pub fn with_cache(mut self, cache: &'a Cache) -> Self {
        self.cache = (self.paging.cr0 & (1 << 31) != 0).then_some(cache);
        self
    }

    #[inline(always)]
    fn translate(
        &self,
        image: &mut Image,
        request: Request,
    ) -> Result<walk::Translation, AccessFailure> {
        if let Some(cache) = self.cache {
            match cache.lookup(self.paging, request, self.keys) {
                Ok(Some(translation)) => return Ok(translation),
                Ok(None) => {}
                Err(failure) => return Err(AccessFailure::Paging(failure)),
            }
        }
        walk::walk(image, self.paging, request, self.keys).map_err(|failure| {
            if let Some(cache) = self.cache {
                cache.fault(self.paging, failure);
            }
            AccessFailure::Paging(failure)
        })
    }

    fn mark_dirty(&self, prepared: &Prepared, fields: &[(u32, u32)]) {
        if let Some(cache) = self.cache {
            for fragment in &prepared.fragments {
                if fragment.dirty && participates(fragment, fields) {
                    cache.mark_dirty(fragment.cache_id, fragment.leaf);
                }
            }
        }
    }

    #[cfg(all(feature = "parallel", any(target_arch = "wasm32", test)))]
    fn shared_dirty_walk_needed(&self, address: u64, width: u32, class: AccessClass) -> bool {
        let Some(cache) = self.cache else {
            return width != 0;
        };
        let mut offset = 0;
        while offset < width {
            let request = Request {
                linear: address + u64::from(offset),
                access: Access::Write,
                class,
            };
            match cache.observe(self.paging, request, self.keys) {
                Ok(Some(translation)) => {
                    offset += u64::from(width - offset).min(translation.remaining) as u32;
                }
                Ok(None) => return true,
                Err(_) => return false,
            }
        }
        false
    }

    fn prepare(
        &self,
        address: u64,
        width: u32,
        access: Access,
        rmw: bool,
        class: AccessClass,
        intent: Intent,
    ) -> Result<Prepared, AccessFailure> {
        check_span(address, width, access).map_err(AccessFailure::Linear)?;
        let mut prepared = Prepared {
            address,
            width,
            fragments: Vec::new(),
            published: false,
            #[cfg(all(feature = "parallel", any(target_arch = "wasm32", test)))]
            _dirty_grant: if access == Access::Write
                && self.paging.cr0 & (1 << 31) != 0
                && self.storage.borrow().image.parallel_attached()
                && self.shared_dirty_walk_needed(address, width, class)
            {
                self.storage.borrow().image.shared_exclusive()
            } else {
                None
            },
            fields: if access == Access::Write {
                vec![(0, width)]
            } else {
                Vec::new()
            },
        };
        let mut candidates = Vec::new();
        let mut offset = 0;
        let mut storage = self.storage.borrow_mut();
        while offset < width {
            let request = Request {
                linear: address + u64::from(offset),
                access,
                class,
            };
            let translation = if self.paging.cr0 & (1 << 31) == 0 {
                None
            } else {
                Some(self.translate(storage.image, request)?)
            };
            let (physical, remaining, leaf, dirty, entry_width, cache_id) = match translation {
                Some(translation) => (
                    translation.physical,
                    translation.remaining,
                    translation.leaf,
                    translation.dirty,
                    translation.entry_width,
                    translation.cache_id,
                ),
                None => {
                    let physical = request.unpaged_physical().map_err(AccessFailure::Paging)?;
                    (physical, (1u64 << 32) - physical, 0, false, 0, None)
                }
            };
            let length = u64::from(width - offset).min(remaining) as u32;
            let write = if access == Access::Write {
                storage
                    .image
                    .prepare_physical_write(physical, length, rmw)
                    .map(Some)
            } else {
                storage
                    .image
                    .physical_check_access(physical, length, access, rmw)
                    .map(|_| None)
            }
            .map_err(|f| {
                AccessFailure::Paging(request.physical_failure(f, PhysicalPhase::FinalAccess))
            })?;
            if self.cache.is_some()
                && let Some(translation) = translation
                && translation.cache_id.is_none()
            {
                candidates.push((prepared.fragments.len(), translation));
            }
            if length == width {
                // The whole access fits this translation. Avoid Vec's spare fragment slots.
                prepared.fragments.reserve_exact(1);
            }
            prepared.fragments.push(Fragment {
                offset,
                length,
                request,
                physical,
                leaf,
                dirty,
                entry_width,
                cache_id,
                write,
            });
            offset += length;
        }
        drop(storage);
        if self.alignment_controls
            && class.is_user()
            && let Err(fault) = intent.check(address, width, access)
        {
            self.storage.borrow_mut().work.clear();
            return Err(AccessFailure::AlignmentCheck(fault));
        }
        if let Some(cache) = self.cache {
            for (index, translation) in candidates {
                let fragment = &mut prepared.fragments[index];
                fragment.cache_id = cache
                    .fill(self.paging, fragment.request, translation)
                    .cache_id;
            }
        }
        Ok(prepared)
    }

    #[inline(always)]
    fn prepare_aligned(
        &self,
        address: u64,
        width: u32,
        access: Access,
        rmw: bool,
        class: AccessClass,
        intent: Intent,
    ) -> Result<Prepared, AccessFailure> {
        self.prepare(address, width, access, rmw, class, intent)
    }

    fn read_prepared(&self, prepared: &Prepared, output: &mut [u8]) {
        let storage = self.storage.borrow();
        for fragment in &prepared.fragments {
            let bytes = storage
                .image
                .physical_read_bytes(fragment.physical, fragment.length)
                .expect("captured physical read admission");
            output[fragment.offset as usize..(fragment.offset + fragment.length) as usize]
                .copy_from_slice(&bytes);
        }
    }

    fn read_rmw_into(
        &self,
        address: u64,
        width: u32,
        intent: Intent,
        output: &mut [u8],
    ) -> Result<(), AccessFailure> {
        let prepared =
            self.prepare_aligned(address, width, Access::Write, true, self.class, intent)?;
        self.read_prepared(&prepared, output);
        self.storage.borrow_mut().work.pending.push(prepared);
        Ok(())
    }

    fn read_at(
        &self,
        address: u64,
        width: u32,
        class: AccessClass,
        intent: Intent,
    ) -> Result<[u8; 16], AccessFailure> {
        assert!(width <= 16);
        let prepared = self.prepare_aligned(address, width, Access::Read, false, class, intent)?;
        let mut bytes = [0; 16];
        self.read_prepared(&prepared, &mut bytes);
        Ok(bytes)
    }

    fn read_fields_at(
        &self,
        address: u64,
        width: u32,
        fields: &[(u32, u32)],
        output: &mut [u8],
        intent: Intent,
    ) -> Result<(), AccessFailure> {
        let prepared =
            self.prepare_aligned(address, width, Access::Read, false, self.class, intent)?;
        let storage = self.storage.borrow();
        for fragment in &prepared.fragments {
            for &(start, length) in fields {
                let begin = start.max(fragment.offset);
                let end = (start + length).min(fragment.offset + fragment.length);
                if begin < end {
                    let bytes = storage
                        .image
                        .physical_read_bytes(
                            fragment.physical + u64::from(begin - fragment.offset),
                            end - begin,
                        )
                        .expect("admitted physical field read");
                    output[begin as usize..end as usize].copy_from_slice(&bytes);
                }
            }
        }
        Ok(())
    }

    fn capture_write(
        &self,
        address: u64,
        width: u32,
        class: AccessClass,
        intent: Intent,
    ) -> Result<(), AccessFailure> {
        self.capture_write_fields(address, width, &[(0, width)], class, intent)
    }

    fn capture_write_fields(
        &self,
        address: u64,
        width: u32,
        fields: &[(u32, u32)],
        class: AccessClass,
        intent: Intent,
    ) -> Result<(), AccessFailure> {
        let mut prepared =
            self.prepare_aligned(address, width, Access::Write, false, class, intent)?;
        if prepared.fields.as_slice() != fields {
            prepared.fields = fields.to_vec();
        }
        self.storage.borrow_mut().work.pending.push(prepared);
        Ok(())
    }

    fn dirty_admission(
        &self,
        prepared: &Prepared,
        fields: &[(u32, u32)],
    ) -> Result<Vec<PreparedPhysicalUpdate>, AccessFailure> {
        let storage = self.storage.borrow();
        let mut updates = Vec::new();
        for fragment in &prepared.fragments {
            if fragment.dirty && participates(fragment, fields) {
                let update = storage
                    .image
                    .prepare_physical_update(fragment.leaf, fragment.entry_width)
                    .map_err(|f| {
                        AccessFailure::Paging(
                            fragment
                                .request
                                .physical_failure(f, PhysicalPhase::DirtyWrite),
                        )
                    })?;
                updates.push(update);
            }
        }
        Ok(updates)
    }

    fn dirty_publish(&self, updates: &[PreparedPhysicalUpdate]) {
        let mut storage = self.storage.borrow_mut();
        for update in updates {
            storage.image.publish_physical_update(update, walk::DIRTY);
        }
    }

    fn write_at(
        &self,
        address: u64,
        width: u32,
        fields: &[(u32, u32)],
        bytes: &[u8],
        class: AccessClass,
        intent: Intent,
    ) -> Result<(), AccessFailure> {
        let captured = {
            let mut storage = self.storage.borrow_mut();
            let index = storage
                .work
                .pending
                .iter()
                .position(|p| p.address == address && p.width == width);
            index.map(|i| storage.work.pending.remove(i))
        };
        let prepared = match captured {
            Some(p) => p,
            None => self.prepare_aligned(address, width, Access::Write, false, class, intent)?,
        };
        if !prepared.published {
            let updates = self.dirty_admission(&prepared, fields)?;
            self.dirty_publish(&updates);
            self.mark_dirty(&prepared, fields);
        }
        let mut storage = self.storage.borrow_mut();
        for fragment in &prepared.fragments {
            for &(start, length) in fields {
                let begin = start.max(fragment.offset);
                let end = (start + length).min(fragment.offset + fragment.length);
                if begin < end {
                    storage.image.publish_physical(
                        fragment
                            .write
                            .as_ref()
                            .expect("captured physical payload admission"),
                        begin - fragment.offset,
                        &bytes[begin as usize..end as usize],
                    );
                }
            }
        }
        Ok(())
    }
}

fn participates(fragment: &Fragment, fields: &[(u32, u32)]) -> bool {
    fields.iter().any(|&(start, length)| {
        start < fragment.offset + fragment.length && fragment.offset < start + length
    })
}

impl GuestMemory for Memory<'_> {
    fn fetch(&self, address: u64) -> Result<u8, AccessFailure> {
        let prepared = self.prepare_aligned(
            address,
            1,
            Access::Execute,
            false,
            self.class,
            Intent::Exempt,
        )?;
        let storage = self.storage.borrow();
        Ok(storage
            .image
            .physical_fetch(prepared.fragments[0].physical)
            .expect("admitted physical fetch"))
    }
    fn read(&self, address: u64, width: u32) -> Result<u64, AccessFailure> {
        let bytes = self.read_at(address, width, self.class, Intent::Exempt)?;
        Ok(u64::from_le_bytes(bytes[..8].try_into().unwrap()))
    }
    fn read_for_write(&self, address: u64, width: u32) -> Result<u64, AccessFailure> {
        self.read_for_write_aligned(address, width, Intent::Exempt)
    }
    fn read_block16_for_write_aligned(
        &self,
        address: u64,
        output: &mut [u8; 16],
    ) -> Result<(), AccessFailure> {
        self.read_rmw_into(address, 16, Intent::Exempt, output)
    }
    fn read_vector(&self, address: u64, width: u32) -> Result<[u8; 16], AccessFailure> {
        self.read_at(address, width, self.class, Intent::Exempt)
    }
    fn read_fields(
        &self,
        address: u64,
        width: u32,
        fields: &[(u32, u32)],
        output: &mut [u8],
    ) -> Result<(), AccessFailure> {
        self.read_fields_at(address, width, fields, output, Intent::Exempt)
    }
    fn write(&mut self, address: u64, width: u32, value: u64) -> Result<(), AccessFailure> {
        self.write_at(
            address,
            width,
            &[(0, width)],
            &value.to_le_bytes(),
            self.class,
            Intent::Exempt,
        )
    }
    fn write_bytes(&mut self, address: u64, bytes: &[u8]) -> Result<(), AccessFailure> {
        self.write_at(
            address,
            bytes.len() as u32,
            &[(0, bytes.len() as u32)],
            bytes,
            self.class,
            Intent::Exempt,
        )
    }
    fn write_fields(
        &mut self,
        address: u64,
        width: u32,
        fields: &[(u32, u32)],
        bytes: &[u8],
    ) -> Result<(), AccessFailure> {
        self.write_at(address, width, fields, bytes, self.class, Intent::Exempt)
    }
    fn check_read(&self, address: u64, width: u32) -> Result<(), AccessFailure> {
        self.prepare_aligned(
            address,
            width,
            Access::Read,
            false,
            self.class,
            Intent::Exempt,
        )
        .map(|_| ())
    }
    fn check_write(&self, address: u64, width: u32) -> Result<(), AccessFailure> {
        self.capture_write(address, width, self.class, Intent::Exempt)
    }
    fn read_aligned(&self, address: u64, width: u32, intent: Intent) -> Result<u64, AccessFailure> {
        let bytes = self.read_at(address, width, self.class, intent)?;
        Ok(u64::from_le_bytes(bytes[..8].try_into().unwrap()))
    }
    fn read_for_write_aligned(
        &self,
        address: u64,
        width: u32,
        intent: Intent,
    ) -> Result<u64, AccessFailure> {
        let mut bytes = [0; 8];
        self.read_rmw_into(address, width, intent, &mut bytes)?;
        Ok(u64::from_le_bytes(bytes))
    }
    fn read_vector_aligned(
        &self,
        address: u64,
        width: u32,
        intent: Intent,
    ) -> Result<[u8; 16], AccessFailure> {
        self.read_at(address, width, self.class, intent)
    }
    fn read_fields_aligned(
        &self,
        address: u64,
        width: u32,
        fields: &[(u32, u32)],
        output: &mut [u8],
        intent: Intent,
    ) -> Result<(), AccessFailure> {
        self.read_fields_at(address, width, fields, output, intent)
    }
    fn write_aligned(
        &mut self,
        address: u64,
        width: u32,
        value: u64,
        intent: Intent,
    ) -> Result<(), AccessFailure> {
        self.write_at(
            address,
            width,
            &[(0, width)],
            &value.to_le_bytes(),
            self.class,
            intent,
        )
    }
    fn write_bytes_aligned(
        &mut self,
        address: u64,
        bytes: &[u8],
        intent: Intent,
    ) -> Result<(), AccessFailure> {
        self.write_at(
            address,
            bytes.len() as u32,
            &[(0, bytes.len() as u32)],
            bytes,
            self.class,
            intent,
        )
    }
    fn write_fields_aligned(
        &mut self,
        address: u64,
        width: u32,
        fields: &[(u32, u32)],
        bytes: &[u8],
        intent: Intent,
    ) -> Result<(), AccessFailure> {
        self.write_at(address, width, fields, bytes, self.class, intent)
    }
    fn check_read_aligned(
        &self,
        address: u64,
        width: u32,
        intent: Intent,
    ) -> Result<(), AccessFailure> {
        self.prepare_aligned(address, width, Access::Read, false, self.class, intent)
            .map(|_| ())
    }
    fn check_write_aligned(
        &self,
        address: u64,
        width: u32,
        intent: Intent,
    ) -> Result<(), AccessFailure> {
        self.capture_write(address, width, self.class, intent)
    }
    fn probe_write(&self, address: u64) -> Result<(), AccessFailure> {
        check_span(address, 1, Access::Write).map_err(AccessFailure::Linear)?;
        let request = Request {
            linear: address,
            access: Access::Write,
            class: self.class,
        };
        let mut storage = self.storage.borrow_mut();
        let translation = if self.paging.cr0 & (1 << 31) == 0 {
            None
        } else {
            Some(self.translate(storage.image, request)?)
        };
        let physical = match translation {
            Some(translation) => translation.physical,
            None => request.unpaged_physical().map_err(AccessFailure::Paging)?,
        };
        storage
            .image
            .physical_check_write(physical, 1)
            .map_err(|fault| {
                AccessFailure::Paging(request.physical_failure(fault, PhysicalPhase::FinalAccess))
            })?;
        if let (Some(cache), Some(translation)) = (self.cache, translation) {
            cache.fill(self.paging, request, translation);
        }
        Ok(())
    }
    fn read_system(&self, address: u64, width: u32) -> Result<[u8; 16], AccessFailure> {
        self.read_at(
            address,
            width,
            AccessClass::ImplicitSupervisor,
            Intent::Exempt,
        )
    }
    fn check_system_write(&self, address: u64, width: u32) -> Result<(), AccessFailure> {
        self.capture_write(
            address,
            width,
            AccessClass::ImplicitSupervisor,
            Intent::Exempt,
        )
    }
    fn check_system_write_fields(
        &self,
        address: u64,
        width: u32,
        fields: &[(u32, u32)],
    ) -> Result<(), AccessFailure> {
        self.capture_write_fields(
            address,
            width,
            fields,
            AccessClass::ImplicitSupervisor,
            Intent::Exempt,
        )
    }
    fn write_system(&mut self, address: u64, width: u32, value: u64) -> Result<(), AccessFailure> {
        self.write_at(
            address,
            width,
            &[(0, width)],
            &value.to_le_bytes(),
            AccessClass::ImplicitSupervisor,
            Intent::Exempt,
        )
    }
    fn publish_system_byte(
        &mut self,
        address: u64,
        width: u32,
        offset: u32,
        byte: u8,
    ) -> Result<(), AccessFailure> {
        let mut bytes = [0; 16];
        bytes[offset as usize] = byte;
        self.write_at(
            address,
            width,
            &[(offset, 1)],
            &bytes,
            AccessClass::ImplicitSupervisor,
            Intent::Exempt,
        )
    }
    fn check_frame_write(&self, address: u64, width: u32, cpl: u8) -> Result<(), AccessFailure> {
        self.capture_write(
            address,
            width,
            AccessClass::operand(cpl, self.raw_ac),
            Intent::Exempt,
        )
    }
    fn check_frame_write_aligned(
        &self,
        address: u64,
        width: u32,
        cpl: u8,
        intent: Intent,
    ) -> Result<(), AccessFailure> {
        self.capture_write(
            address,
            width,
            AccessClass::operand(cpl, self.raw_ac),
            intent,
        )
    }
    fn prepare_publication(&mut self) -> Result<(), AccessFailure> {
        self.prepare_publication_ordered()
            .map_err(|fault| fault.failure)
    }
    fn prepare_publication_ordered(&mut self) -> Result<(), PublicationFailure> {
        let pending = std::mem::take(&mut self.storage.borrow_mut().work.pending);
        let mut updates = Vec::new();
        for (index, prepared) in pending.iter().enumerate() {
            updates.extend(self.dirty_admission(prepared, &prepared.fields).map_err(
                |failure| PublicationFailure {
                    index: Some(index),
                    failure,
                },
            )?);
        }
        self.dirty_publish(&updates);
        for prepared in &pending {
            self.mark_dirty(prepared, &prepared.fields);
        }
        self.storage.borrow_mut().work.pending = pending
            .into_iter()
            .map(|mut p| {
                p.published = true;
                p
            })
            .collect();
        Ok(())
    }
}

#[cfg(test)]
#[path = "access_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "alignment_tests.rs"]
mod alignment_tests;

#[cfg(test)]
#[path = "retained_tests.rs"]
mod retained_tests;

#[cfg(test)]
#[path = "pg0_prepared_tests.rs"]
mod pg0_prepared_tests;
