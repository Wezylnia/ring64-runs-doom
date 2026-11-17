mod arithmetic;
mod format;
mod lane;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    Add,
    Sub,
    Mul,
    Div,
    Sqrt,
    Min,
    Max,
    Cmp,
    Comi,
    Ucomi,
    FloatConvert,
    IntToFloat,
    FloatToInt,
    TruncToInt,
    Rcp,
    Rsqrt,
    // Bit 7 extends the helper opcode without overlapping the legacy format/width fields.
    AddSub = 0x80,
    HorizontalAdd,
    HorizontalSub,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Format {
    F32,
    F64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Op {
    pub kind: Kind,
    pub format: Format,
    pub scalar: bool,
    pub integer64: bool,
    pub predicate: u8,
}

impl Op {
    pub fn encode(self) -> u32 {
        self.kind as u32
            | ((self.format as u32) << 4)
            | ((self.scalar as u32) << 5)
            | ((self.integer64 as u32) << 6)
            | ((self.predicate as u32) << 8)
    }

    pub fn decode(value: u32) -> Self {
        use Kind::*;
        let kind = match value & !0x10 {
            0x80 => AddSub,
            0x81 => HorizontalAdd,
            0x82 => HorizontalSub,
            _ => [
                Add,
                Sub,
                Mul,
                Div,
                Sqrt,
                Min,
                Max,
                Cmp,
                Comi,
                Ucomi,
                FloatConvert,
                IntToFloat,
                FloatToInt,
                TruncToInt,
                Rcp,
                Rsqrt,
            ][(value & 15) as usize],
        };
        Self {
            kind,
            format: if value & 16 == 0 {
                Format::F32
            } else {
                Format::F64
            },
            scalar: value & 32 != 0,
            integer64: value & 64 != 0,
            predicate: (value >> 8) as u8,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    PreComputation,
    PostComputation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub value: u128,
    pub mxcsr: u32,
    pub raised: u32,
    pub stage: Option<Stage>,
}

pub(super) fn eval_pair(op: Op, left: u128, right: u128, mxcsr: u32) -> Outcome {
    debug_assert!(
        !op.scalar
            && !op.integer64
            && matches!(
                op.kind,
                Kind::IntToFloat | Kind::FloatToInt | Kind::TruncToInt
            )
    );
    if op.kind == Kind::IntToFloat {
        eval_lanes(op, left, right, mxcsr, 2)
    } else {
        eval_packed_to_integer(op, left, right, mxcsr, 2)
    }
}

pub fn eval(op: Op, left: u128, right: u128, mxcsr: u32) -> Outcome {
    if matches!(
        op.kind,
        Kind::AddSub | Kind::HorizontalAdd | Kind::HorizontalSub
    ) {
        return eval_sse3(op, left, right, mxcsr);
    }
    if matches!(op.kind, Kind::FloatToInt | Kind::TruncToInt) {
        return if op.scalar {
            eval_scalar_to_integer(op, left, right, mxcsr)
        } else {
            eval_packed_to_integer(op, left, right, mxcsr, 128 / op.format.bits())
        };
    }
    let count = if op.scalar || matches!(op.kind, Kind::Comi | Kind::Ucomi) {
        1
    } else if op.kind == Kind::FloatConvert {
        2
    } else {
        128 / op.format.bits()
    };
    eval_lanes(op, left, right, mxcsr, count)
}

#[inline(never)]
fn eval_scalar_to_integer(op: Op, left: u128, right: u128, mxcsr: u32) -> Outcome {
    // A GPR conversion consumes only the low element. Invalid and precision are
    // its only exceptions, and an invalid conversion suppresses precision.
    let input = op
        .format
        .daz(right as u64 & op.format.mask(), mxcsr & 64 != 0);
    let (value, raised) = arithmetic::to_integer(op, input, mxcsr);
    integer_outcome(left, u128::from(value), mxcsr, raised)
}

#[inline(never)]
fn eval_packed_to_integer(op: Op, old: u128, mut source: u128, mxcsr: u32, count: u32) -> Outcome {
    let mut value = 0;
    let mut raised = 0;
    for index in 0..count {
        let input = op
            .format
            .daz(source as u64 & op.format.mask(), mxcsr & 64 != 0);
        let (integer, flags) = arithmetic::to_integer(op, input, mxcsr);
        value |= u128::from(integer) << (32 * index);
        raised |= flags;
        source >>= op.format.bits();
    }
    integer_outcome(old, value, mxcsr, raised)
}

#[inline]
fn integer_outcome(old: u128, value: u128, mxcsr: u32, mut raised: u32) -> Outcome {
    let unmasked = raised & !(mxcsr >> 7);
    let stage = if unmasked & 1 != 0 {
        // Invalid aborts before precision from any other lane can be reported.
        raised = 1;
        Some(Stage::PreComputation)
    } else if unmasked & 32 != 0 {
        Some(Stage::PostComputation)
    } else {
        None
    };
    Outcome {
        value: if stage.is_some() { old } else { value },
        raised,
        mxcsr: mxcsr | raised,
        stage,
    }
}

#[inline(never)] // Keep ordinary floating-point instructions on the direct eval_lanes path.
fn eval_sse3(op: Op, left: u128, right: u128, mxcsr: u32) -> Outcome {
    let bits = op.format.bits();
    let mask = u128::from(op.format.mask());
    let count = 128 / bits;
    let (kind, a, b) = if op.kind == Kind::AddSub {
        // Subtraction is addition with the second sign reversed, except that an
        // x86 subtraction propagates a second-operand NaN with its original sign.
        let mut adjusted = right;
        for index in (0..count).step_by(2) {
            let shift = index * bits;
            if !op.format.nan(((right >> shift) & mask) as u64) {
                adjusted ^= u128::from(op.format.sign()) << shift;
            }
        }
        (Kind::Add, left, adjusted)
    } else {
        let mut even = 0;
        let mut odd = 0;
        for index in 0..count {
            let half = count / 2;
            let operand = if index < half { left } else { right };
            let pair = operand >> ((index & (half - 1)) * 2 * bits);
            even |= (pair & mask) << (index * bits);
            odd |= ((pair >> bits) & mask) << (index * bits);
        }
        let kind = if op.kind == Kind::HorizontalAdd {
            Kind::Add
        } else {
            Kind::Sub
        };
        (kind, even, odd)
    };
    let mut outcome = eval_lanes(Op { kind, ..op }, a, b, mxcsr, count);
    if outcome.stage.is_some() {
        outcome.value = left;
    }
    outcome
}

#[inline]
fn eval_lanes(op: Op, left: u128, right: u128, mxcsr: u32, count: u32) -> Outcome {
    let masks = (mxcsr >> 7) & 0x3f;
    let float_bits = op.format.bits();
    let float_mask = op.format.mask();
    let source_bits = if op.kind == Kind::IntToFloat {
        if op.scalar && op.integer64 { 64 } else { 32 }
    } else {
        float_bits
    };
    let source_mask = if source_bits == 32 {
        u64::from(u32::MAX)
    } else {
        u64::MAX
    };
    let output_bits = if op.kind == Kind::FloatConvert {
        op.format.opposite().bits()
    } else {
        float_bits
    };
    let mut lanes = [lane::Lane::default(); 4];
    let mut raised = 0;
    for index in 0..count {
        let a = (left >> (index * float_bits)) as u64 & float_mask;
        let b = (right >> (index * source_bits)) as u64 & source_mask;
        lanes[index as usize] = lane::prepare(op, a, b, mxcsr);
        raised |= lanes[index as usize].pre;
    }
    if raised & !masks != 0 {
        return Outcome {
            value: left,
            mxcsr: mxcsr | raised,
            raised,
            stage: Some(Stage::PreComputation),
        };
    }
    for index in 0..count {
        lanes[index as usize] = lane::calculate(op, lanes[index as usize], mxcsr);
        raised |= lanes[index as usize].post;
    }
    if raised & !masks != 0 {
        return Outcome {
            value: left,
            mxcsr: mxcsr | raised,
            raised,
            stage: Some(Stage::PostComputation),
        };
    }
    let gpr = matches!(op.kind, Kind::Comi | Kind::Ucomi);
    let mut value = if op.scalar && !gpr {
        left & !((1u128 << output_bits) - 1)
    } else {
        0
    };
    for index in 0..count {
        value |= (lanes[index as usize].value as u128) << (index * output_bits);
    }
    Outcome {
        value,
        mxcsr: mxcsr | raised,
        raised,
        stage: None,
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod packets;

#[cfg(test)]
mod native_reference;

#[cfg(test)]
mod pair_tests;

#[cfg(test)]
mod sse3_tests;

#[cfg(all(test, target_arch = "x86_64"))]
mod sse3_native;
