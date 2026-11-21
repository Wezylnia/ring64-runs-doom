use super::{Image, MemoryError};

#[derive(Clone, Copy, Debug)]
pub(crate) struct MmioRange {
    pub(crate) base: u64,
    pub(crate) length: u32,
}

pub(crate) enum MmioOperand {
    Ram(u64),
    Device(u64),
}

impl MmioRange {
    fn end(self) -> u128 {
        u128::from(self.base) + u128::from(self.length)
    }

    fn overlaps(self, base: u64, length: u32) -> bool {
        u128::from(base) < self.end()
            && u128::from(self.base) < u128::from(base) + u128::from(length)
    }
}

impl Image {
    pub(crate) fn install_mmio(&mut self, mut ranges: Vec<MmioRange>) -> Result<(), MemoryError> {
        if ranges.is_empty()
            || ranges.iter().any(|range| {
                range.length == 0 || self.mapping_span_failure(range.base, u64::from(range.length))
            })
        {
            return Err(MemoryError::Invalid(
                "MMIO ranges must be nonempty valid guest spans",
            ));
        }
        if !self.mmio.is_empty() {
            return Err(MemoryError::Invalid("MMIO ranges already installed"));
        }
        if ranges.len() > self.mapping_capacity() - self.mappings.len() {
            return Err(MemoryError::Capacity("mapping capacity exceeded"));
        }
        ranges.sort_unstable_by_key(|range| range.base);
        if ranges
            .windows(2)
            .any(|pair| pair[0].overlaps(pair[1].base, pair[1].length))
        {
            return Err(MemoryError::Invalid("MMIO ranges overlap"));
        }
        if self
            .mappings
            .iter()
            .any(|mapping| overlaps(&ranges, mapping.base, mapping.length))
        {
            return Err(MemoryError::Invalid("RAM mapping overlaps MMIO"));
        }
        self.mmio = ranges;
        Ok(())
    }

    pub(crate) fn has_mmio(&self) -> bool {
        !self.mmio.is_empty()
    }

    pub(super) fn overlaps_mmio(&self, base: u64, length: u32) -> bool {
        overlaps(&self.mmio, base, length)
    }

    pub(crate) fn scalar_mmio(&self, address: u64, width: u32) -> bool {
        if !matches!(width, 1 | 2 | 4 | 8) || address & u64::from(width - 1) != 0 {
            return false;
        }
        let count = self.mmio.partition_point(|range| range.base <= address);
        count != 0 && u128::from(address) + u128::from(width) <= self.mmio[count - 1].end()
    }
}

fn overlaps(ranges: &[MmioRange], base: u64, length: u32) -> bool {
    let index = ranges.partition_point(|range| range.end() <= u128::from(base));
    ranges
        .get(index)
        .is_some_and(|range| range.overlaps(base, length))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{MappingInput, MemoryLimits, Permissions};

    fn ram(base: u64) -> MappingInput {
        MappingInput {
            base,
            backing: "data".into(),
            offset: 0,
            length: 8,
            permissions: Permissions {
                read: true,
                write: true,
                execute: true,
            },
        }
    }

    #[test]
    fn mmio_is_reserved_without_becoming_ram() {
        let mut image = Image::mapped(
            vec![("data".into(), vec![0x5a; 8])],
            vec![ram(0x1000)],
            MemoryLimits {
                max_backing_bytes: 8,
                max_backings: 1,
                max_mappings: 3,
            },
        )
        .unwrap();
        image
            .install_mmio(vec![
                MmioRange {
                    base: 0x2000,
                    length: 6,
                },
                MmioRange {
                    base: 0x2006,
                    length: 2,
                },
            ])
            .unwrap();
        assert!(image.scalar_mmio(0x2000, 4));
        assert!(image.scalar_mmio(0x2004, 2));
        assert!(!image.scalar_mmio(0x2004, 4));
        assert!(!image.scalar_mmio(0x2001, 2));
        assert!(image.read(0x2000, 4).is_err());
        assert!(image.fetch(0x2000).is_err());
        assert!(image.set_mappings(vec![ram(0x2000)]).is_err());
        assert_eq!(image.read(0x1000, 1), Ok(0x5a));
    }
}
