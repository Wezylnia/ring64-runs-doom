use super::*;
use crate::x86::protection::tests::{context, image, mapping};

const A: u16 = 0x18;
const B: u16 = 0x20;
const CODE: u16 = 8;
const DATA: u16 = 16;
const A_BASE: u64 = 0x1200;
const B_BASE: u64 = 0x1300;
const GDT: u64 = 0x1000;

fn tss(base: u64, access: u8, limit: u16) -> u64 {
    u64::from(limit) | base << 16 | u64::from(access) << 40
}

const CODE_DESC: u64 = 0x0040_9a00_0000_ffff;
const DATA_DESC: u64 = 0x0040_9200_0000_ffff;

fn fixture() -> (ExecutionContext, Image) {
    let mut ctx = context("protected", 0);
    ctx.set_gdtr(TableRegister {
        base: GDT,
        limit: 0x2f,
    });
    ctx.set_tr(TaskRegister {
        selector: A,
        valid: true,
        base: A_BASE,
        limit: 0x67,
        attributes: 0x8b,
    });
    for segment in TSS_SEGMENTS {
        ctx.set_segment(
            segment,
            if segment == Segment::Cs { CODE } else { DATA },
            ctx.segment(segment),
            ctx.attributes(segment),
        );
    }
    let mut mem = image(vec![0xcd; 0x800], vec![mapping(GDT, 0, 0x800, true, true)]);
    for (selector, raw) in [
        (CODE, CODE_DESC),
        (DATA, DATA_DESC),
        (A, tss(A_BASE, 0x8b, 0x67)),
        (B, tss(B_BASE, 0x89, 0x67)),
    ] {
        mem.write(GDT + u64::from(selector), 8, raw).unwrap();
    }
    for (offset, val) in [
        (28, 0xabcd_0000),
        (32, 0x4567),
        (36, 0x202),
        (56, 0x1700),
        (72, DATA.into()),
        (76, CODE.into()),
        (80, DATA.into()),
        (84, DATA.into()),
        (88, 0),
        (92, 0),
        (96, 0),
        (100, 1),
    ] {
        mem.write(B_BASE + offset, 4, val).unwrap();
    }
    // CR3, LDTR and the T flag in A are static when A is abandoned.
    mem.write(A_BASE + 28, 4, 0x5700).unwrap();
    mem.write(A_BASE + 96, 4, 0).unwrap();
    mem.write(A_BASE + 100, 1, 0).unwrap();
    (ctx, mem)
}

fn direct() -> Start {
    Start::far(FarKind::Call, B, tss(B_BASE, 0x89, 0x67), 0x3210)
}

fn error_of(result: Result<Prepared, Error>) -> Error {
    result.err().expect("rejected task")
}

#[test]
fn nested_entry_snapshot_and_iret_save_only_dynamic_fields() {
    let (mut ctx, mut mem) = fixture();
    let regs = std::array::from_fn(|i| 0xff00_ab00_1234_5670 + i as u64);
    let prepared = prepare(&ctx, &mem, direct(), &regs, 2).unwrap();
    assert_eq!(prepared.cr3, 0xabcd_0000);
    assert_eq!(mem.read(A_BASE + 32, 4).unwrap(), 0xcdcd_cdcd);
    let b = prepared.commit(&mut ctx, &mut mem).unwrap();
    assert_eq!(b.eip, 0x4567);
    assert_eq!(b.flags, 0x4202);
    assert!(b.trap);
    assert_eq!(b.registers[4], 0x1700);
    assert_eq!(ctx.tr().selector, B);
    assert!(!ctx.segment(Segment::Cs).usable);
    assert_eq!(mem.read(B_BASE, 4).unwrap(), 0xcdcd_0018);
    assert_eq!(mem.read(GDT + u64::from(B) + 5, 1).unwrap(), 0x8b);
    assert_eq!(mem.read(GDT + u64::from(A) + 5, 1).unwrap(), 0x8b);
    assert_eq!(mem.read(A_BASE + 24, 8).unwrap(), 0x0000_5700_cdcd_cdcd);
    assert_eq!(mem.read(A_BASE + 32, 8).unwrap(), 0x0000_0002_0000_3210);
    for (i, value) in regs[..8].iter().enumerate() {
        assert_eq!(
            mem.read(A_BASE + 40 + 4 * i as u64, 4).unwrap(),
            *value & 0xffff_ffff
        );
    }
    for (i, expected) in [DATA, CODE, DATA, DATA, DATA, DATA].into_iter().enumerate() {
        assert_eq!(
            mem.read(A_BASE + 72 + 4 * i as u64, 4).unwrap(),
            u64::from(expected)
        );
    }
    assert_eq!(mem.read(A_BASE + 100, 4).unwrap(), 0xcdcd_cd00);
    let mut esp = 0x1700;
    b.qualify(&mut ctx, &mut mem, &mut esp).unwrap();
    assert_eq!(mem.read(GDT + u64::from(CODE) + 5, 1).unwrap(), 0x9b);
    assert!(!ctx.segment(Segment::Fs).usable);
    assert_eq!(esp, 0x1700);
    let back = prepare(
        &ctx,
        &mem,
        Start::nested_return(0x456a),
        &[0x55; 16],
        0x4202,
    )
    .unwrap();
    assert_eq!(back.cr3, 0x5700);
    let a = back.commit(&mut ctx, &mut mem).unwrap();
    assert_eq!(
        (a.eip, a.flags, a.registers[0], ctx.tr().selector),
        (0x3210, 2, regs[0] as u32, A)
    );
    assert_eq!(mem.read(B_BASE + 32, 8).unwrap(), 0x0000_0202_0000_456a);
    assert_eq!(mem.read(B_BASE, 4).unwrap(), 0xcdcd_0018);
    assert_eq!(mem.read(GDT + u64::from(B) + 5, 1).unwrap(), 0x89);
    assert_eq!(mem.read(GDT + u64::from(A) + 5, 1).unwrap(), 0x8b);
}

#[test]
fn rejected_targets_leave_the_source_task_and_images_intact() {
    let (ctx, mut mem) = fixture();
    for (selector, desc, vector, code) in [
        (0, tss(B_BASE, 0x89, 0x67), 13, 0),
        (B | 4, tss(B_BASE, 0x89, 0x67), 13, 0x24),
        (0x38, tss(B_BASE, 0x89, 0x67), 13, 0x38),
        (B, tss(B_BASE, 0x8b, 0x67), 13, B),
        (B, tss(B_BASE, 0x09, 0x67), 11, B),
        (B, tss(B_BASE, 0x89, 0x66), 10, B),
        (B | 3, tss(B_BASE, 0x89, 0x67), 13, B),
    ] {
        let err = error_of(prepare(
            &ctx,
            &mem,
            Start::far(FarKind::Call, selector, desc, 0x80),
            &[0; 16],
            2,
        ));
        assert!(
            matches!(err,Error::Fault(Failure::Protection(f)) if (f.vector,f.error_code)==(vector,code)),
            "{err:?}"
        );
    }
    assert_eq!(
        error_of(prepare(
            &ctx,
            &mem,
            Start::far(FarKind::Call, B, tss(B_BASE, 0x81, 43), 4),
            &[0; 16],
            2
        )),
        Error::Unsupported
    );
    mem.write(B_BASE + 36, 4, 0x20002).unwrap();
    assert_eq!(
        error_of(prepare(&ctx, &mem, direct(), &[0; 16], 2)),
        Error::Unsupported
    );
    assert_eq!(ctx.tr().selector, A);
    assert_eq!(mem.read(A_BASE + 32, 4).unwrap(), 0xcdcd_cdcd);
    assert_eq!(mem.read(B_BASE, 2).unwrap(), 0xcdcd);
}

#[test]
fn late_selector_faults_preserve_incoming_visible_state_and_external_provenance() {
    for (slot, selector, raw, vector) in [
        (96, 0x28, 0x02u64 << 40, 10),
        (76, 0x28, DATA_DESC, 10),
        (76, 0x28, CODE_DESC & !(0x80 << 40), 11),
        (80, 0x28, CODE_DESC, 10),
        (80, 0x28, DATA_DESC & !(0x80 << 40), 12),
        (84, 0x28, tss(B_BASE, 0x89, 0x67), 10),
        (84, 0x28, DATA_DESC & !(0x80 << 40), 11),
    ] {
        let (mut ctx, mut mem) = fixture();
        mem.write(GDT + selector, 8, raw).unwrap();
        mem.write(B_BASE + slot, 2, selector).unwrap();
        let stored = prepare(
            &ctx,
            &mem,
            Start::gate(B, 0x7612, 0x10202, 1, Some(0x29)),
            &[9; 16],
            2,
        )
        .unwrap();
        let b = stored.commit(&mut ctx, &mut mem).unwrap();
        let mut sp = u64::from(b.registers[4]);
        let err = b.qualify(&mut ctx, &mut mem, &mut sp).unwrap_err();
        assert!(
            matches!(err,Error::Fault(Failure::Protection(f)) if (f.vector,f.error_code)==(vector,0x29)),
            "{err:?}"
        );
        assert_eq!((b.eip, ctx.tr().selector, sp), (0x4567, B, 0x1700));
        assert_eq!(mem.read(A_BASE + 32, 8).unwrap(), 0x0001_0202_0000_7612);
        assert_eq!(mem.read(0x16fc, 4).unwrap(), 0xcdcd_cdcd);
        if slot == 84 {
            assert_eq!(ctx.selector(Segment::Ds), 0x28);
            assert!(!ctx.segment(Segment::Ds).usable);
            assert!(ctx.segment(Segment::Cs).usable);
            assert!(ctx.segment(Segment::Ss).usable);
        }
    }
}

#[test]
fn idt_error_push_precedes_eip_limit_and_respects_stack_width() {
    for (sp, limit, expected) in [(0xffff_1700, 0xffff, 13), (0x1700, 0x10, 12)] {
        let (mut ctx, mut mem) = fixture();
        mem.write(B_BASE + 56, 4, sp).unwrap();
        mem.write(GDT + u64::from(DATA), 8, 0x0000_9200_0000_0000 | limit)
            .unwrap();
        mem.write(GDT + u64::from(CODE), 8, CODE_DESC & !0xffff | 0x40)
            .unwrap();
        let b = prepare(
            &ctx,
            &mem,
            Start::gate(B, 0xa, 2, 1, Some(0x5bad)),
            &[0; 16],
            2,
        )
        .unwrap()
        .commit(&mut ctx, &mut mem)
        .unwrap();
        let mut cursor = sp;
        let err = b.qualify(&mut ctx, &mut mem, &mut cursor).unwrap_err();
        assert!(
            matches!(err,Error::Fault(Failure::Protection(f)) if (f.vector,f.error_code)==(expected,1)),
            "{err:?}"
        );
        if expected == 13 {
            assert_eq!(cursor, 0xffff_16fc);
            assert_eq!(mem.read(0x16fc, 4).unwrap(), 0x5bad);
        } else {
            assert_eq!(cursor, sp);
            assert_eq!(mem.read(0x16fc, 4).unwrap(), 0xcdcd_cdcd);
        }
    }
}

#[test]
fn old_root_write_refusal_publishes_no_earlier_dynamic_or_backlink_fields() {
    let (ctx, data) = fixture();
    let original = data.read_bytes(GDT, 0x800).unwrap();
    for (offset, width, phase, table, selector) in [
        (0x25, 8, SystemPhase::BusyWrite, Table::Gdt, B),
        (0x25c, 4, SystemPhase::TssWrite, Table::Tss, A),
        (0x301, 2, SystemPhase::TssWrite, Table::Tss, B),
    ] {
        let mem = image(
            original.clone(),
            vec![
                mapping(GDT, 0, offset, true, true),
                mapping(GDT + u64::from(offset), offset, 1, true, false),
                mapping(
                    GDT + u64::from(offset + 1),
                    offset + 1,
                    0x800 - offset - 1,
                    true,
                    true,
                ),
            ],
        );
        let err = error_of(prepare(&ctx, &mem, direct(), &[0; 16], 2));
        assert!(
            matches!(err, Error::Fault(Failure::SystemMemory(f)) if (f.phase,f.width,f.table,f.reference,f.failing_address)==(phase,width,table,SystemReference::Selector(selector),u128::from(GDT)+u128::from(offset))),
            "{err:?}"
        );
        assert_eq!(mem.read_bytes(GDT, 0x800).unwrap(), original);
        assert_eq!(ctx.tr().selector, A);
    }
}

#[test]
fn qualify_reads_new_root_ldt_and_conforming_code_then_reuses_completed_caches() {
    let (mut ctx, mut old) = fixture();
    let (_, mut new) = fixture();
    let ldt = 0x1500;
    new.write(GDT + 0x28, 8, tss(ldt, 0x82, 0x3f)).unwrap();
    new.write(ldt + 8, 8, CODE_DESC | 4 << 40).unwrap();
    new.write(ldt + 16, 8, DATA_DESC | 3 << 45).unwrap();
    for (at, selector) in [(72, 0), (76, 0x0f), (80, 0x17), (84, 0x1f), (96, 0x28)] {
        old.write(B_BASE + at, 2, selector).unwrap();
    }
    // DS references one missing descriptor. Vary the backing at this single
    // access to exercise resume in the already committed incoming context.
    let payload = new.read_bytes(GDT, 0x800).unwrap();
    let mut partial = image(
        payload,
        vec![
            mapping(GDT, 0, 0x518, true, true),
            mapping(0x1520, 0x520, 0x2e0, true, true),
        ],
    );
    let b = prepare(&ctx, &old, direct(), &[0; 16], 2)
        .unwrap()
        .commit(&mut ctx, &mut old)
        .unwrap();
    let mut esp = u64::from(b.registers[4]);
    assert!(
        matches!(b.qualify(&mut ctx,&mut partial,&mut esp),Err(Error::Fault(Failure::SystemMemory(f))) if f.table==Table::Ldt && f.phase==SystemPhase::DescriptorRead && f.reference==SystemReference::Selector(0x1f))
    );
    assert_eq!(ctx.cpl(), 3);
    assert_eq!(ctx.ldtr().base, ldt);
    assert!(ctx.segment(Segment::Cs).usable);
    assert!(ctx.attributes(Segment::Cs).conforming);
    assert_eq!(ctx.attributes(Segment::Cs).dpl, 0);
    assert!(ctx.segment(Segment::Ss).usable);
    assert_eq!(partial.read(ldt + 13, 1).unwrap(), 0x9f);
    assert_eq!(partial.read(ldt + 21, 1).unwrap(), 0xf3);
    // Previously used tables and descriptors would now fail if reread.
    new.write(GDT + 0x28, 8, 0).unwrap();
    new.write(ldt + 8, 8, 0).unwrap();
    new.write(ldt + 16, 8, 0).unwrap();
    new.write(ldt + 24, 8, DATA_DESC | 3 << 45).unwrap();
    b.qualify(&mut ctx, &mut new, &mut esp).unwrap();
    assert!(ctx.segment(Segment::Ds).usable);
    assert_eq!(new.read(ldt + 29, 1).unwrap(), 0xf3);
    assert_eq!(old.read(ldt + 13, 1).unwrap(), 0xcd);
}

#[test]
fn old_tss_overwriting_its_own_page_table_publishes_captured_spans_only_after_all_dirty_admissions()
{
    use crate::memory::MemoryLimits;
    use crate::x86::paging::{
        State,
        access::{AccessState, Memory},
        walk,
    };
    use crate::x86::protection_keys::State as Keys;
    for deny_last_dirty in [false, true] {
        let (mut ctx, data) = fixture();
        // A's saved EIP overwrites PTE[9], the entry mapping the rest of A's
        // outgoing fields. B and the GDT are on a separate page.
        ctx.set_tr(TaskRegister {
            base: 0x9004,
            ..ctx.tr()
        });
        let mut bytes = vec![0; 0x5000];
        bytes[0x1000..0x1004].copy_from_slice(&0x2023u32.to_le_bytes());
        bytes[0x2004..0x2008].copy_from_slice(&0x4023u32.to_le_bytes());
        bytes[0x2024..0x2028].copy_from_slice(&0x2023u32.to_le_bytes());
        bytes[0x4000..0x4800].copy_from_slice(&data.read_bytes(GDT, 0x800).unwrap());
        let mut image = Image::physical_mapped(
            vec![("data".into(), bytes)],
            vec![
                mapping(0, 0, 0x2004, true, true),
                mapping(0x2004, 0x2004, 4, true, !deny_last_dirty),
                mapping(0x2008, 0x2008, 0x2ff8, true, true),
            ],
            MemoryLimits {
                max_backing_bytes: 0x5000,
                max_backings: 1,
                max_mappings: 3,
            },
        )
        .unwrap();
        let paging = State {
            cr0: 0x8001_0011,
            cr3: 0x1000,
            ..State::reset()
        };
        let mut accesses = AccessState::default();
        let mut memory = Memory::new(&mut image, &paging, 0, 2, &mut accesses, &Keys::ZERO);
        let prepared = prepare(&ctx, &memory, direct(), &[0x5555; 16], 2).unwrap();
        let result = prepared.commit(&mut ctx, &mut memory);
        if deny_last_dirty {
            assert!(
                matches!(
                    result,
                    Err(Error::Fault(Failure::SystemPaging(SystemPagingFault {
                        phase: SystemPhase::TssWrite,
                        table: Table::Tss,
                        reference: SystemReference::Selector(B),
                        failure: walk::Failure::Physical(walk::PhysicalFault {
                            phase: walk::PhysicalPhase::DirtyWrite,
                            ..
                        })
                    })))
                ),
                "{result:?}"
            );
            assert_eq!(ctx.tr().selector, A);
            assert_eq!(image.physical_read(0x2024, 4).unwrap(), 0x2023);
            assert_eq!(image.physical_read(0x2028, 4).unwrap(), 0);
            assert_eq!(image.physical_read(0x4300, 2).unwrap(), 0xcdcd);
            assert_eq!(image.physical_read(0x4025, 1).unwrap(), 0x89);
        } else {
            assert_eq!(result.unwrap().eip, 0x4567);
            assert_eq!(image.physical_read(0x2024, 4).unwrap(), 0x3210);
            assert_eq!(image.physical_read(0x2028, 4).unwrap(), 2);
            assert_eq!(image.physical_read(0x202c, 4).unwrap(), 0x5555);
            assert_eq!(image.physical_read(0x2060, 4).unwrap(), u64::from(DATA));
            assert_eq!(image.physical_read(0x4300, 2).unwrap(), u64::from(A));
            assert_eq!(image.physical_read(0x4025, 1).unwrap(), 0x8b);
        }
    }
}

fn task_gate(target: u16, access: u8) -> u64 {
    u64::from(target) << 16 | u64::from(access) << 40
}

#[test]
fn task_jump_preserves_both_nt_images_and_both_backlinks_while_handing_off_busy() {
    for direct in [true, false] {
        for old_nt in [0, NT] {
            for new_nt in [0, NT] {
                let (mut ctx, mut mem) = fixture();
                mem.write(B_BASE + 36, 4, (2 | new_nt).into()).unwrap();
                let (selector, descriptor) = if direct {
                    (B, tss(B_BASE, 0x89, 0x67))
                } else {
                    (0x28, task_gate(B | 3, 0x85))
                };
                let b = prepare(
                    &ctx,
                    &mem,
                    Start::far(FarKind::Jump, selector, descriptor, 0x3210),
                    &[0; 16],
                    (2 | old_nt).into(),
                )
                .unwrap()
                .commit(&mut ctx, &mut mem)
                .unwrap();
                assert_eq!(b.flags, 2 | new_nt);
                assert_eq!(ctx.tr().selector, if direct { B } else { B | 3 });
                assert_eq!(mem.read(A_BASE + 32, 4).unwrap(), 0x3210);
                assert_eq!(mem.read(A_BASE + 36, 4).unwrap(), u64::from(2 | old_nt));
                assert_eq!(mem.read(GDT + u64::from(A) + 5, 1).unwrap(), 0x89);
                assert_eq!(mem.read(GDT + u64::from(B) + 5, 1).unwrap(), 0x8b);
                for base in [A_BASE, B_BASE] {
                    assert_eq!(mem.read(base, 4).unwrap(), 0xcdcd_cdcd);
                }
            }
        }
    }
}

#[test]
fn only_the_outer_task_gate_controls_privilege_and_presence_before_target_or_tr() {
    const GATE: u16 = 0x2c; // Already captured by the far owner from the LDT.
    for kind in [FarKind::Call, FarKind::Jump] {
        for (cpl, outer, access, vector) in [
            (3, GATE, 0x45, 13),
            (0, GATE | 3, 0xc5, 13),
            (3, GATE | 3, 0x65, 11),
        ] {
            let (mut ctx, mem) = fixture();
            ctx.set_mode_and_cpl(Mode::Protected, cpl);
            ctx.set_tr(TaskRegister::default());
            let err = error_of(prepare(
                &ctx,
                &mem,
                Start::far(kind, outer, task_gate(0xffff, access), 4),
                &[0; 16],
                2,
            ));
            assert!(
                matches!(err,Error::Fault(Failure::Protection(f)) if (f.vector,f.error_code,f.selector)==(vector,GATE,Some(outer))),
                "{err:?}"
            );
        }
        for (inner, access, vector) in [
            (0, 0x89, 13),
            (B | 4, 0x89, 13),
            (0x40, 0x89, 13),
            (B | 3, 0x92, 13),
            (B | 3, 0x0b, 13),
            (B | 3, 0x09, 11),
        ] {
            let (mut ctx, mut mem) = fixture();
            ctx.set_mode_and_cpl(Mode::Protected, 3);
            mem.write(GDT + u64::from(B), 8, tss(B_BASE, access, 0x67))
                .unwrap();
            let err = error_of(prepare(
                &ctx,
                &mem,
                Start::far(kind, GATE | 3, task_gate(inner, 0xe5), 4),
                &[0; 16],
                2,
            ));
            assert!(
                matches!(err,Error::Fault(Failure::Protection(f)) if (f.vector,f.error_code,f.selector)==(vector,inner&!3,Some(inner))),
                "{err:?}"
            );
        }
    }
}

#[test]
fn gated_call_from_cpl3_can_enter_dpl0_tss_and_nested_return_through_its_backlink() {
    let (mut ctx, mut mem) = fixture();
    ctx.set_mode_and_cpl(Mode::Protected, 3);
    ctx.set_gdtr(TableRegister {
        limit: 0x3f,
        ..ctx.gdtr()
    });
    mem.write(GDT + 0x30, 8, CODE_DESC | 3 << 45).unwrap();
    mem.write(GDT + 0x38, 8, DATA_DESC | 3 << 45).unwrap();
    for segment in TSS_SEGMENTS {
        ctx.set_segment(
            segment,
            if segment == Segment::Cs { 0x33 } else { 0x3b },
            ctx.segment(segment),
            ctx.attributes(segment),
        );
    }
    let gated = Start::far(FarKind::Call, 0x2f, task_gate(B | 3, 0xe5), 0x1234);
    let b = prepare(&ctx, &mem, gated, &[7; 16], 2)
        .unwrap()
        .commit(&mut ctx, &mut mem)
        .unwrap();
    assert_eq!((ctx.cpl(), ctx.tr().selector, b.flags & NT), (0, B | 3, NT));
    assert_eq!(mem.read(B_BASE, 4).unwrap(), 0xcdcd_0018);
    assert_eq!(mem.read(GDT + u64::from(A) + 5, 1).unwrap(), 0x8b);
    assert_eq!(mem.read(GDT + u64::from(B) + 5, 1).unwrap(), 0x8b);
    b.qualify(&mut ctx, &mut mem, &mut 0x1700).unwrap();
    let a = prepare(
        &ctx,
        &mem,
        Start::nested_return(0x5678),
        &[9; 16],
        b.flags.into(),
    )
    .unwrap()
    .commit(&mut ctx, &mut mem)
    .unwrap();
    assert_eq!(
        (a.eip, a.flags, ctx.tr().selector, ctx.cpl()),
        (0x1234, 2, A, 3)
    );
    assert_eq!(mem.read(GDT + u64::from(B) + 5, 1).unwrap(), 0x89);
    assert_eq!(mem.read(B_BASE + 36, 4).unwrap() & u64::from(NT), 0);
    assert_eq!(mem.read(B_BASE, 4).unwrap(), 0xcdcd_0018);
    a.qualify(&mut ctx, &mut mem, &mut u64::from(a.registers[4]))
        .unwrap();
    assert_eq!(ctx.selector(Segment::Cs), 0x33);
}

#[test]
fn task_jump_requires_both_busy_writes_and_no_access_to_either_backlink() {
    let (mut ctx, source) = fixture();
    let initial = source.read_bytes(GDT, 0x800).unwrap();
    let jump = Start::far(FarKind::Jump, B, tss(B_BASE, 0x89, 0x67), 0x1234);
    for selector in [A, B] {
        let at = u32::from(selector) + 5;
        let denied = image(
            initial.clone(),
            vec![
                mapping(GDT, 0, at, true, true),
                mapping(GDT + u64::from(at), at, 1, true, false),
                mapping(GDT + u64::from(at) + 1, at + 1, 0x800 - at - 1, true, true),
            ],
        );
        let err = error_of(prepare(&ctx, &denied, jump, &[0; 16], 2));
        assert!(
            matches!(err,Error::Fault(Failure::SystemMemory(f)) if (f.phase,f.reference)==(SystemPhase::BusyWrite,SystemReference::Selector(selector))),
            "{err:?}"
        );
        assert_eq!(denied.read_bytes(GDT, 0x800).unwrap(), initial);
        assert_eq!(ctx.tr().selector, A);
    }
    let mut absent = image(
        initial,
        vec![
            mapping(GDT, 0, 0x200, true, true),
            mapping(A_BASE + 4, 0x204, 0xfc, true, true),
            mapping(B_BASE + 4, 0x304, 0x4fc, true, true),
        ],
    );
    prepare(&ctx, &absent, jump, &[0; 16], 2)
        .unwrap()
        .commit(&mut ctx, &mut absent)
        .unwrap();
    assert_eq!(ctx.tr().selector, B);
}

#[test]
fn jmp_from_nested_task_preserves_the_incoming_tasks_chosen_return_link() {
    const C: u16 = 0x28;
    const C_BASE: u64 = 0x1400;
    let (mut ctx, mut mem) = fixture();
    let template = mem.read_bytes(B_BASE, 0x68).unwrap();
    mem.write_bytes(C_BASE, &template).unwrap();
    let c_descriptor = tss(C_BASE, 0x89, 0x67);
    mem.write(GDT + u64::from(C), 8, c_descriptor).unwrap();
    mem.write(C_BASE, 2, u64::from(A)).unwrap();
    mem.write(C_BASE + 36, 4, u64::from(2 | NT)).unwrap();

    let b = prepare(&ctx, &mem, direct(), &[7; 16], 2)
        .unwrap()
        .commit(&mut ctx, &mut mem)
        .unwrap();
    b.qualify(&mut ctx, &mut mem, &mut u64::from(b.registers[4]))
        .unwrap();
    let c = prepare(
        &ctx,
        &mem,
        Start::far(FarKind::Jump, C, c_descriptor, 0x6789),
        &[8; 16],
        b.flags.into(),
    )
    .unwrap()
    .commit(&mut ctx, &mut mem)
    .unwrap();
    assert_eq!(
        mem.read(B_BASE + 36, 4).unwrap() & u64::from(NT),
        u64::from(NT)
    );
    assert_eq!(mem.read(B_BASE + 32, 4).unwrap(), 0x6789);
    assert_eq!(mem.read(GDT + u64::from(B) + 5, 1).unwrap(), 0x89);
    assert_eq!(mem.read(C_BASE, 4).unwrap(), 0xcdcd_0018);
    assert_ne!(c.flags & NT, 0);
    c.qualify(&mut ctx, &mut mem, &mut u64::from(c.registers[4]))
        .unwrap();
    let a = prepare(
        &ctx,
        &mem,
        Start::nested_return(0x456f),
        &[9; 16],
        c.flags.into(),
    )
    .unwrap()
    .commit(&mut ctx, &mut mem)
    .unwrap();
    assert_eq!(
        (a.eip, a.flags, a.registers[0], ctx.tr().selector),
        (0x3210, 2, 7, A)
    );
    assert_eq!(mem.read(GDT + u64::from(C) + 5, 1).unwrap(), 0x89);
    assert_eq!(mem.read(GDT + u64::from(A) + 5, 1).unwrap(), 0x8b);
}

#[test]
fn incoming_busy_dirty_refusal_keeps_outgoing_busy_image_and_dirty_bit_unchanged() {
    use crate::memory::MemoryLimits;
    use crate::x86::paging::{
        State,
        access::{AccessState, Memory},
        walk,
    };
    use crate::x86::protection_keys::State as Keys;
    let (mut ctx, fixture) = fixture();
    // GDT[A] ends the first virtual page, GDT[B] begins the next.
    ctx.set_gdtr(TableRegister {
        base: 0x1fe0,
        limit: 0x2f,
    });
    let mut bytes = vec![0; 0x6000];
    for (at, entry) in [(0x1000, 0x2023u32), (0x2004, 0x4023), (0x2008, 0x5023)] {
        bytes[at..at + 4].copy_from_slice(&entry.to_le_bytes());
    }
    bytes[0x4200..0x4400].copy_from_slice(&fixture.read_bytes(A_BASE, 0x200).unwrap());
    let entries = fixture.read_bytes(GDT + u64::from(A), 16).unwrap();
    bytes[0x4ff8..0x5008].copy_from_slice(&entries);
    let mut image = Image::physical_mapped(
        vec![("data".into(), bytes)],
        vec![
            mapping(0, 0, 0x2008, true, true),
            mapping(0x2008, 0x2008, 4, true, false),
            mapping(0x200c, 0x200c, 0x3ff4, true, true),
        ],
        MemoryLimits {
            max_backing_bytes: 0x6000,
            max_backings: 1,
            max_mappings: 3,
        },
    )
    .unwrap();
    let paging = State {
        cr0: 0x8001_0011,
        cr3: 0x1000,
        ..State::reset()
    };
    let mut accesses = AccessState::default();
    let mut memory = Memory::new(&mut image, &paging, 0, 2, &mut accesses, &Keys::ZERO);
    let prepared = prepare(
        &ctx,
        &memory,
        Start::far(FarKind::Jump, B, tss(B_BASE, 0x89, 0x67), 0x1234),
        &[0; 16],
        2,
    )
    .unwrap();
    assert!(matches!(
        prepared.commit(&mut ctx, &mut memory),
        Err(Error::Fault(Failure::SystemPaging(SystemPagingFault {
            phase: SystemPhase::BusyWrite,
            table: Table::Gdt,
            reference: SystemReference::Selector(B),
            failure: walk::Failure::Physical(walk::PhysicalFault {
                phase: walk::PhysicalPhase::DirtyWrite,
                ..
            })
        })))
    ));
    assert_eq!(ctx.tr().selector, A);
    assert_eq!(image.physical_read(0x4220, 4).unwrap(), 0xcdcd_cdcd);
    assert_eq!(image.physical_read(0x4ffd, 1).unwrap(), 0x8b);
    assert_eq!(image.physical_read(0x5005, 1).unwrap(), 0x89);
    assert_eq!(image.physical_read(0x2004, 4).unwrap(), 0x4023);
    assert_eq!(image.physical_read(0x2008, 4).unwrap(), 0x5023);
}
