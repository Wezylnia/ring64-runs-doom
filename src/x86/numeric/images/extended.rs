use super::{Format, ImageOp, X87State, dword, fx, put_dword};
use crate::x86::xstate::{MAX_BYTES, PKRU_OFFSET};

pub const HEADER: u32 = 512;

pub struct ExtendedImage {
    operation: ImageOp,
    requested: u64,
    present: u64,
    pub bytes: [u8; MAX_BYTES],
}

impl ExtendedImage {
    pub fn new(operation: ImageOp, xcr0: u64, eax: u64, edx: u64) -> Self {
        Self {
            operation,
            requested: xcr0 & (u64::from(eax as u32) | (edx << 32)),
            present: 0,
            bytes: [0; MAX_BYTES],
        }
    }

    pub fn header_width(&self) -> u32 {
        if self.operation.is_store() { 8 } else { 24 }
    }

    pub fn accept_header(&mut self, xcr0: u64) -> bool {
        let old = u64::from_le_bytes(self.bytes[512..520].try_into().unwrap());
        if self.operation.is_store() {
            // This processor does not implement the init optimization.
            self.bytes[512..520].copy_from_slice(&(old | self.requested).to_le_bytes());
            self.present = self.requested;
            true
        } else {
            self.present = self.requested & old;
            old & !xcr0 == 0 && self.bytes[520..536].iter().all(|&b| b == 0)
        }
    }

    /// Actual component accesses in the order used by the standard instruction.
    /// The header read is separate and precedes all component accesses.
    pub fn fields(&self) -> impl Iterator<Item = (u32, u32)> + use<> {
        let save = self.operation.is_store();
        let wide = self.operation.format == Format::Fx64Wide;
        let x87 = self.present & 1 != 0;
        let sse = self.present & 2 != 0;
        let mxcsr = self.requested & 2 != 0;
        let xmm = (160, (16 * self.operation.xmm_count()) as u32);
        let x87_pointers = if wide {
            [(0, 5), (6, 18), (0, 0)]
        } else {
            // Deprecated FCS/FDS are written as zero, and ignored on restore.
            let selector = if save { 2 } else { 0 };
            [(0, 5), (6, 6 + selector), (16, 4 + selector)]
        };
        x87_pointers
            .into_iter()
            .filter(move |&(_, length)| x87 && length != 0)
            .chain((0..8).filter(move |_| x87).map(|i| (32 + 16 * i, 10)))
            .chain((save && sse).then_some(xmm))
            .chain(mxcsr.then_some((24, if save { 8 } else { 4 })))
            .chain((!save && sse).then_some(xmm))
            .chain((self.present & (1 << 9) != 0).then_some((PKRU_OFFSET as u32, 4)))
            .chain(save.then_some((HEADER, 8)))
    }

    pub fn save(&mut self, x87: &X87State, xmm: &[[u8; 16]; 16], mxcsr: u32, pkru: u32) {
        if self.requested & 1 != 0 {
            fx::save_x87(self.operation, x87, &mut self.bytes);
        }
        if self.requested & 2 != 0 {
            fx::save_xmm(self.operation, xmm, &mut self.bytes);
            put_dword(&mut self.bytes, 24, mxcsr);
            put_dword(&mut self.bytes, 28, 0xffff);
        }
        if self.requested & (1 << 9) != 0 {
            put_dword(&mut self.bytes, PKRU_OFFSET, pkru);
        }
    }

    pub fn mxcsr(&self) -> Result<u32, u32> {
        let loaded = dword(&self.bytes, 24);
        if loaded & !0xffff == 0 {
            Ok(loaded)
        } else {
            Err(loaded)
        }
    }

    /// The caller has admitted all reads and checked MXCSR before publishing banks.
    pub fn restore(
        &self,
        x87: &mut X87State,
        xmm: &mut [[u8; 16]; 16],
        mxcsr: &mut u32,
        pkru: &mut u32,
    ) {
        if self.requested & 1 != 0 {
            if self.present & 1 != 0 {
                fx::restore_x87(self.operation, &self.bytes, x87);
            } else {
                *x87 = X87State::default();
            }
        }
        if self.requested & 2 != 0 {
            if self.present & 2 != 0 {
                fx::restore_xmm(self.operation, &self.bytes, xmm);
            } else {
                xmm[..self.operation.xmm_count()].fill([0; 16]);
            }
            *mxcsr = dword(&self.bytes, 24);
        }
        if self.requested & (1 << 9) != 0 {
            *pkru = if self.present & (1 << 9) != 0 {
                dword(&self.bytes, PKRU_OFFSET)
            } else {
                0
            };
        }
    }
}
