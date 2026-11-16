use crate::memory::Access;

/// Natural alignment belongs to the architectural operation, not its byte storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intent {
    Exempt,
    Natural(u32),
    /// A GDTR/IDTR record has a word limit followed by a dword or qword base.
    SystemRecord(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fault {
    pub address: u64,
    pub width: u32,
    pub access: Access,
    pub alignment: u32,
}

impl Intent {
    /// Scalar SIMD operands retain their width; permitted unaligned m128 is exempt.
    /// Mandatory m128 alignment is admitted separately before this policy.
    pub fn vector(width: u32) -> Self {
        match width {
            2 | 4 | 8 => Self::Natural(width),
            16 => Self::Exempt,
            _ => panic!("unsupported scalar/unaligned vector operand width"),
        }
    }

    pub(super) fn check(self, address: u64, width: u32, access: Access) -> Result<(), Fault> {
        let check = |address, width, alignment| {
            assert!(
                matches!(alignment, 1 | 2 | 4 | 8),
                "natural alignment granule"
            );
            if address & u64::from(alignment - 1) == 0 {
                Ok(())
            } else {
                Err(Fault {
                    address,
                    width,
                    access,
                    alignment,
                })
            }
        };
        match self {
            Self::Exempt => Ok(()),
            Self::Natural(alignment) => check(address, width, alignment),
            Self::SystemRecord(base) => {
                assert!(
                    matches!(base, 4 | 8) && width == base + 2,
                    "system record format"
                );
                check(address, 2, 2)?;
                check(address + 2, base, base)
            }
        }
    }
}
