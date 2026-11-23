use crate::embedding::exports::{
    memory::{paging, protection_keys::core_protection_key},
    tests::*,
};
use crate::x86::paging::{CR4_PKE, State as Paging};
use crate::x86::protection_keys::{PKRS_MSR, State as Keys};

fn setup() {
    initialize(0x1000, &[0x90]);
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        core.paging = Some(Paging::default());
        unsafe {
            (*core.state.get()).protection_keys = Keys {
                pkru: 0x12345678,
                pkrs: 0xabcdef01,
            };
        }
    });
}

fn bank() -> Keys {
    BRIDGE.with_borrow(|bridge| unsafe {
        (*bridge.core.as_ref().unwrap().state.get()).protection_keys
    })
}

#[test]
fn pkrs_msr_is_live_with_pks_disabled_and_rejects_reserved_upper_bits_atomically() {
    setup();
    assert_eq!(
        paging::control::core_paging_control(2, PKRS_MSR, 0, 0x1000, 2, 0),
        0
    );
    BRIDGE.with_borrow(|bridge| {
        let raw = unsafe { &*bridge.core.as_ref().unwrap().state.get() };
        assert_eq!(
            u64::from_le_bytes(raw.vector_scratch[..8].try_into().unwrap()),
            0xabcdef01
        );
    });
    for value in [0, u64::from(u32::MAX)] {
        assert_eq!(
            paging::control::core_paging_control(3, PKRS_MSR, value, 0x1000, 2, 0),
            0
        );
        assert_eq!(
            bank(),
            Keys {
                pkru: 0x12345678,
                pkrs: value as u32
            }
        );
    }
    for value in [1 << 32, u64::MAX] {
        assert_eq!(
            paging::control::core_paging_control(3, PKRS_MSR, value, 0x1000, 2, 0),
            crate::embedding::layout::PROTECTION
        );
        assert_eq!(
            bank(),
            Keys {
                pkru: 0x12345678,
                pkrs: u32::MAX
            }
        );
    }
}

#[test]
fn pkru_helper_validates_feature_and_registers_before_bank_or_output_publication() {
    setup();
    let before = bank();
    assert_eq!(core_protection_key(0, 0, 1, 1), INVALID);
    assert_eq!(core_protection_key(1, 0, 1, 1), INVALID);
    assert_eq!(bank(), before);
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        core.paging.as_mut().unwrap().cr4 |= CR4_PKE;
        unsafe { (*core.state.get()).vector_scratch.fill(0xa5) };
    });
    for (kind, ecx, edx) in [(0, 1, 0), (1, 1, 0), (1, 0, 1)] {
        assert_eq!(
            core_protection_key(kind, 0, ecx, edx),
            crate::embedding::layout::PROTECTION
        );
        assert_eq!(bank(), before);
        BRIDGE.with_borrow(|bridge| {
            assert_eq!(
                unsafe { (*bridge.core.as_ref().unwrap().state.get()).vector_scratch },
                [0xa5; 16]
            )
        });
    }
    assert_eq!(core_protection_key(1, u32::MAX, 0, 0), 0);
    assert_eq!(
        bank(),
        Keys {
            pkru: u32::MAX,
            pkrs: before.pkrs
        }
    );
    assert_eq!(core_protection_key(0, 0, 0, u32::MAX), 0);
    BRIDGE.with_borrow(|bridge| {
        let raw = unsafe { &*bridge.core.as_ref().unwrap().state.get() };
        assert_eq!(&raw.vector_scratch[..8], &u64::from(u32::MAX).to_le_bytes());
        assert_eq!(&raw.vector_scratch[8..], &[0; 8]);
    });
    assert_eq!(core_protection_key(2, 0, 0, 0), -1);
    assert_eq!(bank().pkru, u32::MAX);
}
