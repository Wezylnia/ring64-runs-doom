use crate::dbt::wasm::tests::*;

const SYSTEM_FLAGS: u64 = (1 << 21) | (1 << 10) | (1 << 9);

fn packet(code: &[u8], input: &[(usize, u64)], result: u64, flags: u64, defined: u64) -> Value {
    let artifact = compiled_fixture(0x1000, code, 0x1000);
    let mut packet = execution_case(
        &artifact.bytes,
        &artifact.entries,
        input,
        SYSTEM_FLAGS | 0x8d7,
        0x1000,
        1,
        (
            &[(0, result)],
            SYSTEM_FLAGS | flags | 2,
            0x1000 + code.len() as u64,
            1,
            0,
        ),
    );
    packet["flagMask"] = json!(format!("{:x}", SYSTEM_FLAGS | defined | 2));
    packet
}

#[test]
fn bmi_bit_positions_flags_and_overlapping_registers() {
    let mut cases = Vec::new();
    for bits in [32u32, 64] {
        let vex = if bits == 32 { 0x78 } else { 0xf8 };
        let top = 1u64 << (bits - 1);
        let mask = top | (top - 1);
        for src in [0, 1, 2, 9, top, top | 8, mask] {
            let first = (0..bits).find(|i| src & (1u64 << i) != 0);
            // BLS and TZCNT each read and replace RAX; preserve the high
            // input bits for dword tests to catch an accidental qword read.
            let input = [(0, src | !mask)];
            for (modrm, result, carry) in [
                (0xd8, first.map_or(0, |i| 1u64 << i), src != 0),
                (
                    0xd0,
                    first.map_or(mask, |i| ((1u128 << (i + 1)) - 1) as u64),
                    src == 0,
                ),
                (0xc8, src - first.map_or(0, |i| 1u64 << i), src == 0),
            ] {
                let flags = u64::from(carry)
                    | (u64::from(result == 0) << 6)
                    | (u64::from(result & top != 0) << 7);
                cases.push(packet(
                    &[0xc4, 0xe2, vex, 0xf3, modrm],
                    &input,
                    result,
                    flags,
                    0x8c1,
                ));
            }
            let mut tz = vec![0xf3];
            if bits == 64 {
                tz.push(0x48);
            }
            tz.extend([0x0f, 0xbc, 0xc0]);
            let count = first.unwrap_or(bits);
            let flags = u64::from(src == 0) | (u64::from(count == 0) << 6);
            cases.push(packet(&tz, &input, count.into(), flags, 0x41));
        }
        // RAX is both the written destination and the complemented source.
        cases.push(packet(
            &[0xc4, 0xe2, vex, 0xf2, 0xc1],
            &[(0, 3), (1, top | 5)],
            top | 4,
            0x80,
            0x8c1,
        ));
    }
    for (v, expected, flags) in [(0, 16, 1), (1, 0, 0x40), (0x8000, 15, 0)] {
        let high = 0xdead_beef_cafe_0000;
        cases.push(packet(
            &[0xf3, 0x66, 0x0f, 0xbc, 0xc0],
            &[(0, high | v)],
            high | expected,
            flags,
            0x41,
        ));
    }
    run_cases(cases);
}

#[test]
fn bextr_clips_control_bytes_without_wrapping_shift_counts() {
    let mut cases = Vec::new();
    let source = 0x9176_34e2_8c50_afd9;
    for (bits, vex) in [(32u32, 0x78), (64, 0xf8)] {
        for start in [0, 1, bits - 1, bits, 64, 65, 255] {
            for len in [0, 1, 31, 32, 63, 64, 65, 255] {
                let value = (0..len)
                    .take_while(|&i| start + i < bits)
                    .fold(0, |value, i| value | ((source >> (start + i)) & 1) << i);
                let control = 0xffff_ffff_0000_0000 | u64::from(start | len << 8);
                // RAX supplies the control before being overwritten.
                cases.push(packet(
                    &[0xc4, 0xe2, vex, 0xf7, 0xc2],
                    &[(0, control), (2, source)],
                    value,
                    u64::from(value == 0) << 6,
                    0x841,
                ));
            }
        }
    }
    run_cases(cases);
}
