use super::{CR4_PKE, CR4_PKS, CR4_SMAP, CR4_SMEP, State};
use crate::memory::{Access, Fault, Image};
use crate::x86::protection_keys::State as ProtectionKeys;

const ADDRESS: u64 = 0x000f_ffff_ffff_f000;
const ACCESSED: u64 = 1 << 5;
pub const DIRTY: u64 = 1 << 6;

#[cfg(test)]
pub(crate) mod physical_counts {
    use std::cell::Cell;

    // Attempted Image calls and requested byte widths, in this order:
    // read/read-bytes, write-check/check-bytes, write/write-bytes.
    thread_local! { static COUNTS: Cell<[u64; 6]> = const { Cell::new([0; 6]) }; }

    fn record(slot: usize, width: u32) {
        COUNTS.with(|counts| {
            let mut current = counts.get();
            current[slot] += 1;
            current[slot + 1] += u64::from(width);
            counts.set(current);
        });
    }
    pub fn read(width: u32) {
        record(0, width);
    }
    pub fn check(width: u32) {
        record(2, width);
    }
    pub fn write(width: u32) {
        record(4, width);
    }
    pub fn take() -> [u64; 6] {
        COUNTS.with(|counts| counts.replace([0; 6]))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessClass {
    User,
    Supervisor { ac: bool },
    ImplicitSupervisor,
}

impl AccessClass {
    pub fn operand(cpl: u8, ac: bool) -> Self {
        if cpl == 3 {
            Self::User
        } else {
            Self::Supervisor { ac }
        }
    }

    pub fn is_user(self) -> bool {
        self == Self::User
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    pub linear: u64,
    pub access: Access,
    pub class: AccessClass,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageFault {
    pub address: u64,
    pub access: Access,
    pub error_code: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhysicalPhase {
    WalkRead,
    AccessedWrite,
    DirtyWrite,
    FinalAccess,
}

impl PhysicalPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WalkRead => "walk-read",
            Self::AccessedWrite => "accessed-write",
            Self::DirtyWrite => "dirty-write",
            Self::FinalAccess => "final-access",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhysicalFault {
    pub linear: u64,
    pub access: Access,
    pub physical: Fault,
    pub phase: PhysicalPhase,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Page(PageFault),
    Physical(PhysicalFault),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rights {
    pub user: bool,
    pub writable: bool,
    pub executable: bool,
    pub key: u8,
}

impl Rights {
    pub fn authorize(
        self,
        state: &State,
        request: Request,
        keys: &ProtectionKeys,
    ) -> Result<(), Failure> {
        let user_request = request.class.is_user();
        let protection_denied = !user_request
            && self.user
            && if request.access == Access::Execute {
                state.cr4 & CR4_SMEP != 0
            } else {
                state.cr4 & CR4_SMAP != 0
                    && !matches!(request.class, AccessClass::Supervisor { ac: true })
            };
        let key_denied = request.access != Access::Execute
            && state.cr0 & (1 << 31) != 0
            && state.efer & (1 << 10) != 0
            && if self.user {
                state.cr4 & CR4_PKE != 0 && key_denied(keys.pkru, self.key, state, request)
            } else {
                state.cr4 & CR4_PKS != 0 && key_denied(keys.pkrs, self.key, state, request)
            };
        if user_request && !self.user
            || request.access == Access::Write
                && !self.writable
                && (user_request || state.cr0 & (1 << 16) != 0)
            || request.access == Access::Execute && !self.executable
            || protection_denied
            || key_denied
        {
            Err(request.page_fault(state, true, false, key_denied))
        } else {
            Ok(())
        }
    }
}

fn key_denied(word: u32, key: u8, state: &State, request: Request) -> bool {
    let pair = word >> (u32::from(key & 15) * 2) & 3;
    pair & 1 != 0
        || request.access == Access::Write
            && pair & 2 != 0
            && (request.class.is_user() || state.cr0 & (1 << 16) != 0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Translation {
    pub physical: u64,
    pub leaf: u64,
    pub dirty: bool,
    pub remaining: u64,
    pub entry_width: u32,
    pub linear_base: u64,
    pub physical_base: u64,
    pub span: u64,
    pub rights: Rights,
    pub global: bool,
    pub accessed: bool,
    pub cache_id: Option<u64>,
    entries: [u64; 4],
    count: usize,
}

impl Translation {
    pub fn at(mut self, linear: u64) -> Self {
        let offset = linear - self.linear_base;
        self.physical = self.physical_base + offset;
        self.remaining = self.span - offset;
        self
    }

    pub fn contains(self, linear: u64) -> bool {
        linear.wrapping_sub(self.linear_base) < self.span
    }

    pub fn entries(&self) -> &[u64] {
        &self.entries[..self.count]
    }
}

impl Request {
    pub fn unpaged_physical(self) -> Result<u64, Failure> {
        if self.linear >= 1u64 << 32 {
            return Err(self.physical_failure(
                Fault {
                    address: self.linear,
                    width: 1,
                    access: self.access,
                    cause: crate::memory::Cause::AddressRange,
                    failing: 1u128 << 32,
                },
                PhysicalPhase::FinalAccess,
            ));
        }
        Ok(self.linear)
    }

    fn page_fault(self, state: &State, present: bool, reserved: bool, key: bool) -> Failure {
        Failure::Page(PageFault {
            address: self.linear,
            access: self.access,
            error_code: u32::from(present)
                | (u32::from(self.access == Access::Write) << 1)
                | (u32::from(self.class.is_user()) << 2)
                | (u32::from(reserved) << 3)
                | (u32::from(
                    self.access == Access::Execute
                        && (state.cr4 & CR4_SMEP != 0
                            || state.cr4 & (1 << 5) != 0 && state.efer & (1 << 11) != 0),
                ) << 4)
                | (u32::from(key) << 5),
        })
    }

    pub fn physical_failure(self, physical: Fault, phase: PhysicalPhase) -> Failure {
        Failure::Physical(PhysicalFault {
            linear: self.linear,
            access: self.access,
            physical,
            phase,
        })
    }
}

pub fn peek(
    image: &Image,
    state: &State,
    request: Request,
    keys: &ProtectionKeys,
) -> Result<Translation, Failure> {
    peek_with_reader(state, request, keys, |address, width| {
        read_walk_word(image, address, width)
    })
}

fn read_walk_word(image: &Image, address: u64, width: u32) -> Result<u64, Fault> {
    #[cfg(test)]
    physical_counts::read(width);
    image.physical_read(address, width)
}

fn peek_with_reader(
    state: &State,
    request: Request,
    keys: &ProtectionKeys,
    mut read: impl FnMut(u64, u32) -> Result<u64, Fault>,
) -> Result<Translation, Failure> {
    walk_with::<false, false>(
        state,
        request,
        |address, width, _| read(address, width).map(Some),
        keys,
    )
    .map(|translation| translation.expect("observational walk cannot restart"))
}

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
#[derive(Default)]
pub(crate) struct UpperWords {
    words: [WalkWord; 3],
}

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
#[derive(Default)]
struct WalkWord {
    address: u64,
    value: u64,
    width: u32,
}

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
impl UpperWords {
    pub(crate) fn peek(
        &mut self,
        image: &Image,
        state: &State,
        request: Request,
        keys: &ProtectionKeys,
    ) -> Result<Translation, Failure> {
        // The pure walker reads once per visited level. Only the possible
        // ancestors of a 4 KiB leaf are eligible. An early large-leaf result
        // is refused by the caller, which drops this visit's scratch.
        let upper = if state.efer & (1 << 10) != 0 { 3 } else { 1 };
        let mut level = 0;
        peek_with_reader(state, request, keys, |address, width| {
            let index = level;
            level += 1;
            if index >= upper {
                return read_walk_word(image, address, width);
            }
            let word = &mut self.words[index];
            if word.width == width && word.address == address {
                return Ok(word.value);
            }
            let value = read_walk_word(image, address, width)?;
            *word = WalkWord {
                address,
                value,
                width,
            };
            Ok(value)
        })
    }
}

pub fn walk(
    image: &mut Image,
    state: &State,
    request: Request,
    keys: &ProtectionKeys,
) -> Result<Translation, Failure> {
    actual_walk::<false>(image, state, request, |_| true, keys)
        .map(|translation| translation.expect("ordinary access does not abandon a walk"))
}

/// Stop a required-byte walk immediately when an A write invalidates consumed encoding.
pub fn fetch_walk(
    image: &mut Image,
    state: &State,
    request: Request,
    current: impl FnMut(&Image) -> bool,
    keys: &ProtectionKeys,
) -> Result<Option<Translation>, Failure> {
    actual_walk::<true>(image, state, request, current, keys)
}

fn actual_walk<const FETCH: bool>(
    image: &mut Image,
    state: &State,
    request: Request,
    mut current: impl FnMut(&Image) -> bool,
    keys: &ProtectionKeys,
) -> Result<Option<Translation>, Failure> {
    #[cfg(all(feature = "parallel", any(target_arch = "wasm32", test)))]
    let _grant = image.shared_exclusive();
    walk_with::<true, FETCH>(
        state,
        request,
        |address, width, accessed| {
            if accessed {
                #[cfg(test)]
                physical_counts::check(width);
                image.physical_check_write(address, width)?;
                #[cfg(test)]
                physical_counts::read(width);
                let current_word = image.physical_read(address, width)?;
                #[cfg(test)]
                physical_counts::write(width);
                image.physical_write(address, width, current_word | ACCESSED)?;
                Ok(current(image).then_some(current_word | ACCESSED))
            } else {
                #[cfg(test)]
                physical_counts::read(width);
                image.physical_read(address, width).map(Some)
            }
        },
        keys,
    )
}

fn walk_with<const EFFECTS: bool, const FETCH: bool>(
    state: &State,
    request: Request,
    entry_access: impl FnMut(u64, u32, bool) -> Result<Option<u64>, Fault>,
    keys: &ProtectionKeys,
) -> Result<Option<Translation>, Failure> {
    if state.cr0 & (1 << 31) == 0 {
        let physical = request.unpaged_physical()?;
        return Ok(Some(Translation {
            physical,
            leaf: 0,
            dirty: false,
            remaining: (1u64 << 32) - physical,
            entry_width: 0,
            linear_base: 0,
            physical_base: 0,
            span: 1u64 << 32,
            rights: Rights {
                user: true,
                writable: true,
                executable: true,
                key: 0,
            },
            global: false,
            accessed: false,
            cache_id: None,
            entries: [0; 4],
            count: 0,
        }));
    }
    if state.efer & (1 << 10) != 0 {
        walk_paged_with::<EFFECTS, FETCH, true, true>(state, request, entry_access, keys)
    } else if state.cr4 & (1 << 5) != 0 {
        walk_paged_with::<EFFECTS, FETCH, false, true>(state, request, entry_access, keys)
    } else {
        walk_paged_with::<EFFECTS, FETCH, false, false>(state, request, entry_access, keys)
    }
}

// Keep one set of walk/effect rules while specializing geometry and entry width.
fn walk_paged_with<const EFFECTS: bool, const FETCH: bool, const IA32E: bool, const PAE: bool>(
    state: &State,
    request: Request,
    mut entry_access: impl FnMut(u64, u32, bool) -> Result<Option<u64>, Fault>,
    keys: &ProtectionKeys,
) -> Result<Option<Translation>, Failure> {
    let entry_width = if IA32E || PAE { 8 } else { 4 };
    let shifts: &[u32] = if IA32E {
        &[39, 30, 21, 12]
    } else if PAE {
        &[21, 12]
    } else {
        &[22, 12]
    };
    let mut table = if IA32E {
        state.cr3 & ADDRESS
    } else {
        state.cr3 & 0xfffff000
    };
    if !IA32E && PAE {
        let root =
            state.pdpte.expect("validated legacy PAE quartet")[(request.linear >> 30 & 3) as usize];
        if root & 1 == 0 {
            return Err(request.page_fault(state, false, false, false));
        }
        table = root & ADDRESS;
    }
    let mut user = true;
    let mut writable = true;
    let mut executable = true;
    let mut accessed = true;
    let mut entries = [0; 4];
    let mut words = [0; 4];
    for (index, &shift) in shifts.iter().enumerate() {
        let address = table
            + ((request.linear >> shift) & if entry_width == 4 { 1023 } else { 511 })
                * u64::from(entry_width);
        entries[index] = address;
        let Some(entry) = entry_access(address, entry_width, false)
            .map_err(|f| request.physical_failure(f, PhysicalPhase::WalkRead))?
        else {
            return Ok(None);
        };
        words[index] = entry;
        if entry & 1 == 0 {
            return Err(request.page_fault(state, false, false, false));
        }
        let large = index + 1 < shifts.len()
            && entry & (1 << 7) != 0
            && (entry_width == 8 || state.cr4 & (1 << 4) != 0);
        let leaf = index + 1 == shifts.len() || large;
        let reserved_address = large
            && if entry_width == 4 {
                entry & (1 << 21) != 0
            } else {
                entry & (((1u64 << shift) - 1) & !0x1fff) != 0
            };
        if IA32E && index == 0 && large
            || reserved_address
            || !IA32E && PAE && entry & 0x7ff0000000000000 != 0
            || state.efer & (1 << 11) == 0 && entry & (1 << 63) != 0
        {
            return Err(request.page_fault(state, true, true, false));
        }
        if EFFECTS && entry & ACCESSED == 0 {
            if entry_access(address, entry_width, true)
                .map_err(|f| request.physical_failure(f, PhysicalPhase::AccessedWrite))?
                .is_none()
            {
                return Ok(None);
            }
            if FETCH {
                // An A alias can redirect this byte's already-read hierarchy before a lower fault.
                for used in 0..=index {
                    let Some(current) = entry_access(entries[used], entry_width, false)
                        .map_err(|f| request.physical_failure(f, PhysicalPhase::WalkRead))?
                    else {
                        return Ok(None);
                    };
                    if (current ^ words[used]) & !ACCESSED != 0 {
                        return Ok(None);
                    }
                }
            }
        }
        accessed &= EFFECTS || entry & ACCESSED != 0;
        user &= entry & 4 != 0;
        writable &= entry & 2 != 0;
        executable &= entry & (1 << 63) == 0;
        if leaf {
            let rights = Rights {
                user,
                writable,
                executable,
                key: if IA32E { ((entry >> 59) & 15) as u8 } else { 0 },
            };
            rights.authorize(state, request, keys)?;
            let offset_mask = (1u64 << shift) - 1;
            return Ok(Some(Translation {
                physical: (if entry_width == 4 && large {
                    (entry & 0xffc00000) | ((entry & 0x001fe000) << 19)
                } else {
                    entry & ADDRESS & !offset_mask
                }) | (request.linear & offset_mask),
                leaf: address,
                dirty: entry & DIRTY == 0,
                remaining: (1u64 << shift) - (request.linear & offset_mask),
                entry_width,
                linear_base: request.linear & !offset_mask,
                physical_base: if entry_width == 4 && large {
                    (entry & 0xffc00000) | ((entry & 0x001fe000) << 19)
                } else {
                    entry & ADDRESS & !offset_mask
                },
                span: 1u64 << shift,
                rights,
                global: entry & (1 << 8) != 0,
                accessed,
                cache_id: None,
                entries,
                count: index + 1,
            }));
        }
        table = entry & ADDRESS;
    }
    unreachable!("four-level walk always reaches a PTE")
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod legacy_tests;

#[cfg(test)]
mod supervisor_tests;

#[cfg(test)]
mod unpaged_tests;
