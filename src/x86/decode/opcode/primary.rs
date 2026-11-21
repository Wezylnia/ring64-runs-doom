use crate::x86::compound_stack::{CompoundStack, CompoundStackKind};
use crate::x86::context::{Mode, Segment};
use crate::x86::instruction::{
    Address, AddressBase, Alu, Carry, Count, CountCondition, DataOperand, Location,
    MemoryAluOperands, Operand, Operation, RegisterView, Repeat, Shift, StringKind, Unary, Width,
};

use super::super::{
    self as decoder, DecodeError, cursor::Cursor, operand::ModRm, prefix::Prefixes,
};
use super::{branch, byte_or_wide, escaped, relative_displacement};

pub(super) fn decode<F>(
    opcode: u8,
    prefixes: Prefixes,
    cursor: &mut Cursor<F>,
) -> Result<Operation, DecodeError>
where
    F: FnMut(u64) -> Result<u8, DecodeError>,
{
    if let Some(result) = decoder::io::refine(opcode, prefixes, cursor) {
        return result;
    }
    if let Some(result) = decoder::software_events::one_byte(opcode, prefixes, cursor) {
        return result;
    }
    if let Some(result) = decoder::system_flags::one_byte(opcode, prefixes) {
        return result;
    }
    if let Some(result) = decoder::far::one_byte(opcode, prefixes, cursor) {
        return result;
    }
    if let Some(result) = decoder::protection::one_byte(opcode, prefixes, cursor) {
        return result;
    }
    if matches!(opcode, 0x9b | 0xd8..=0xdf) {
        return decoder::x87::refine(opcode, prefixes, cursor);
    }
    let string_kind = match opcode {
        0xa4 | 0xa5 => Some(StringKind::Movs),
        0xaa | 0xab => Some(StringKind::Stos),
        0xac | 0xad => Some(StringKind::Lods),
        0xa6 | 0xa7 => Some(StringKind::Cmps),
        0xae | 0xaf => Some(StringKind::Scas),
        _ => None,
    };
    if prefixes.lock
        && !matches!(opcode,
            0x00..=0x05 | 0x08..=0x0d | 0x0f | 0x10..=0x15 | 0x18..=0x1d
            | 0x20..=0x25 | 0x28..=0x2d | 0x30..=0x35 | 0x38..=0x3d
            | 0x50..=0x61 | 0x68 | 0x6a | 0x80 | 0x81 | 0x83..=0x87 | 0x8f | 0x90..=0x97
            | 0xc2 | 0xc3 | 0xc8 | 0xc9 | 0xe0..=0xe3 | 0xe8 | 0xf6 | 0xf7 | 0xfe | 0xff)
    {
        return Err(if string_kind.is_some() || matches!(opcode, 0xfc | 0xfd) {
            DecodeError::Invalid
        } else {
            DecodeError::Unsupported
        });
    }
    if let Some(kind) = string_kind {
        let comparison = matches!(kind, StringKind::Cmps | StringKind::Scas);
        let repeat = match prefixes.repeat {
            None => Repeat::None,
            Some(0xf3) if comparison => Repeat::WhileEqual,
            Some(0xf2) if comparison => Repeat::WhileNotEqual,
            Some(0xf3) => Repeat::Count,
            _ => return Err(DecodeError::Unsupported),
        };
        return Ok(Operation::String {
            kind,
            width: byte_or_wide(opcode, prefixes.width()),
            address_width: prefixes.address_width(),
            source_segment: prefixes.segment_override.unwrap_or(Segment::Ds),
            repeat,
        });
    }
    if opcode == 0x0f {
        return escaped::decode(cursor.byte()?, prefixes, cursor);
    }
    if opcode == 0x90 && prefixes.repeat == Some(0xf3) {
        return Ok(Operation::Nop);
    }
    if prefixes.repeat.is_some() {
        return Err(DecodeError::Unsupported);
    }
    let rex = prefixes.rex;
    let wide = prefixes.width();
    let stack_width = prefixes.stack_width();
    let near_width = prefixes.near_width();
    match opcode {
        0x86 | 0x87 => {
            let width = byte_or_wide(opcode, wide);
            let operands = ModRm::read(cursor, rex)?;
            Ok(Operation::Exchange {
                dst: operands.location(cursor, prefixes, width)?,
                src: rex.view(operands.register, width),
                width,
            })
        }
        0x90..=0x97 => {
            let bank = (opcode - 0x90) | rex.base_extension();
            Ok(if bank == 0 {
                Operation::Nop
            } else {
                Operation::Exchange {
                    dst: Location::Register(RegisterView::low(bank)),
                    src: RegisterView::low(0),
                    width: wide,
                }
            })
        }
        0xfc | 0xfd => Ok(Operation::Direction {
            set: opcode == 0xfd,
        }),
        0x50..=0x57 => Ok(Operation::Push {
            src: Operand::Register((opcode - 0x50) | rex.base_extension()),
            width: stack_width,
        }),
        0x58..=0x5f => Ok(Operation::Pop {
            dst: (opcode - 0x58) | rex.base_extension(),
            width: stack_width,
        }),
        0x68 | 0x6a => Ok(Operation::Push {
            src: Operand::Immediate(immediate(cursor, stack_width, opcode == 0x6a)?),
            width: stack_width,
        }),
        0x8f => {
            let operands = ModRm::read(cursor, rex)?;
            if operands.selector != 0 {
                return Err(DecodeError::Unsupported);
            }
            Ok(match operands.location(cursor, prefixes, stack_width)? {
                Location::Register(register) => Operation::Pop {
                    dst: register.bank,
                    width: stack_width,
                },
                Location::Memory(address) => Operation::PopMemory {
                    address,
                    width: stack_width,
                },
            })
        }
        0xc9 => Ok(Operation::Leave { width: stack_width }),
        0xc8 => {
            let allocation = cursor.unsigned(2)? as u16;
            let nesting = cursor.byte()? & 31;
            Ok(Operation::CompoundStack(CompoundStack {
                kind: CompoundStackKind::Enter {
                    allocation,
                    nesting,
                },
                width: stack_width,
            }))
        }
        0x40..=0x4f => Ok(Operation::Unary {
            kind: if opcode < 0x48 {
                Unary::Inc
            } else {
                Unary::Dec
            },
            dst: Location::Register(RegisterView::low(opcode & 7)),
            width: wide,
        }),
        0x60 | 0x61 => {
            if prefixes.long64 {
                return Err(DecodeError::Invalid);
            }
            Ok(Operation::CompoundStack(CompoundStack {
                kind: if opcode == 0x60 {
                    CompoundStackKind::PushAll
                } else {
                    CompoundStackKind::PopAll
                },
                width: stack_width,
            }))
        }
        0x69 | 0x6b => {
            let operands = ModRm::read(cursor, rex)?;
            let src = operands.location(cursor, prefixes, wide)?;
            Ok(Operation::Imul {
                dst: rex.view(operands.register, wide),
                src,
                immediate: Some(immediate(cursor, wide, opcode == 0x6b)?),
                width: wide,
            })
        }
        0xe8 => {
            let displacement = relative_displacement(cursor, near_width)?;
            Ok(Operation::Call {
                target: Operand::Immediate(
                    cursor.next_pc().wrapping_add(displacement as u64) & near_width.mask(),
                ),
                width: near_width,
            })
        }
        0xe0..=0xe3 => {
            let displacement = i64::from(cursor.byte()? as i8);
            Ok(Operation::CountBranch {
                condition: match opcode {
                    0xe0 => CountCondition::NotEqual,
                    0xe1 => CountCondition::Equal,
                    0xe2 => CountCondition::Always,
                    _ => CountCondition::Zero,
                },
                count_width: prefixes.address_width(),
                target: cursor.next_pc().wrapping_add(displacement as u64) & near_width.mask(),
            })
        }
        0xff => {
            let operands = ModRm::read(cursor, rex)?;
            if operands.selector <= 1 {
                return Ok(Operation::Unary {
                    kind: if operands.selector == 0 {
                        Unary::Inc
                    } else {
                        Unary::Dec
                    },
                    dst: operands.location(cursor, prefixes, wide)?,
                    width: wide,
                });
            }
            match operands.selector {
                3 | 5 => decoder::far::memory(&operands, prefixes, cursor),
                2 => Ok(Operation::Call {
                    target: stack_operand(operands.location(cursor, prefixes, near_width)?),
                    width: near_width,
                }),
                4 => Ok(match operands.location(cursor, prefixes, near_width)? {
                    Location::Register(register) => Operation::JumpRegister {
                        target: register.bank,
                        width: near_width,
                    },
                    Location::Memory(address) => Operation::JumpMemory {
                        address,
                        width: near_width,
                    },
                }),
                6 => Ok(Operation::Push {
                    src: stack_operand(operands.location(cursor, prefixes, stack_width)?),
                    width: stack_width,
                }),
                _ => Err(DecodeError::Unsupported),
            }
        }
        0xc3 => Ok(Operation::Return {
            cleanup: 0,
            width: near_width,
        }),
        0xc2 => Ok(Operation::Return {
            cleanup: cursor.unsigned(2)? as u16,
            width: near_width,
        }),
        0xb0..=0xbf => {
            let (start, width) = if opcode < 0xb8 {
                (0xb0, Width::W8)
            } else {
                (0xb8, wide)
            };
            Ok(Operation::Mov {
                dst: rex.view((opcode - start) | rex.base_extension(), width),
                src: DataOperand::Immediate(cursor.unsigned(width.bytes())?),
                width,
            })
        }
        0x88..=0x8b => {
            let width = byte_or_wide(opcode, wide);
            let operands = ModRm::read(cursor, rex)?;
            let register = rex.view(operands.register, width);
            let location = operands.location(cursor, prefixes, width)?;
            if opcode & 2 == 0 {
                Ok(move_to(location, DataOperand::Register(register), width))
            } else {
                Ok(match location {
                    Location::Register(src) => Operation::Mov {
                        dst: register,
                        src: DataOperand::Register(src),
                        width,
                    },
                    Location::Memory(address) => Operation::Load {
                        dst: register,
                        address,
                        width,
                    },
                })
            }
        }
        0xc6 | 0xc7 => {
            let width = byte_or_wide(opcode, wide);
            let operands = ModRm::read(cursor, rex)?;
            if operands.selector != 0 {
                return Err(DecodeError::Unsupported);
            }
            let destination = operands.location(cursor, prefixes, width)?;
            let value = immediate(cursor, width, false)?;
            Ok(move_to(destination, DataOperand::Immediate(value), width))
        }
        0xa0..=0xa3 => {
            let width = byte_or_wide(opcode, wide);
            let address = Address {
                base: AddressBase::None,
                index: None,
                displacement: cursor.unsigned(prefixes.address_width().bytes())? as i64,
                size: prefixes.address_width(),
                segment: prefixes.segment_override.unwrap_or(Segment::Ds),
            };
            if opcode & 2 == 0 {
                Ok(Operation::Load {
                    dst: RegisterView::low(0),
                    address,
                    width,
                })
            } else {
                Ok(Operation::Store {
                    src: DataOperand::Register(RegisterView::low(0)),
                    address,
                    width,
                })
            }
        }
        0x8d => {
            let operands = ModRm::read(cursor, rex)?;
            if operands.mode == 3 {
                return Err(DecodeError::Invalid);
            }
            Ok(Operation::Lea {
                dst: rex.view(operands.register, wide),
                address: operands.address(cursor, prefixes)?,
                width: wide,
            })
        }
        0x00..=0x05
        | 0x08..=0x0d
        | 0x10..=0x15
        | 0x18..=0x1d
        | 0x20..=0x25
        | 0x28..=0x2d
        | 0x30..=0x35
        | 0x38..=0x3d
        | 0x84
        | 0x85 => {
            let width = byte_or_wide(opcode, wide);
            let kind = if matches!(opcode, 0x84 | 0x85) {
                Alu::Test
            } else {
                selector_kind((opcode >> 3) & 7).expect("admitted arithmetic opcode")
            };
            if opcode & 7 >= 4 && !matches!(opcode, 0x84 | 0x85) {
                return Ok(alu_to(
                    Location::Register(RegisterView::low(0)),
                    kind,
                    DataOperand::Immediate(immediate(cursor, width, false)?),
                    width,
                ));
            }
            let operands = ModRm::read(cursor, rex)?;
            let register = rex.view(operands.register, width);
            let location = operands.location(cursor, prefixes, width)?;
            if opcode & 2 == 0 || kind == Alu::Test {
                Ok(alu_to(
                    location,
                    kind,
                    DataOperand::Register(register),
                    width,
                ))
            } else {
                Ok(match location {
                    Location::Register(src) => Operation::Alu {
                        kind,
                        dst: register,
                        src: DataOperand::Register(src),
                        width,
                    },
                    Location::Memory(address) => Operation::MemoryAlu {
                        kind,
                        address,
                        operands: MemoryAluOperands::RegisterFirst(register),
                        width,
                    },
                })
            }
        }
        0x80 | 0x81 | 0x83 | 0xf6 | 0xf7 => {
            let width = if matches!(opcode, 0x80 | 0xf6) {
                Width::W8
            } else {
                wide
            };
            let operands = ModRm::read(cursor, rex)?;
            let kind = if matches!(opcode, 0xf6 | 0xf7) {
                match operands.selector {
                    0 => Alu::Test,
                    2 | 3 => {
                        return Ok(Operation::Unary {
                            kind: if operands.selector == 2 {
                                Unary::Not
                            } else {
                                Unary::Neg
                            },
                            dst: operands.location(cursor, prefixes, width)?,
                            width,
                        });
                    }
                    4 | 5 => {
                        return Ok(Operation::Multiply {
                            signed: operands.selector == 5,
                            src: operands.location(cursor, prefixes, width)?,
                            width,
                        });
                    }
                    6 | 7 => {
                        return Ok(Operation::Divide {
                            signed: operands.selector == 7,
                            src: operands.location(cursor, prefixes, width)?,
                            width,
                        });
                    }
                    _ => return Err(DecodeError::Unsupported),
                }
            } else {
                selector_kind(operands.selector).ok_or(DecodeError::Unsupported)?
            };
            let destination = operands.location(cursor, prefixes, width)?;
            let value = immediate(cursor, width, opcode == 0x83)?;
            Ok(alu_to(
                destination,
                kind,
                DataOperand::Immediate(value),
                width,
            ))
        }
        0xfe => {
            let operands = ModRm::read(cursor, rex)?;
            if operands.selector > 1 {
                return Err(DecodeError::Unsupported);
            }
            Ok(Operation::Unary {
                kind: if operands.selector == 0 {
                    Unary::Inc
                } else {
                    Unary::Dec
                },
                dst: operands.location(cursor, prefixes, Width::W8)?,
                width: Width::W8,
            })
        }
        0xf8 | 0xf9 | 0xf5 => Ok(Operation::Carry {
            kind: match opcode {
                0xf8 => Carry::Clear,
                0xf9 => Carry::Set,
                _ => Carry::Complement,
            },
        }),
        0x9f => Ok(Operation::Lahf),
        0x9e => Ok(Operation::Sahf),
        0xc0 | 0xc1 | 0xd0..=0xd3 => {
            let width = byte_or_wide(opcode, wide);
            let operands = ModRm::read(cursor, rex)?;
            let kind = match operands.selector {
                0 => Shift::Rol,
                1 => Shift::Ror,
                2 => Shift::Rcl,
                3 => Shift::Rcr,
                4 => Shift::Shl,
                5 => Shift::Shr,
                7 => Shift::Sar,
                _ => return Err(DecodeError::Unsupported),
            };
            let dst = operands.location(cursor, prefixes, width)?;
            let count = match opcode {
                0xc0 | 0xc1 => Count::Immediate(cursor.byte()?),
                0xd0 | 0xd1 => Count::Immediate(1),
                _ => Count::Cl,
            };
            Ok(Operation::Shift {
                kind,
                dst,
                count,
                width,
            })
        }
        0xa8 | 0xa9 => {
            let width = byte_or_wide(opcode, wide);
            Ok(alu_to(
                Location::Register(RegisterView::low(0)),
                Alu::Test,
                DataOperand::Immediate(immediate(cursor, width, false)?),
                width,
            ))
        }
        0x63 => {
            if !prefixes.long64 {
                return Err(if matches!(prefixes.mode, Mode::Real | Mode::Virtual8086) {
                    DecodeError::Invalid
                } else {
                    DecodeError::Unsupported
                });
            }
            let source_width = if wide == Width::W64 { Width::W32 } else { wide };
            let operands = ModRm::read(cursor, rex)?;
            Ok(Operation::Extend {
                dst: rex.view(operands.register, wide),
                src: operands.location(cursor, prefixes, source_width)?,
                source_width,
                width: wide,
                signed: true,
            })
        }
        0x98 => Ok(Operation::Extend {
            dst: RegisterView::low(0),
            src: Location::Register(RegisterView::low(0)),
            source_width: match wide {
                Width::W16 => Width::W8,
                Width::W32 => Width::W16,
                Width::W64 => Width::W32,
                Width::W8 => unreachable!(),
            },
            width: wide,
            signed: true,
        }),
        0x99 => Ok(Operation::SignHigh { width: wide }),
        0xeb | 0xe9 | 0x70..=0x7f => branch(opcode, prefixes, cursor),
        _ => Err(DecodeError::Unsupported),
    }
}

fn immediate<F>(cursor: &mut Cursor<F>, width: Width, short: bool) -> Result<u64, DecodeError>
where
    F: FnMut(u64) -> Result<u8, DecodeError>,
{
    let value = if short {
        cursor.byte()? as i8 as i64 as u64
    } else if width == Width::W64 {
        cursor.unsigned(4)? as u32 as i32 as i64 as u64
    } else {
        cursor.unsigned(width.bytes())?
    };
    Ok(value & width.mask())
}

fn move_to(dst: Location, src: DataOperand, width: Width) -> Operation {
    match dst {
        Location::Register(dst) => Operation::Mov { dst, src, width },
        Location::Memory(address) => Operation::Store {
            src,
            address,
            width,
        },
    }
}

fn alu_to(dst: Location, kind: Alu, src: DataOperand, width: Width) -> Operation {
    match dst {
        Location::Register(dst) => Operation::Alu {
            kind,
            dst,
            src,
            width,
        },
        Location::Memory(address) => Operation::MemoryAlu {
            kind,
            address,
            operands: MemoryAluOperands::MemoryFirst(src),
            width,
        },
    }
}

fn selector_kind(selector: u8) -> Option<Alu> {
    match selector {
        0 => Some(Alu::Add),
        1 => Some(Alu::Or),
        2 => Some(Alu::Adc),
        3 => Some(Alu::Sbb),
        4 => Some(Alu::And),
        5 => Some(Alu::Sub),
        6 => Some(Alu::Xor),
        7 => Some(Alu::Cmp),
        _ => None,
    }
}

fn stack_operand(location: Location) -> Operand {
    match location {
        Location::Register(register) => Operand::Register(register.bank),
        Location::Memory(address) => Operand::Memory(address),
    }
}
