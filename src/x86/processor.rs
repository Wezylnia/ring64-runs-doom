use super::context::{ExecutionContext, Mode};
use super::paging::{self, ControlFailure};
use super::protection::{ProtectionFault, ProtectionPhase};

pub const TSC_MSR: u32 = 0x10;
pub const TSC_AUX_MSR: u32 = 0xc0000103;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Instruction {
    Cpuid = 0,
    Rdtsc = 1,
    Rdtscp = 2,
    Xgetbv = 3,
    Xsetbv = 4,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct TimeState {
    pub tsc: u64,
    pub tsc_aux: u32,
}

pub fn cpuid(eax: u32, ecx: u32, cr4: u64, xcr0: u64) -> [u32; 4] {
    match eax {
        0 => [0xd, 0x676e6952, 0x656e6967, 0x6e453436],
        1 => [
            0,
            0,
            0x04020200 | (u32::from(cr4 & paging::CR4_OSXSAVE != 0) << 27),
            0x00002010,
        ],
        7 if ecx == 0 => [
            0,
            0x00102480,
            0x80000008 | if cr4 & (1 << 22) != 0 { 1 << 4 } else { 0 },
            0,
        ],
        0xd => match ecx {
            0 => [
                super::xstate::SUPPORTED as u32,
                if xcr0 & (1 << 9) != 0 {
                    super::xstate::MAX_BYTES
                } else {
                    super::xstate::PKRU_OFFSET
                } as u32,
                super::xstate::MAX_BYTES as u32,
                0,
            ],
            9 => [8, super::xstate::PKRU_OFFSET as u32, 0, 0],
            _ => [0; 4],
        },
        0x80000000 => [0x80000001, 0, 0, 0],
        0x80000001 => [0, 0, 0, 0x08000000],
        _ => [0; 4],
    }
}

impl TimeState {
    pub fn sample(self, retired: u64) -> u64 {
        self.tsc.wrapping_add(retired)
    }

    pub fn settle(&mut self, retired: u64) {
        self.tsc = self.sample(retired);
    }

    pub fn read_msr(self, index: u32, retired: u64) -> Result<u64, ControlFailure> {
        match index {
            TSC_MSR => Ok(self.sample(retired)),
            TSC_AUX_MSR => Ok(self.tsc_aux.into()),
            _ => Err(ControlFailure::Unsupported),
        }
    }

    pub fn write_msr(
        &mut self,
        index: u32,
        value: u64,
        prior_retired: u64,
    ) -> Result<(), ControlFailure> {
        match index {
            // The same bank is the invocation base until its common exit settles it.
            TSC_MSR => self.tsc = value.wrapping_sub(prior_retired).wrapping_sub(1),
            TSC_AUX_MSR => {
                self.tsc_aux = value
                    .try_into()
                    .map_err(|_| ControlFailure::GeneralProtection)?;
            }
            _ => return Err(ControlFailure::Unsupported),
        }
        Ok(())
    }
}

pub fn admit_timestamp(
    context: &ExecutionContext,
    paging: Option<paging::State>,
) -> Result<(), ProtectionFault> {
    if context.mode() == Mode::Real
        || context.cpl() == 0
        || paging.is_none_or(|state| state.cr4 & paging::CR4_TSD == 0)
    {
        Ok(())
    } else {
        Err(ProtectionFault {
            vector: 13,
            error_code: 0,
            phase: ProtectionPhase::Privilege,
            selector: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paging_identity_discloses_only_the_completed_translation_facilities() {
        assert_eq!(
            cpuid(1, 0, 0, 1),
            [0, 0, (1 << 26) | (1 << 17) | (1 << 9), (1 << 13) | (1 << 4)]
        );
        assert_eq!(
            cpuid(7, 0, 0, 1),
            [
                0,
                (1 << 20) | (1 << 13) | (1 << 10) | (1 << 7),
                0x80000008,
                0
            ]
        );
    }

    #[test]
    fn supervisor_protection_disclosure_uses_the_live_structured_subleaf() {
        assert_eq!(
            cpuid(0, u32::MAX, 0, 1),
            [0xd, 0x676e6952, 0x656e6967, 0x6e453436]
        );
        assert_eq!(cpuid(7, 0, 0, 1), [0, 0x00102480, 0x80000008, 0]);
        for subleaf in [1, 2, 0x80000000, u32::MAX] {
            assert_eq!(cpuid(7, subleaf, 0, 1), [0; 4], "subleaf {subleaf:x}");
        }
    }

    #[test]
    fn protection_keys_identity_tracks_only_live_pke_at_the_structured_leaf() {
        for cr4 in [0, 1 << 22, 1 << 24, (1 << 22) | (1 << 24), u64::MAX] {
            let expected = 0x80000008 | if cr4 & (1 << 22) != 0 { 0x10 } else { 0 };
            assert_eq!(cpuid(7, 0, cr4, 1), [0, 0x102480, expected, 0]);
            for subleaf in [1, 2, u32::MAX] {
                assert_eq!(cpuid(7, subleaf, cr4, 1), [0; 4]);
            }
            assert_eq!(
                cpuid(0, 0, cr4, 1),
                [0xd, 0x676e6952, 0x656e6967, 0x6e453436]
            );
        }
    }

    #[test]
    fn standard_xsave_discovery_uses_independent_osxsave_and_xcr0_authorities() {
        for cr4 in [0, paging::CR4_OSXSAVE] {
            for (xcr0, enabled_bytes) in [(1, 576), (3, 576), (0x201, 584), (0x203, 584)] {
                assert_eq!(
                    cpuid(1, 0, cr4, xcr0)[2] >> 26,
                    if cr4 == 0 { 1 } else { 3 }
                );
                assert_eq!(cpuid(0xd, 0, cr4, xcr0), [0x203, enabled_bytes, 584, 0]);
                assert_eq!(cpuid(0xd, 9, cr4, xcr0), [8, 576, 0, 0]);
                for subleaf in [1, 2, 8, 10, 63, u32::MAX] {
                    assert_eq!(cpuid(0xd, subleaf, cr4, xcr0), [0; 4]);
                }
            }
        }
    }

    #[test]
    fn identification_is_literal_and_ignores_unclaimed_subleaves() {
        for (leaf, expected) in [
            (0, [0xd, 0x676e6952, 0x656e6967, 0x6e453436]),
            (1, [0, 0, 0x04020200, 0x00002010]),
            (0x80000000, [0x80000001, 0, 0, 0]),
            (0x80000001, [0, 0, 0, 0x08000000]),
        ] {
            for subleaf in [0, 1, 0x80000000, u32::MAX] {
                assert_eq!(cpuid(leaf, subleaf, 0, 1), expected, "{leaf:x}/{subleaf:x}");
            }
        }
        for leaf in [
            2,
            7,
            0xd,
            0x15,
            0x16,
            0x40000000,
            0x80000007,
            0x80000008,
            u32::MAX,
        ] {
            assert_eq!(cpuid(leaf, u32::MAX, 0, 1), [0; 4], "{leaf:x}");
        }
        let [_, ebx, ecx, edx] = cpuid(0, 0, 0, 1);
        assert_eq!(
            [ebx, edx, ecx]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect::<Vec<_>>(),
            b"Ring64Engine"
        );
    }

    #[test]
    fn timestamp_samples_are_observational_and_settlement_wraps() {
        assert_eq!(TimeState::default(), TimeState { tsc: 0, tsc_aux: 0 });
        let mut time = TimeState {
            tsc: u64::MAX - 1,
            tsc_aux: 0x89abcdef,
        };
        assert_eq!(time.sample(0), u64::MAX - 1);
        assert_eq!(time.read_msr(TSC_MSR, 1), Ok(u64::MAX));
        assert_eq!(time.sample(2), 0);
        assert_eq!(time.sample(3), 1);
        assert_eq!(time.read_msr(TSC_AUX_MSR, u64::MAX), Ok(0x89abcdef));
        assert_eq!(time.tsc, u64::MAX - 1);
        time.settle(3);
        assert_eq!(
            time,
            TimeState {
                tsc: 1,
                tsc_aux: 0x89abcdef
            }
        );
    }

    #[test]
    fn timestamp_writes_replace_the_value_at_their_own_retirement() {
        let mut time = TimeState {
            tsc: 100,
            tsc_aux: 7,
        };
        assert_eq!(time.sample(3), 103);
        assert_eq!(time.write_msr(TSC_MSR, u64::MAX, 3), Ok(()));
        assert_eq!(time.tsc, u64::MAX - 4);
        assert_eq!(time.sample(4), u64::MAX);
        assert_eq!(time.sample(5), 0);
        assert_eq!(time.write_msr(TSC_MSR, 4, 5), Ok(()));
        assert_eq!(time.tsc, u64::MAX - 1);
        assert_eq!(time.sample(6), 4);
        time.settle(8);
        assert_eq!(time, TimeState { tsc: 6, tsc_aux: 7 });

        for value in [0, 0xffffffff, 0x100000000, 1 << 47, 1 << 63, u64::MAX] {
            for prior in [0, 1, 3, 0xffffffff, u64::MAX] {
                time.write_msr(TSC_MSR, value, prior).unwrap();
                assert_eq!(time.sample(prior.wrapping_add(1)), value);
                time.settle(prior.wrapping_add(3));
                assert_eq!(time.tsc, value.wrapping_add(2));
            }
        }
    }

    #[test]
    fn aux_reserved_values_and_unknown_indices_do_not_publish() {
        let mut time = TimeState {
            tsc: 100,
            tsc_aux: 7,
        };
        for value in [0, u32::MAX.into()] {
            time.write_msr(TSC_AUX_MSR, value, 123).unwrap();
            assert_eq!(time.read_msr(TSC_AUX_MSR, 123), Ok(value));
            assert_eq!(time.tsc, 100);
        }
        let before = time;
        for value in [0x100000000, 1 << 63, u64::MAX] {
            assert_eq!(
                time.write_msr(TSC_AUX_MSR, value, 123),
                Err(ControlFailure::GeneralProtection)
            );
            assert_eq!(time, before);
        }
        for index in [0, 0x11, 0x3b, 0xc0000102, 0xc0000104, u32::MAX] {
            assert_eq!(time.read_msr(index, 123), Err(ControlFailure::Unsupported));
            assert_eq!(
                time.write_msr(index, 1, 123),
                Err(ControlFailure::Unsupported)
            );
            assert_eq!(time, before);
        }
    }

    #[test]
    fn timestamp_privilege_uses_live_tsd_with_real_and_linear_exceptions() {
        let enabled = paging::State::default();
        let disabled = paging::State {
            cr4: enabled.cr4 | paging::CR4_TSD,
            ..enabled
        };
        let expected = ProtectionFault {
            vector: 13,
            error_code: 0,
            phase: ProtectionPhase::Privilege,
            selector: None,
        };
        for mode in [
            Mode::Real,
            Mode::Protected,
            Mode::Virtual8086,
            Mode::Compatibility,
            Mode::Long64,
        ] {
            for cpl in 0..4 {
                if (mode == Mode::Virtual8086 && cpl != 3) || (mode == Mode::Real && cpl != 0) {
                    continue;
                }
                let mut context = ExecutionContext::default();
                context.set_mode_and_cpl(mode, cpl);
                assert_eq!(admit_timestamp(&context, None), Ok(()));
                assert_eq!(admit_timestamp(&context, Some(enabled)), Ok(()));
                assert_eq!(
                    admit_timestamp(&context, Some(disabled)),
                    if cpl == 0 { Ok(()) } else { Err(expected) }
                );
            }
        }
    }
}
