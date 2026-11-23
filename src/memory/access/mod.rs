use super::address::{check_physical_span, check_span};
use super::image::locate_index;
use super::{Access, Cause, Fault, Image};

mod prepared;
pub(crate) use prepared::{PreparedPhysicalUpdate, PreparedPhysicalWrite};

#[inline]
fn copy_field(target: &mut [u8], source: &[u8]) {
    match target.len() {
        1 => target[0] = source[0],
        2 => target[..2].copy_from_slice(&source[..2]),
        4 => target[..4].copy_from_slice(&source[..4]),
        8 => target[..8].copy_from_slice(&source[..8]),
        10 => target[..10].copy_from_slice(&source[..10]),
        16 => target[..16].copy_from_slice(&source[..16]),
        _ => target.copy_from_slice(source),
    }
}

impl Image {
    pub fn physical_fetch(&self, address: u64) -> Result<u8, Fault> {
        let first = self.preflight::<true>(address, 1, Access::Execute, false, false)?;
        Ok(self.byte_after_preflight(address, first))
    }

    pub fn physical_read(&self, address: u64, width: u32) -> Result<u64, Fault> {
        assert!(
            matches!(width, 1 | 2 | 4 | 8),
            "unsupported scalar access width"
        );
        let first = self.preflight::<true>(address, width, Access::Read, false, true)?;
        Ok(self.scalar_after_preflight(address, width, first))
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    pub(crate) fn probe_physical_read<T>(
        &self,
        probe: impl FnOnce(&Self) -> Option<T>,
    ) -> Option<T> {
        let last = self.last_mapping.get();
        let previous = self.previous_mapping.get();
        let result = probe(self);
        if result.is_none() {
            self.last_mapping.set(last);
            self.previous_mapping.set(previous);
        }
        result
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    #[inline(always)]
    pub(crate) fn physical_read_mapped_byte(&self, address: u64) -> Option<u8> {
        if !self.physical {
            return None;
        }
        let index = self.locate_index(address)?;
        // The validated mapping bounds both the physical address and the byte
        // in RAM. The checked caller handles missing and inaccessible mappings.
        self.mappings[index]
            .permissions
            .read
            .then(|| self.byte_after_preflight(address, index))
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    #[inline(always)]
    pub(crate) fn physical_read_mapped_dword(&self, address: u64) -> Option<u32> {
        self.physical_read_mapped_array(address)
            .map(u32::from_le_bytes)
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    #[inline(always)]
    pub(crate) fn physical_read_mapped_qword(&self, address: u64) -> Option<u64> {
        self.physical_read_mapped_array(address)
            .map(u64::from_le_bytes)
    }

    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    #[inline(always)]
    fn physical_read_mapped_array<const N: usize>(&self, address: u64) -> Option<[u8; N]> {
        if !self.physical || address > super::address::PHYSICAL_ADDRESS_END - N as u64 {
            return None;
        }
        let mapping = &self.mappings[self.locate_index(address)?];
        let relative = address - mapping.base;
        if !mapping.permissions.read || u64::from(mapping.length) - relative < N as u64 {
            return None;
        }
        // The validated physical mapping covers every byte of this read.
        let offset = mapping.offset as usize + relative as usize;
        let bytes = &self.backings[mapping.backing].bytes;
        Some(bytes[offset..offset + N].try_into().unwrap())
    }

    pub fn physical_read_for_write(&self, address: u64, width: u32) -> Result<u64, Fault> {
        assert!(
            matches!(width, 1 | 2 | 4 | 8),
            "unsupported scalar access width"
        );
        let first = self.preflight::<true>(address, width, Access::Write, true, true)?;
        Ok(self.scalar_after_preflight(address, width, first))
    }

    pub fn physical_write(&mut self, address: u64, width: u32, value: u64) -> Result<(), Fault> {
        assert!(
            matches!(width, 1 | 2 | 4 | 8),
            "unsupported scalar access width"
        );
        let first = self.preflight::<true>(address, width, Access::Write, false, true)?;
        self.write_after_preflight(address, &value.to_le_bytes()[..width as usize], first);
        Ok(())
    }

    pub fn physical_read_bytes(&self, address: u64, length: u32) -> Result<Vec<u8>, Fault> {
        let first = self.preflight::<true>(address, length, Access::Read, false, false)?;
        Ok(self.bytes_after_preflight(address, length, first))
    }

    pub(crate) fn physical_read_into(&self, address: u64, output: &mut [u8]) -> Result<(), Fault> {
        let width = u32::try_from(output.len()).expect("physical read length exceeds wasm32");
        self.read_fields_profile::<true>(address, width, &[(0, width)], output)
    }

    pub fn physical_write_bytes(&mut self, address: u64, bytes: &[u8]) -> Result<(), Fault> {
        let width = u32::try_from(bytes.len()).expect("memory copy length exceeds wasm32");
        let first = self.preflight::<true>(address, width, Access::Write, false, false)?;
        self.write_after_preflight(address, bytes, first);
        Ok(())
    }

    pub fn physical_check_read(&self, address: u64, width: u32) -> Result<(), Fault> {
        self.preflight::<true>(address, width, Access::Read, false, false)
            .map(|_| ())
    }

    pub fn physical_check_write(&self, address: u64, width: u32) -> Result<(), Fault> {
        self.preflight::<true>(address, width, Access::Write, false, false)
            .map(|_| ())
    }

    pub fn physical_check_execute(&self, address: u64, width: u32) -> Result<(), Fault> {
        self.preflight::<true>(address, width, Access::Execute, false, false)
            .map(|_| ())
    }

    pub fn physical_check_access(
        &self,
        address: u64,
        width: u32,
        access: Access,
        rmw: bool,
    ) -> Result<(), Fault> {
        self.preflight::<true>(address, width, access, rmw, false)
            .map(|_| ())
    }

    pub fn fetch(&self, address: u64) -> Result<u8, Fault> {
        let index = self.preflight::<false>(address, 1, Access::Execute, false, false)?;
        Ok(self.byte_after_preflight(address, index))
    }

    pub fn read(&self, address: u64, width: u32) -> Result<u64, Fault> {
        assert!(
            matches!(width, 1 | 2 | 4 | 8),
            "unsupported scalar access width"
        );
        if width == 1 {
            let first = self.preflight::<false>(address, 1, Access::Read, false, true)?;
            return Ok(u64::from(self.byte_after_preflight(address, first)));
        }
        let first = self.preflight::<false>(address, width, Access::Read, false, true)?;
        Ok(self.scalar_after_preflight(address, width, first))
    }

    pub fn read_for_write(&self, address: u64, width: u32) -> Result<u64, Fault> {
        assert!(
            matches!(width, 1 | 2 | 4 | 8),
            "unsupported scalar access width"
        );
        if width == 1 {
            let first = self.preflight::<false>(address, 1, Access::Write, true, true)?;
            return Ok(u64::from(self.byte_after_preflight(address, first)));
        }
        let first = self.preflight::<false>(address, width, Access::Write, true, true)?;
        Ok(self.scalar_after_preflight(address, width, first))
    }

    pub fn write(&mut self, address: u64, width: u32, value: u64) -> Result<(), Fault> {
        assert!(
            matches!(width, 1 | 2 | 4 | 8),
            "unsupported scalar access width"
        );
        let first = self.preflight::<false>(address, width, Access::Write, false, true)?;
        #[cfg(all(feature = "parallel", any(target_arch = "wasm32", test)))]
        if self.shared.is_some() {
            self.shared_write(address, &value.to_le_bytes()[..width as usize], first);
            return Ok(());
        }
        let mapping = &self.mappings[first];
        let relative = address - mapping.base;
        if u64::from(width) <= u64::from(mapping.length) - relative {
            let offset = mapping.offset as usize + relative as usize;
            let backing = &mut self.backings[mapping.backing];
            if backing.has_sources {
                self.validity
                    .write(&backing.incarnation, offset as u32, width);
            }
            match width {
                1 => backing.bytes[offset] = value as u8,
                2 => {
                    backing.bytes[offset..offset + 2].copy_from_slice(&(value as u16).to_le_bytes())
                }
                4 => {
                    backing.bytes[offset..offset + 4].copy_from_slice(&(value as u32).to_le_bytes())
                }
                8 => backing.bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes()),
                _ => unreachable!(),
            }
        } else {
            self.write_after_preflight(address, &value.to_le_bytes()[..width as usize], first);
        }
        Ok(())
    }

    pub fn read_bytes(&self, address: u64, length: u32) -> Result<Vec<u8>, Fault> {
        let first = self.preflight::<false>(address, length, Access::Read, false, false)?;
        Ok(self.bytes_after_preflight(address, length, first))
    }

    fn bytes_after_preflight(&self, address: u64, length: u32, mut index: usize) -> Vec<u8> {
        #[cfg(all(feature = "parallel", any(target_arch = "wasm32", test)))]
        if self.shared.is_some() {
            let mut bytes = vec![0; length as usize];
            self.shared_read(address, &mut bytes, index);
            return bytes;
        }
        let mut output = Vec::with_capacity(length as usize);
        let mut cursor = u128::from(address);
        let end = cursor + u128::from(length);
        while cursor < end {
            let mapping = &self.mappings[index];
            let offset = mapping.offset as usize + (cursor as u64 - mapping.base) as usize;
            let count = (end.min(mapping.end()) - cursor) as usize;
            output.extend_from_slice(&self.backings[mapping.backing].bytes[offset..offset + count]);
            cursor += count as u128;
            index += 1;
        }
        output
    }

    pub fn read_vector(&self, address: u64, width: u32) -> Result<[u8; 16], Fault> {
        assert!(matches!(width, 2 | 4 | 8 | 10 | 16));
        let first = self.preflight::<false>(address, width, Access::Read, false, false)?;
        Ok(self.vector_after_preflight(address, width, first))
    }

    pub(crate) fn read_block16_for_write(
        &self,
        address: u64,
        output: &mut [u8; 16],
    ) -> Result<(), Fault> {
        let first = self.preflight::<false>(address, 16, Access::Write, true, false)?;
        *output = self.vector_after_preflight(address, 16, first);
        Ok(())
    }

    fn vector_after_preflight(&self, address: u64, width: u32, mut index: usize) -> [u8; 16] {
        #[cfg(all(feature = "parallel", any(target_arch = "wasm32", test)))]
        if self.shared.is_some() {
            let mut bytes = [0; 16];
            self.shared_read(address, &mut bytes[..width as usize], index);
            return bytes;
        }
        let mapping = &self.mappings[index];
        let relative = address - mapping.base;
        if u64::from(width) <= u64::from(mapping.length) - relative {
            let offset = mapping.offset as usize + relative as usize;
            let source = &self.backings[mapping.backing].bytes[offset..];
            if width == 16 {
                return source[..16].try_into().unwrap();
            }
            let mut output = [0; 16];
            match width {
                2 => output[..2].copy_from_slice(&source[..2]),
                4 => output[..4].copy_from_slice(&source[..4]),
                8 => output[..8].copy_from_slice(&source[..8]),
                10 => output[..10].copy_from_slice(&source[..10]),
                _ => unreachable!(),
            }
            return output;
        }
        let mut output = [0; 16];
        self.read_across_mappings(address, &mut output[..width as usize], &mut index);
        output
    }

    pub fn write_bytes(&mut self, address: u64, bytes: &[u8]) -> Result<(), Fault> {
        let width = u32::try_from(bytes.len()).expect("memory copy length exceeds u32");
        let first = self.preflight::<false>(address, width, Access::Write, false, false)?;
        self.write_after_preflight(address, bytes, first);
        Ok(())
    }

    pub fn check_write(&self, address: u64, width: u32) -> Result<(), Fault> {
        self.preflight::<false>(address, width, Access::Write, false, false)
            .map(|_| ())
    }

    pub fn check_read(&self, address: u64, width: u32) -> Result<(), Fault> {
        self.preflight::<false>(address, width, Access::Read, false, false)
            .map(|_| ())
    }

    pub fn read_fields(
        &self,
        address: u64,
        width: u32,
        fields: &[(u32, u32)],
        output: &mut [u8],
    ) -> Result<(), Fault> {
        self.read_fields_profile::<false>(address, width, fields, output)
    }

    fn read_fields_profile<const PHYSICAL: bool>(
        &self,
        address: u64,
        width: u32,
        fields: &[(u32, u32)],
        output: &mut [u8],
    ) -> Result<(), Fault> {
        Self::check_fields(width, fields, output.len());
        let mut index = self.preflight::<PHYSICAL>(address, width, Access::Read, false, false)?;
        #[cfg(all(feature = "parallel", any(target_arch = "wasm32", test)))]
        if self.shared.is_some() {
            for &(start, length) in fields {
                let cursor = address + u64::from(start);
                while self.mappings[index].end() <= u128::from(cursor) {
                    index += 1;
                }
                self.shared_read(
                    cursor,
                    &mut output[start as usize..(start + length) as usize],
                    index,
                );
            }
            return Ok(());
        }
        if fields.is_empty() {
            return Ok(());
        }
        let mapping = &self.mappings[index];
        let relative = address - mapping.base;
        if u64::from(width) <= u64::from(mapping.length) - relative {
            let offset = mapping.offset as usize + relative as usize;
            let backing = &self.backings[mapping.backing].bytes;
            for &(start, length) in fields {
                let begin = offset + start as usize;
                let source = &backing[begin..begin + length as usize];
                let target = &mut output[start as usize..(start + length) as usize];
                copy_field(target, source);
            }
            return Ok(());
        }
        for &(start, length) in fields {
            let cursor = address + u64::from(start);
            while self.mappings[index].end() <= u128::from(cursor) {
                index += 1;
            }
            let target = &mut output[start as usize..(start + length) as usize];
            self.read_across_mappings(cursor, target, &mut index);
        }
        Ok(())
    }

    fn read_across_mappings(&self, mut cursor: u64, target: &mut [u8], index: &mut usize) {
        let mut copied = 0;
        while copied < target.len() {
            let mapping = &self.mappings[*index];
            let relative = (cursor - mapping.base) as usize;
            let offset = mapping.offset as usize + relative;
            let count = (target.len() - copied).min(mapping.length as usize - relative);
            target[copied..copied + count]
                .copy_from_slice(&self.backings[mapping.backing].bytes[offset..offset + count]);
            copied += count;
            if copied < target.len() {
                cursor += count as u64;
                *index += 1;
            }
        }
    }

    pub fn write_fields(
        &mut self,
        address: u64,
        width: u32,
        fields: &[(u32, u32)],
        bytes: &[u8],
    ) -> Result<(), Fault> {
        Self::check_fields(width, fields, bytes.len());
        let mut index = self.preflight::<false>(address, width, Access::Write, false, false)?;
        #[cfg(all(feature = "parallel", any(target_arch = "wasm32", test)))]
        if self.shared.is_some() {
            for &(start, length) in fields {
                let cursor = address + u64::from(start);
                while self.mappings[index].end() <= u128::from(cursor) {
                    index += 1;
                }
                self.shared_write(
                    cursor,
                    &bytes[start as usize..(start + length) as usize],
                    index,
                );
            }
            return Ok(());
        }
        if fields.is_empty() {
            return Ok(());
        }
        let mapping = &self.mappings[index];
        let relative = address - mapping.base;
        if u64::from(width) <= u64::from(mapping.length) - relative {
            let offset = mapping.offset as usize + relative as usize;
            let backing = &mut self.backings[mapping.backing];
            for &(start, length) in fields {
                let begin = offset + start as usize;
                let end = begin + length as usize;
                if backing.has_sources {
                    self.validity
                        .write(&backing.incarnation, begin as u32, length);
                }
                let target = &mut backing.bytes[begin..end];
                let source = &bytes[start as usize..(start + length) as usize];
                copy_field(target, source);
            }
            return Ok(());
        }
        for &(start, length) in fields {
            let cursor = address + u64::from(start);
            while self.mappings[index].end() <= u128::from(cursor) {
                index += 1;
            }
            self.write_after_preflight(
                cursor,
                &bytes[start as usize..(start + length) as usize],
                index,
            );
        }
        Ok(())
    }

    fn check_fields(width: u32, fields: &[(u32, u32)], buffer_length: usize) {
        assert!(width as usize <= buffer_length, "field buffer is too short");
        let mut end = 0;
        for &(start, length) in fields {
            let next = u64::from(start) + u64::from(length);
            assert!(
                length != 0 && start >= end && next <= u64::from(width),
                "fields must be nonempty ordered slices of the nominal operand"
            );
            end = next as u32;
        }
    }

    fn write_after_preflight(&mut self, address: u64, bytes: &[u8], mut index: usize) {
        #[cfg(all(feature = "parallel", any(target_arch = "wasm32", test)))]
        if self.shared.is_some() {
            self.shared_write(address, bytes, index);
            return;
        }
        let mut cursor = address;
        let mut source = 0;
        while source < bytes.len() {
            let mapping = &self.mappings[index];
            let relative = (cursor - mapping.base) as usize;
            let offset = mapping.offset as usize + relative;
            let count = (bytes.len() - source).min(mapping.length as usize - relative);
            let backing = &mut self.backings[mapping.backing];
            if backing.has_sources {
                self.validity
                    .write(&backing.incarnation, offset as u32, count as u32);
            }
            match count {
                8 => backing.bytes[offset..offset + 8].copy_from_slice(&bytes[source..source + 8]),
                16 => {
                    backing.bytes[offset..offset + 16].copy_from_slice(&bytes[source..source + 16])
                }
                _ => backing.bytes[offset..offset + count]
                    .copy_from_slice(&bytes[source..source + count]),
            }
            source += count;
            if source < bytes.len() {
                cursor += count as u64;
                index += 1;
            }
        }
    }

    #[inline]
    pub(super) fn preflight<const PHYSICAL: bool>(
        &self,
        address: u64,
        width: u32,
        access: Access,
        rmw: bool,
        cache_scalar: bool,
    ) -> Result<usize, Fault> {
        #[cfg(test)]
        if PHYSICAL {
            prepared::record_preflight();
        }
        if PHYSICAL {
            check_physical_span(address, width, access)?;
        } else {
            check_span(address, width, access)?;
        }
        if width == 0 {
            return Ok(0);
        }
        let mut cursor = u128::from(address);
        let end = cursor + u128::from(width);
        let failure = |cause, failing| Fault {
            address,
            width,
            access,
            cause,
            failing,
        };
        let first = if cache_scalar {
            self.locate_index(address)
        } else {
            locate_index(&self.mappings, address)
        }
        .ok_or_else(|| failure(Cause::Unmapped, cursor))?;
        let mut index = first;
        loop {
            let mapping = &self.mappings[index];
            let permitted = match access {
                Access::Read => mapping.permissions.read,
                Access::Write => mapping.permissions.write && (!rmw || mapping.permissions.read),
                Access::Execute => mapping.permissions.execute,
            };
            if !permitted {
                return Err(failure(Cause::Permission, cursor));
            }
            if end <= mapping.end() {
                return Ok(first);
            }
            cursor = mapping.end();
            index += 1;
            if self
                .mappings
                .get(index)
                .is_none_or(|mapping| u128::from(mapping.base) != cursor)
            {
                return Err(failure(Cause::Unmapped, cursor));
            }
        }
    }

    #[inline]
    fn byte_after_preflight(&self, address: u64, index: usize) -> u8 {
        #[cfg(all(feature = "parallel", any(target_arch = "wasm32", test)))]
        if self.shared.is_some() {
            let mut byte = [0];
            self.shared_read(address, &mut byte, index);
            return byte[0];
        }
        let mapping = &self.mappings[index];
        let offset = mapping.offset as usize + (address - mapping.base) as usize;
        self.backings[mapping.backing].bytes[offset]
    }

    #[inline]
    fn scalar_after_preflight(&self, address: u64, width: u32, mut index: usize) -> u64 {
        #[cfg(all(feature = "parallel", any(target_arch = "wasm32", test)))]
        if self.shared.is_some() {
            let mut bytes = [0; 8];
            self.shared_read(address, &mut bytes[..width as usize], index);
            return u64::from_le_bytes(bytes);
        }
        let mapping = &self.mappings[index];
        let relative = address - mapping.base;
        let offset = mapping.offset as usize + relative as usize;
        if width == 1 {
            return u64::from(self.backings[mapping.backing].bytes[offset]);
        }
        if u64::from(width) <= u64::from(mapping.length) - relative {
            let bytes = &self.backings[mapping.backing].bytes;
            return match width {
                2 => u64::from(u16::from_le_bytes(
                    bytes[offset..offset + 2].try_into().unwrap(),
                )),
                4 => u64::from(u32::from_le_bytes(
                    bytes[offset..offset + 4].try_into().unwrap(),
                )),
                8 => u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap()),
                _ => unreachable!(),
            };
        }
        let mut bytes = [0u8; 8];
        let mut cursor = u128::from(address);
        let end = cursor + u128::from(width);
        let mut destination = 0;
        while cursor < end {
            let mapping = &self.mappings[index];
            let offset = mapping.offset as usize + (cursor as u64 - mapping.base) as usize;
            let count = (end.min(mapping.end()) - cursor) as usize;
            bytes[destination..destination + count]
                .copy_from_slice(&self.backings[mapping.backing].bytes[offset..offset + count]);
            cursor += count as u128;
            destination += count;
            index += 1;
        }
        u64::from_le_bytes(bytes)
    }
}

#[cfg(test)]
mod tests;
