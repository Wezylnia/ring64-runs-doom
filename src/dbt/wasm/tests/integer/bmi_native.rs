use crate::dbt::wasm::tests::*;
use core::arch::{
    asm,
    x86_64::{__cpuid, __cpuid_count},
};
use std::io::Write;

#[test]
fn native_bmi1_results_and_defined_flags_match_emitted_wasm() {
    let identity = __cpuid(0);
    let vendor: Vec<_> = [identity.ebx, identity.edx, identity.ecx]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
    let vendor = String::from_utf8_lossy(&vendor);
    if identity.eax < 7 {
        let _ = writeln!(
            std::io::stderr(),
            "native BMI1 skipped on {vendor}: no leaf 7"
        );
        return;
    }
    let features = __cpuid_count(7, 0).ebx;
    if features & 8 == 0 {
        let _ = writeln!(
            std::io::stderr(),
            "native BMI1 skipped on {vendor}: leaf 7 EBX={features:#x}"
        );
        return;
    }

    const DEST: u64 = 0x1122_3344_5566_7788;
    const SIGN_ZERO_CARRY_OVERFLOW: u64 = 0x8c1;
    const ZERO_CARRY_OVERFLOW: u64 = 0x841;
    const ZERO_CARRY: u64 = 0x41;
    type Native = fn(u64, u64) -> (u64, u64);

    // The host assembler encodes the mnemonic independently of the guest bytes.
    macro_rules! form {
        ($bytes:expr, $operation:literal, $flags:expr) => {
            ($bytes.as_slice(), $flags, (|word, control| {
                let result: u64;
                let flags: u64;
                // CPUID above admits all instructions below. No instruction changes DF.
                unsafe {
                    asm!($operation, "pushfq", "pop {flags}",
                        inout("rax") DEST => result,
                        in("rcx") word, in("rdx") control,
                        flags = lateout(reg) flags,
                    );
                }
                (result, flags)
            }) as Native)
        };
    }
    let forms: &[(&[u8], u64, Native)] = &[
        form!(
            [0xc4, 0xe2, 0x68, 0xf2, 0xc1],
            "andn eax, edx, ecx",
            SIGN_ZERO_CARRY_OVERFLOW
        ),
        form!(
            [0xc4, 0xe2, 0xe8, 0xf2, 0xc1],
            "andn rax, rdx, rcx",
            SIGN_ZERO_CARRY_OVERFLOW
        ),
        form!(
            [0xc4, 0xe2, 0x68, 0xf7, 0xc1],
            "bextr eax, ecx, edx",
            ZERO_CARRY_OVERFLOW
        ),
        form!(
            [0xc4, 0xe2, 0xe8, 0xf7, 0xc1],
            "bextr rax, rcx, rdx",
            ZERO_CARRY_OVERFLOW
        ),
        form!(
            [0xc4, 0xe2, 0x78, 0xf3, 0xd9],
            "blsi eax, ecx",
            SIGN_ZERO_CARRY_OVERFLOW
        ),
        form!(
            [0xc4, 0xe2, 0xf8, 0xf3, 0xd9],
            "blsi rax, rcx",
            SIGN_ZERO_CARRY_OVERFLOW
        ),
        form!(
            [0xc4, 0xe2, 0x78, 0xf3, 0xd1],
            "blsmsk eax, ecx",
            SIGN_ZERO_CARRY_OVERFLOW
        ),
        form!(
            [0xc4, 0xe2, 0xf8, 0xf3, 0xd1],
            "blsmsk rax, rcx",
            SIGN_ZERO_CARRY_OVERFLOW
        ),
        form!(
            [0xc4, 0xe2, 0x78, 0xf3, 0xc9],
            "blsr eax, ecx",
            SIGN_ZERO_CARRY_OVERFLOW
        ),
        form!(
            [0xc4, 0xe2, 0xf8, 0xf3, 0xc9],
            "blsr rax, rcx",
            SIGN_ZERO_CARRY_OVERFLOW
        ),
        form!([0x66, 0xf3, 0x0f, 0xbc, 0xc1], "tzcnt ax, cx", ZERO_CARRY),
        form!([0xf3, 0x0f, 0xbc, 0xc1], "tzcnt eax, ecx", ZERO_CARRY),
        form!([0xf3, 0x48, 0x0f, 0xbc, 0xc1], "tzcnt rax, rcx", ZERO_CARRY),
    ];
    let inputs = [
        (0, 0),
        (1, 0x0100),
        (2, 0x0101),
        (0xffff, 0x1000),
        (0x1_0000, 0x1010),
        (0x8000_0000, 0xff1f),
        (1 << 32, 0x0120),
        (1 << 63, 0x013f),
        (u64::MAX, 0x2040),
        (u64::MAX, 0x4000),
        (u64::MAX, 0xff00),
        (u64::MAX, 0xffff),
        (0xfedc_ba98_7654_3210, 0xfeed_4004),
    ];
    let mut cases = Vec::new();
    for &(bytes, mask, native) in forms {
        let artifact = compiled_fixture(0x1000, bytes, 0x1000);
        for &(word, control) in &inputs {
            let (result, flags) = native(word, control);
            let mut case = execution_case(
                &artifact.bytes,
                &artifact.entries,
                &[(0, DEST), (1, word), (2, control)],
                0x8d7,
                0x1000,
                1,
                (
                    &[(0, result)],
                    flags & mask,
                    0x1000 + bytes.len() as u64,
                    1,
                    0,
                ),
            );
            case["flagMask"] = json!(format!("{mask:x}"));
            cases.push(case);
        }
    }
    let rows = cases.len();
    run_cases(cases);
    let _ = writeln!(
        std::io::stderr(),
        "native BMI1: {rows} comparisons passed on {vendor}, signature={:#x}, leaf 7 EBX={features:#x}",
        __cpuid(1).eax
    );
}
