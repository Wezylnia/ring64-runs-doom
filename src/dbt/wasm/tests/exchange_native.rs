use super::*;
use core::arch::{asm, x86_64::__cpuid};
use std::io::Write;

#[repr(align(16))]
struct Slot(u128);

#[test]
fn paired_exchange_matches_native_memory_registers_and_flags() {
    let features = __cpuid(1);
    let mut cases = Vec::new();
    for wide in [false, true] {
        if (wide && features.ecx & (1 << 13) == 0) || (!wide && features.edx & (1 << 8) == 0) {
            continue;
        }
        let original = 0x0123456789abcdef_fedcba9876543210u128;
        let replacement = [0xd00dfacebabebabeu64, 0xface0123cafebeef];
        for locked in [false, true] {
            let mut bytes = Vec::new();
            if locked {
                bytes.push(0xf0);
            }
            if wide {
                bytes.push(0x48);
            }
            bytes.extend([0x0f, 0xc7, 0x0f]); // [rdi]
            let artifact = compiled_fixture(0x1000, &bytes, 0x1000);
            for difference in [[0, 0], [1, 0], [0, 1]] {
                let lo = if wide {
                    original as u64
                } else {
                    0xaaaa000076543210
                };
                let hi = if wide {
                    (original >> 64) as u64
                } else {
                    0xbbbb0000fedcba98
                };
                let before = [lo ^ difference[0], hi ^ difference[1]];
                for seed in [2u64, 0xcd7] {
                    let mut slot = Slot(original);
                    let mut a = before[0];
                    let mut d = before[1];
                    let flags: u64;
                    // The slot is aligned, writable, private and feature-qualified above.
                    // Save RBX for the host ABI and restore the caller's flags, including DF.
                    macro_rules! native {
                        ($operation:literal) => {
                            unsafe {
                                asm!(
                                    "pushfq", "push r8", "popfq", "xchg rbx, rsi",
                                    $operation, "xchg rbx, rsi", "pushfq", "pop r9", "popfq",
                                    inout("rax") a, inout("rdx") d,
                                    in("rsi") replacement[0], in("rcx") replacement[1],
                                    in("rdi") &mut slot.0, in("r8") seed, out("r9") flags,
                                );
                            }
                        };
                    }
                    match (wide, locked) {
                        (true, true) => native!("lock cmpxchg16b [rdi]"),
                        (true, false) => native!("cmpxchg16b [rdi]"),
                        (false, true) => native!("lock cmpxchg8b [rdi]"),
                        (false, false) => native!("cmpxchg8b [rdi]"),
                    }
                    let mut case = execution_case(
                        &artifact.bytes,
                        &artifact.entries,
                        &[
                            (0, before[0]),
                            (2, before[1]),
                            (3, replacement[0]),
                            (1, replacement[1]),
                            (7, 0x8000),
                        ],
                        seed,
                        0x1000,
                        1,
                        (
                            &[(0, a), (2, d)],
                            flags & 0xcd7,
                            0x1000 + bytes.len() as u64,
                            1,
                            0,
                        ),
                    );
                    case["accesses"] = if wide {
                        json!([
                            {"kind":"block16_for_write","args":["8000",2],"result":format!("{original:x}")},
                            {"kind":"vector_store","args":["8000",16,2,16,format!("{:x}",slot.0)],"result":1}
                        ])
                    } else {
                        assert_eq!(slot.0 >> 64, original >> 64);
                        json!([
                            {"kind":"load_for_write","args":["8000",8],"result":format!("{:x}",original as u64)},
                            {"kind":"access_failed","args":[],"result":0},
                            {"kind":"store","args":["8000",8,format!("{:x}",slot.0 as u64)],"result":1}
                        ])
                    };
                    cases.push(case);
                }
            }
        }
    }
    let count = cases.len();
    run_cases(cases);
    let _ = writeln!(
        std::io::stderr(),
        "native paired exchange: {count} comparisons passed, CPUID signature={:#x}, CX8={}, CX16={}",
        features.eax,
        features.edx >> 8 & 1,
        features.ecx >> 13 & 1
    );
}
