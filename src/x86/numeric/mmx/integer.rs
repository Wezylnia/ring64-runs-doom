use super::*;

fn lane(value: u64, bits: u32, index: u32) -> u64 {
    (value >> (bits * index)) & mask(bits)
}

fn mask(bits: u32) -> u64 {
    u64::MAX >> (64 - bits)
}

fn signed(value: u64, bits: u32) -> i64 {
    ((value << (64 - bits)) as i64) >> (64 - bits)
}

fn packed(bits: u32, mut calculate: impl FnMut(u32) -> u64) -> u64 {
    let mut output = 0;
    for i in 0..64 / bits {
        output |= (calculate(i) & mask(bits)) << (i * bits);
    }
    output
}

pub(super) fn binary(kind: VectorBinary, left: u64, right: u64) -> u64 {
    use VectorBinary::*;
    match kind {
        And => left & right,
        AndNot => !left & right,
        Or => left | right,
        Xor => left ^ right,
        MulUnsignedDword => u64::from(left as u32) * u64::from(right as u32),
        SumAbsoluteByteDifferences => (0..8)
            .map(|i| lane(left, 8, i).abs_diff(lane(right, 8, i)))
            .sum(),
        MaddSignedWord => packed(32, |i| {
            let product = |j| signed(lane(left, 16, j), 16) * signed(lane(right, 16, j), 16);
            (product(i * 2) + product(i * 2 + 1)) as u64
        }),
        PackSigned(width) => pack(left, right, width.bits(), false),
        PackUnsignedWord => pack(left, right, 16, true),
        UnpackLow(width) => unpack(left, right, width.bits(), false),
        UnpackHigh(width) => unpack(left, right, width.bits(), true),
        Add(width)
        | Sub(width)
        | SignedSaturatingAdd(width)
        | UnsignedSaturatingAdd(width)
        | SignedSaturatingSub(width)
        | UnsignedSaturatingSub(width)
        | CompareEqual(width)
        | CompareGreater(width)
        | Average(width) => {
            let bits = width.bits();
            packed(bits, |i| {
                let a = lane(left, bits, i);
                let b = lane(right, bits, i);
                match kind {
                    Add(_) => a.wrapping_add(b),
                    Sub(_) => a.wrapping_sub(b),
                    SignedSaturatingAdd(_) | SignedSaturatingSub(_) => {
                        let a = signed(a, bits);
                        let b = signed(b, bits);
                        let result = if matches!(kind, SignedSaturatingAdd(_)) {
                            a + b
                        } else {
                            a - b
                        };
                        result.clamp(-(1i64 << (bits - 1)), (1i64 << (bits - 1)) - 1) as u64
                    }
                    UnsignedSaturatingAdd(_) => (a + b).min(mask(bits)),
                    UnsignedSaturatingSub(_) => a.saturating_sub(b),
                    CompareEqual(_) => {
                        if a == b {
                            mask(bits)
                        } else {
                            0
                        }
                    }
                    CompareGreater(_) => {
                        if signed(a, bits) > signed(b, bits) {
                            mask(bits)
                        } else {
                            0
                        }
                    }
                    Average(_) => (a + b + 1) >> 1,
                    _ => unreachable!(),
                }
            })
        }
        MinUnsignedByte | MaxUnsignedByte => packed(8, |i| {
            let a = lane(left, 8, i);
            let b = lane(right, 8, i);
            if kind == MinUnsignedByte {
                a.min(b)
            } else {
                a.max(b)
            }
        }),
        MinSignedWord | MaxSignedWord => packed(16, |i| {
            let a = signed(lane(left, 16, i), 16);
            let b = signed(lane(right, 16, i), 16);
            (if kind == MinSignedWord {
                a.min(b)
            } else {
                a.max(b)
            }) as u64
        }),
        MulLowWord => packed(16, |i| lane(left, 16, i) * lane(right, 16, i)),
        MulHighWord {
            signed: signed_product,
        } => packed(16, |i| {
            let a = lane(left, 16, i);
            let b = lane(right, 16, i);
            if signed_product {
                ((signed(a, 16) * signed(b, 16)) >> 16) as u64
            } else {
                (a * b) >> 16
            }
        }),
    }
}

fn pack(left: u64, right: u64, source_bits: u32, unsigned: bool) -> u64 {
    let target_bits = source_bits / 2;
    let half = 64 / source_bits;
    packed(target_bits, |i| {
        let value = signed(
            lane(if i < half { left } else { right }, source_bits, i % half),
            source_bits,
        );
        if unsigned {
            value.clamp(0, mask(target_bits) as i64) as u64
        } else {
            value.clamp(
                -(1i64 << (target_bits - 1)),
                (1i64 << (target_bits - 1)) - 1,
            ) as u64
        }
    })
}

fn unpack(left: u64, right: u64, bits: u32, high: bool) -> u64 {
    let start = if high { 32 / bits } else { 0 };
    packed(bits, |i| {
        lane(if i & 1 == 0 { left } else { right }, bits, start + i / 2)
    })
}

pub(super) fn shift(kind: VectorShift, value: u64, count: u64) -> u64 {
    use VectorShift::*;
    let bits = match kind {
        Left(width) | Right(width) | ArithmeticRight(width) => width.bits(),
        _ => unreachable!("not a selected MMX shift"),
    };
    packed(bits, |i| {
        let lane = lane(value, bits, i);
        match kind {
            ArithmeticRight(_) => (signed(lane, bits) >> count.min(u64::from(bits - 1))) as u64,
            Left(_) if count < u64::from(bits) => lane << count,
            Right(_) if count < u64::from(bits) => lane >> count,
            _ => 0,
        }
    })
}

pub(super) fn shuffle(value: u64, immediate: u8) -> u64 {
    packed(16, |i| {
        lane(value, 16, u32::from((immediate >> (2 * i)) & 3))
    })
}

pub(super) fn mask_bytes(value: u64) -> u64 {
    (0..8).map(|i| ((value >> (8 * i + 7)) & 1) << i).sum()
}

pub(super) fn ssse3(kind: Ssse3Op, left: u64, right: u64, immediate: u8) -> u64 {
    use Ssse3Op::*;
    match kind {
        Pshufb => packed(8, |i| {
            let selector = lane(right, 8, i) as u8;
            if selector & 0x80 == 0 {
                lane(left, 8, u32::from(selector & 7))
            } else {
                0
            }
        }),
        Phaddw | Phaddd | Phaddsw | Phsubw | Phsubd | Phsubsw => {
            let bits = if matches!(kind, Phaddd | Phsubd) {
                32
            } else {
                16
            };
            let pairs = 32 / bits;
            packed(bits, |i| {
                let source = if i < pairs { left } else { right };
                let first = 2 * (i % pairs);
                let a = signed(lane(source, bits, first), bits);
                let b = signed(lane(source, bits, first + 1), bits);
                match kind {
                    Phaddsw => (a + b).clamp(-32768, 32767) as u64,
                    Phsubsw => (a - b).clamp(-32768, 32767) as u64,
                    Phaddw | Phaddd => (a + b) as u64,
                    _ => (a - b) as u64,
                }
            })
        }
        Pmaddubsw => packed(16, |i| {
            let product = |j| lane(left, 8, j) as i64 * signed(lane(right, 8, j), 8);
            (product(2 * i) + product(2 * i + 1)).clamp(-32768, 32767) as u64
        }),
        Pmulhrsw => packed(16, |i| {
            let product = signed(lane(left, 16, i), 16) * signed(lane(right, 16, i), 16);
            ((product + 0x4000) >> 15) as u64
        }),
        Psignb | Psignw | Psignd | Pabsb | Pabsw | Pabsd => {
            let bits = match kind {
                Psignb | Pabsb => 8,
                Psignw | Pabsw => 16,
                _ => 32,
            };
            packed(bits, |i| {
                let sign = signed(lane(right, bits, i), bits);
                if matches!(kind, Pabsb | Pabsw | Pabsd) {
                    sign.unsigned_abs()
                } else {
                    (signed(lane(left, bits, i), bits) * sign.signum()) as u64
                }
            })
        }
        Palignr => {
            let joined = u128::from(right) | (u128::from(left) << 64);
            joined.checked_shr(8 * u32::from(immediate)).unwrap_or(0) as u64
        }
    }
}
