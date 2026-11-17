use core::arch::{asm, x86_64::__cpuid};
use std::io::Write;

#[repr(align(64))]
struct Area([u8; 576]);

#[test]
fn hardware_standard_xsave_and_xrstor_use_logical_x87_slots_with_rotated_top() {
    let id = __cpuid(0);
    let vendor_bytes = [
        id.ebx.to_le_bytes(),
        id.edx.to_le_bytes(),
        id.ecx.to_le_bytes(),
    ]
    .concat();
    let vendor = String::from_utf8_lossy(&vendor_bytes);
    const XSAVE_OSXSAVE: u32 = (1 << 26) | (1 << 27);
    if __cpuid(1).ecx & XSAVE_OSXSAVE != XSAVE_OSXSAVE {
        let _ = writeln!(
            std::io::stderr(),
            "native XSAVE oracle skipped on {vendor}: XSAVE/OSXSAVE unavailable"
        );
        return;
    }
    let (low, high): (u32, u32);
    // SAFETY: CPUID confirms that XGETBV is enabled. ECX=0 selects XCR0.
    unsafe {
        asm!("xgetbv", in("ecx") 0u32, out("eax") low, out("edx") high, options(nostack, nomem, preserves_flags));
    }
    let xcr0 = (u64::from(high) << 32) | u64::from(low);
    if xcr0 & 3 != 3 {
        let _ = writeln!(
            std::io::stderr(),
            "native XSAVE oracle skipped on {vendor} (XCR0={xcr0:#x}): x87/SSE state disabled"
        );
        return;
    }

    let mut host = Area([0; 576]);
    let mut fx = Area([0; 576]);
    let mut xs = Area([0; 576]);
    let mut restore_input = Area([0; 576]);
    let mut restored = Area([0; 576]);
    let mut restored_top = [0; 10];
    let slots: [[u8; 16]; 8] = std::array::from_fn(|i| {
        let mut slot = [0; 16];
        slot[..8].copy_from_slice(&((1u64 << 63) + i as u64).to_le_bytes());
        slot[8..10].copy_from_slice(&(0x3fffu16 + i as u16).to_le_bytes());
        slot
    });

    // Build XRSTOR input independently of either save instruction.
    restore_input.0[..2].copy_from_slice(&0x037fu16.to_le_bytes());
    restore_input.0[2..4].copy_from_slice(&(5u16 << 11).to_le_bytes());
    restore_input.0[4] = 0xff;
    restore_input.0[512] = 1;
    for (slot, payload) in restore_input.0[32..160].chunks_exact_mut(16).zip(slots) {
        slot[..10].copy_from_slice(&payload[..10]);
    }

    // SAFETY: all image pointers are distinct and 64-byte aligned. EDX:EAX requests
    // only x87. Both headers start at zero except for the valid x87 restore bit.
    // Only normal raw80 values enter an initialized, exception-masked x87 stack.
    // No Rust code runs between saving and restoring the host x87, XMM and MXCSR.
    unsafe {
        asm!(
            "fxsave64 [{host}]",
            "fninit",
            "fld tbyte ptr [{slots}]",
            "fld tbyte ptr [{slots} + 16]",
            "fld tbyte ptr [{slots} + 32]",
            "fld tbyte ptr [{slots} + 48]",
            "fld tbyte ptr [{slots} + 64]",
            "fld tbyte ptr [{slots} + 80]",
            "fld tbyte ptr [{slots} + 96]",
            "fld tbyte ptr [{slots} + 112]",
            "fincstp", "fincstp", "fincstp",
            "fxsave64 [{fx}]",
            "xsave64 [{xs}]",
            "fninit",
            "xrstor64 [{restore_input}]",
            "fxsave64 [{restored}]",
            "fstp tbyte ptr [{restored_top}]",
            "fxrstor64 [{host}]",
            host = in(reg) host.0.as_mut_ptr(),
            slots = in(reg) slots.as_ptr(),
            fx = in(reg) fx.0.as_mut_ptr(),
            xs = in(reg) xs.0.as_mut_ptr(),
            restore_input = in(reg) restore_input.0.as_ptr(),
            restored = in(reg) restored.0.as_mut_ptr(),
            restored_top = in(reg) restored_top.as_mut_ptr(),
            in("eax") 1u32, in("edx") 0u32, options(nostack),
        );
    }

    let top = |image: &Area| (u16::from_le_bytes([image.0[2], image.0[3]]) >> 11) & 7;
    assert_eq!((top(&fx), top(&xs), top(&restored)), (3, 3, 5));
    assert_eq!((fx.0[4], xs.0[4], restored.0[4]), (0xff, 0xff, 0xff));
    assert_eq!(xs.0[512] & 1, 1);
    for (logical, payload) in slots.iter().enumerate() {
        // Eight pushes reverse the register order. FINCSTP then rotates TOP to 3.
        let expected = &slots[7 - ((3 + logical) & 7)][..10];
        let at = 32 + 16 * logical;
        assert_eq!(&fx.0[at..at + 10], expected, "FXSAVE64 ST({logical})");
        assert_eq!(&xs.0[at..at + 10], expected, "XSAVE64 ST({logical})");
        assert_eq!(
            &restored.0[at..at + 10],
            &payload[..10],
            "XRSTOR64 ST({logical})"
        );
    }
    assert_eq!(restored_top, slots[0][..10], "XRSTOR64 architectural ST(0)");
    let _ = writeln!(
        std::io::stderr(),
        "native XSAVE oracle verified rotated save and restore slots on {vendor} (XCR0={xcr0:#x})"
    );
}
