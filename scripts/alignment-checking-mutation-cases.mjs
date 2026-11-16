const access = 'src/x86/paging/access.rs';
const intent = 'src/x86/alignment.rs';
const exceptions = 'src/x86/exception.rs';
const controls = 'src/x86/paging/controls.rs';
const frameGate = 'alignment_uses_current_controls_and_prospective_frame_privilege';
const granular = 'semantic operand granules suppress all candidate numeric and RMW state';
const edit = (file, old, replacement) => ({ file, old, replacement });
const native = (name, filter, ...edits) => ({ name, kind: 'native', filter, edits });
const publicCpu = (name, filter, ...edits) => ({ name, kind: 'public', filter, edits });
const gate = `        let prepared = self.prepare(address, width, access, rmw, class)?;
        if self.alignment_controls
            && class.is_user()
            && let Err(fault) = intent.check(address, width, access)
        {
            self.storage.borrow_mut().work.clear();
            return Err(AccessFailure::AlignmentCheck(fault));
        }
        Ok(prepared)`;
export const mutants = [
  native(
    'new-ac-retains-older-mandatory-alignment-report',
    'alignment_report_bank_switches_classes_without_resurrecting_an_older_fault',
    edit(
      'src/embedding/exports/context.rs',
      `            AccessFailure::AlignmentCheck(fault) => {
                self.alignment_fault = None;
                self.alignment_check = Some(fault);
            }`,
      `            AccessFailure::AlignmentCheck(fault) => {
                self.alignment_check = Some(fault);
            }`,
    ),
  ),
  native(
    'natural-check-uses-last-fragment-start',
    'split_page_alignment_retains_accessed_but_never_dirty_or_pending_payload',
    edit(
      access,
      '            && let Err(fault) = intent.check(address, width, access)',
      '            && let Err(fault) = intent.check(prepared.fragments.last().unwrap().request.linear, width, access)',
    ),
  ),
  publicCpu(
    'aligned-effective-offset-suppresses-linear-natural-check',
    'final segmented linear address reverses effective-offset residues',
    edit(
      'src/embedding/exports/context.rs',
      '        match guest_access!(core, PAGED, read(address, width)) {',
      `        match guest_access!(core, PAGED,
            read_aligned(address, width, if offset & u64::from(width - 1) == 0 {
                crate::x86::alignment::Intent::Exempt
            } else { crate::x86::alignment::Intent::Natural(width) })) {`,
    ),
  ),
  native(
    'nominal-extent-replaces-natural-granule',
    'component_alignment_follows_complete_record_admission_and_reports_its_field',
    edit(
      intent,
      '            Self::Natural(alignment) => check(address, width, alignment),',
      `            Self::Natural(_alignment) => {
                if address % u64::from(width) == 0 { Ok(()) }
                else { Err(Fault { address, width, access, alignment: width }) }
            }`,
    ),
  ),
  native(
    'rmw-reads-payload-after-dirty-publication',
    'rmw_retains_its_leaf_and_payload_may_clear_the_planned_dirty_bit',
    edit(
      access,
      `        let prepared =
            self.prepare_aligned(address, width, Access::Write, true, self.class, intent)?;
        let mut bytes = [0; 8];`,
      `        let prepared =
            self.prepare_aligned(address, width, Access::Write, true, self.class, intent)?;
        let updates = self.dirty_admission(&prepared, &[(0, width)])?;
        self.dirty_publish(&updates);
        let mut bytes = [0; 8];`,
    ),
  ),
  native(
    'dirty-publication-overwrites-later-payload',
    'rmw_retains_its_leaf_and_payload_may_clear_the_planned_dirty_bit',
    edit(
      access,
      `        if !prepared.published {
            let updates = self.dirty_admission(&prepared, fields)?;
            self.dirty_publish(&updates);
        }
        let mut storage = self.storage.borrow_mut();`,
      `        let deferred_updates = if !prepared.published {
            self.dirty_admission(&prepared, fields)?
        } else { Vec::new() };
        let mut storage = self.storage.borrow_mut();`,
    ),
    edit(
      access,
      `        }
        Ok(())
    }
}

fn participates`,
      `        }
        drop(storage);
        self.dirty_publish(&deferred_updates);
        Ok(())
    }
}

fn participates`,
    ),
  ),
  publicCpu(
    'x87-store-candidate-published-before-operand-admission',
    granular,
    edit(
      'src/embedding/exports/x87.rs',
      '        let mut address = None;\n        let source = if width == 0 {',
      `        if operation.is_store() {
            unsafe { let state = &mut *core.state.get(); let _ = x87::eval(&mut state.x87, operation, 0, flags); }
        }
        let mut address = None;
        let source = if width == 0 {`,
    ),
  ),
  native(
    'pg-incorrectly-required',
    frameGate,
    edit(
      access,
      'alignment_controls: ac && paging.cr0 & 0x40001 == 0x40001,',
      'alignment_controls: ac && paging.cr0 & 0x40001 == 0x40001 && paging.cr0 & 0x80000000 != 0,',
    ),
  ),
  native(
    'cpl-one-and-two-treated-as-user',
    frameGate,
    edit(
      access,
      '            class: AccessClass::operand(cpl, ac),',
      '            class: AccessClass::operand(if matches!(cpl, 1 | 2) { 3 } else { cpl }, ac),',
    ),
  ),
  native(
    'am-reloads-pdpte',
    'alignment_mask_is_admitted_without_translation_or_pdpte_effects',
    edit(
      controls,
      '                0 => (self.cr0 ^ candidate.cr0) & 0xe0000000 != 0,',
      '                0 => (self.cr0 ^ candidate.cr0) & 0xe0040000 != 0,',
    ),
  ),
  native(
    'accessed-write-discarded-before-ac',
    'split_page_alignment_retains_accessed_but_never_dirty_or_pending_payload',
    edit(
      'src/x86/paging/walk.rs',
      '            image.physical_write(address, width, current_word | ACCESSED)?;',
      '            let _ = (address, width, current_word | ACCESSED);',
    ),
  ),
  native(
    'a-only-source-owner-invalidation-skipped',
    'alignment_fault_retains_accessed_source_owner_invalidation_without_payload',
    edit(
      'src/memory/validity.rs',
      '                owner.valid = false;',
      '                owner.valid = true;',
    ),
  ),
  native(
    'pending-ac-accepts-foreign-error-codes',
    'alignment_exception_checkpoint_accepts_only_its_zero_or_ext_error_slot',
    edit(
      exceptions,
      '            17 => matches!(self.error_code, Some(0 | 1)),',
      '            17 => self.error_code.is_some(),',
    ),
  ),
  native(
    'captured-ac-rechecks-disabled-live-gate',
    'alignment_staging_uses_captured_fault_even_after_eligibility_is_cleared',
    edit(
      'src/embedding/exports/exception_delivery.rs',
      `            ALIGNMENT_CHECK => {
                if core.alignment_check.is_none() {`,
      `            ALIGNMENT_CHECK => {
                if state.flags & (1 << 18) == 0 || core.paging.is_none_or(|bank| bank.cr0 & 0x40001 != 0x40001) {
                    return 0;
                }
                if core.alignment_check.is_none() {`,
    ),
  ),
  native(
    'validated-control-import-keeps-stale-ac',
    'paging_import_clears_transient_alignment_only_after_validation_and_keeps_pending_source',
    edit(
      'src/embedding/exports/paging.rs',
      `        core.paging = Some(candidate);
        core.paging_work.clear();
        core.alignment_check = None;
        0`,
      `        core.paging = Some(candidate);
        core.paging_work.clear();
        0`,
    ),
  ),
  native(
    'saturated-epoch-skips-pending-owner-cancellation',
    'context_reuse_pending_aba_is_cancelled_even_when_epoch_saturates',
    edit(
      'src/embedding/exports/translation.rs',
      `        self.translation_owners
            .cancel_pending(|owner| self.image.release(owner));`,
      `        if self.context_version != u64::MAX {
            self.translation_owners.cancel_pending(|owner| self.image.release(owner));
        }`,
    ),
  ),
  native(
    'enter-probe-widens-to-operand-width',
    'enter_alignment_does_not_widen_its_final_byte_probe',
    edit(
      'src/x86/compound_stack.rs',
      '            memory.probe_write(address).map_err(Failure::Operand)?;',
      '            memory.check_write_aligned(address, u32::from(width.bytes()), Intent::Natural(u32::from(width.bytes()))).map_err(Failure::Operand)?;',
    ),
  ),
  publicCpu(
    'ac-overwrites-cr2',
    granular,
    edit(
      'src/embedding/exports/context.rs',
      `            AccessFailure::AlignmentCheck(fault) => {
                self.alignment_fault = None;
                self.alignment_check = Some(fault);
            }`,
      `            AccessFailure::AlignmentCheck(fault) => {
                if let Some(bank) = self.paging.as_mut() { bank.cr2 = fault.address; }
                self.alignment_fault = None;
                self.alignment_check = Some(fault);
            }`,
    ),
  ),
  publicCpu(
    'generated-operand-refusal-loses-ac-reason',
    granular,
    edit(
      'src/dbt/wasm/memory.rs',
      '        if let Some(reason) = self.access_reason {',
      '        if let Some(reason) = self.access_reason.filter(|_| false) {',
    ),
  ),
  publicCpu(
    'popa-reads-skipped-sp-slot',
    'POPA preserves all registers on AC and never accesses the saved-SP hole',
    edit(
      'src/x86/compound_stack.rs',
      `                if index == 3 {
                    sp = merge(sp, sp.wrapping_add(u64::from(width.bytes())), stack);`,
      `                if index == 3 {
                    let _ = read(context, memory, sp & stack.mask(), width, observe)?;
                    sp = merge(sp, sp.wrapping_add(u64::from(width.bytes())), stack);`,
    ),
  ),
  publicCpu(
    'rep-zero-count-still-touches-an-operand',
    'MOVS preserves source-before-destination order and skips zero or suspended work',
    edit(
      'src/dbt/wasm/strings.rs',
      '        self.op(Op::I64Eqz).op(Op::BrIf(0));',
      '        self.op(Op::I64Eqz).op(Op::Drop);',
    ),
  ),
  publicCpu(
    'movs-publishes-source-index-before-refused-read',
    'MOVS preserves source-before-destination order and skips zero or suspended work',
    edit(
      'src/dbt/wasm/strings.rs',
      `        if matches!(kind, Movs | Lods | Cmps) {
            self.string_address(6, address_width, source_segment);
            self.load_address_value(width, false, exit_depth);`,
      `        if matches!(kind, Movs | Lods | Cmps) {
            self.string_address(6, address_width, source_segment);
            self.string_index(6, width, address_width);
            self.load_address_value(width, false, exit_depth);`,
    ),
  ),
  publicCpu(
    'movs-preflights-destination-before-source',
    'MOVS preserves source-before-destination order and skips zero or suspended work',
    edit(
      'src/dbt/wasm/strings.rs',
      `        if matches!(kind, Movs | Lods | Cmps) {
            self.string_address(6, address_width, source_segment);
            self.load_address_value(width, false, exit_depth);`,
      `        if kind == Movs {
            self.string_address(7, address_width, Segment::Es);
            self.load_address_value(width, true, exit_depth);
        }
        if matches!(kind, Movs | Lods | Cmps) {
            self.string_address(6, address_width, source_segment);
            self.load_address_value(width, false, exit_depth);`,
    ),
    edit(
      'src/dbt/wasm/mod.rs',
      '    let write_intent = unit.instructions.iter().any(|instruction| {',
      `    let write_intent = unit.instructions.iter().any(|instruction| matches!(instruction.operation,
        Operation::String { kind: crate::x86::instruction::StringKind::Movs, .. }))
        || unit.instructions.iter().any(|instruction| {`,
    ),
  ),
  publicCpu(
    'ins-consumes-two-provider-values-for-one-element',
    'INS retains consumed input before destination AC',
    edit(
      'src/embedding/exports/io.rs',
      '            u64::from(port_read(port, width.bits()))',
      `            let _ = port_read(port, width.bits());
            u64::from(port_read(port, width.bits()))`,
    ),
  ),
  publicCpu(
    'outs-calls-provider-before-source-refusal',
    'OUTS source refusal prevents provider output',
    edit(
      'src/embedding/exports/io.rs',
      `            if kind == 3 {
                data = match guest_access!(core, paged, read(address, u32::from(width.bytes()))) {
                    Ok(value) => value,
                    Err(failure) => {
                        core.access_failure(failure);
                        return Err(core.memory_failure_reason());
                    }
                };
                if OBSERVED {
                    core.debug_operand(address, u32::from(width.bytes()), Access::Read);
                }
            }`,
      '            // Deliberately defer OUTS source admission until after the provider.',
    ),
    edit(
      'src/embedding/exports/io.rs',
      `        core.io_effect = false;
        if kind == 2 {`,
      `        core.io_effect = false;
        if kind == 3 {
            let paged = core.paging.is_some();
            if let Err(failure) = guest_access!(core, paged, read(address, u32::from(width.bytes()))) {
                core.access_failure(failure);
                return core.memory_failure_reason();
            }
            if OBSERVED { core.debug_operand(address, u32::from(width.bytes()), Access::Read); }
        }
        if kind == 2 {`,
    ),
  ),
  publicCpu(
    'popf-preclears-ac-before-stack-read',
    'POPF reads under old AC and sixteen-bit restores cannot clear the upper flag',
    edit(
      'src/dbt/wasm/system_flags.rs',
      `                self.stack_word(operation.width, exit_depth);
                self.flags_call(operation, Some(VALUE), exit_depth);`,
      `                let memory = MemArg { offset: offset_of!(RunState, flags) as u64, align: 3, memory_index: 0 };
                self.op(Op::LocalGet(0)).op(Op::LocalGet(0)).op(Op::I64Load(memory))
                    .op(Op::I64Const(!crate::x86::flags::AC as i64)).op(Op::I64And).op(Op::I64Store(memory));
                self.stack_word(operation.width, exit_depth);
                self.flags_call(operation, Some(VALUE), exit_depth);`,
    ),
  ),
  native(
    'am-rejected-by-normalized-import',
    'alignment_mask_is_admitted_without_translation_or_pdpte_effects',
    edit(
      controls,
      'self.cr0 & !(CR0_WP | CR0_AM | CR0_NUMERIC) != CR0_FIXED & !CR0_NUMERIC',
      'self.cr0 & !(CR0_WP | CR0_NUMERIC) != CR0_FIXED & !CR0_NUMERIC',
    ),
  ),
  native(
    'am-causes-translation-fence',
    'alignment_mask_is_admitted_without_translation_or_pdpte_effects',
    edit(controls, 'self.cr0 & !(CR0_NUMERIC | CR0_AM),', 'self.cr0 & !CR0_NUMERIC,'),
    edit(controls, 'other.cr0 & !(CR0_NUMERIC | CR0_AM),', 'other.cr0 & !CR0_NUMERIC,'),
  ),
  native(
    'am-gate-omitted',
    frameGate,
    edit(
      access,
      'alignment_controls: ac && paging.cr0 & 0x40001 == 0x40001,',
      'alignment_controls: ac,',
    ),
  ),
  native(
    'ac-gate-omitted',
    frameGate,
    edit(
      access,
      'alignment_controls: ac && paging.cr0 & 0x40001 == 0x40001,',
      'alignment_controls: paging.cr0 & 0x40001 == 0x40001,',
    ),
  ),
  native(
    'cpl-gate-omitted',
    frameGate,
    edit(
      access,
      '            && class.is_user()\n            && let Err(fault)',
      '            && let Err(fault)',
    ),
  ),
  native(
    'ac-before-complete-nominal-admission',
    'complete_nominal_page_admission_precedes_alignment_in_fragment_order',
    edit(
      access,
      gate,
      gate
        .replace('        let prepared = self.prepare(address, width, access, rmw, class)?;\n', '')
        .replace(
          '        Ok(prepared)',
          '        let prepared = self.prepare(address, width, access, rmw, class)?;\n        Ok(prepared)',
        ),
    ),
  ),
  native(
    'dirty-publication-before-ac',
    'split_page_alignment_retains_accessed_but_never_dirty_or_pending_payload',
    edit(
      access,
      gate,
      gate.replace(
        '        if self.alignment_controls',
        `        if access == Access::Write {
            let updates = self.dirty_admission(&prepared, &[(0, width)])?;
            self.dirty_publish(&updates);
        }
        if self.alignment_controls`,
      ),
    ),
  ),
  native(
    'ac-keeps-unpublished-pending-batch',
    'refused_alignment_cancels_unpublished_batch_without_undoing_prior_publication',
    edit(
      access,
      gate,
      gate.replace(
        '            self.storage.borrow_mut().work.clear();',
        '            let _ = &self.storage;',
      ),
    ),
  ),
  native(
    'frame-check-uses-initiating-cpl',
    frameGate,
    edit(
      access,
      `    fn check_frame_write_aligned(
        &self,
        address: u64,
        width: u32,
        cpl: u8,
        intent: Intent,
    ) -> Result<(), AccessFailure> {
        self.capture_write(
            address,
            width,
            AccessClass::operand(cpl, self.raw_ac),
            intent,
        )
    }`,
      `    fn check_frame_write_aligned(
        &self,
        address: u64,
        width: u32,
        _cpl: u8,
        intent: Intent,
    ) -> Result<(), AccessFailure> {
        self.capture_write(address, width, self.class, intent)
    }`,
    ),
  ),
  native(
    'captured-frame-authority-ignored-at-commit',
    'captured_frame_authority_does_not_recheck_initiating_privilege_at_commit',
    edit(
      access,
      '.position(|p| p.address == address && p.width == width);',
      '.position(|p| false && p.address == address && p.width == width);',
    ),
  ),
  native(
    'system-record-base-field-omitted',
    'component_alignment_follows_complete_record_admission_and_reports_its_field',
    edit(intent, '                check(address + 2, base, base)', '                Ok(())'),
  ),
  native(
    'metadata-inherits-ordinary-alignment',
    'odd_system_metadata_is_exempt_while_the_same_user_operand_faults',
    edit(
      access,
      `    fn read_system(&self, address: u64, width: u32) -> Result<[u8; 16], AccessFailure> {
        self.read_at(
            address,
            width,
            AccessClass::ImplicitSupervisor,
            Intent::Exempt,
        )
    }`,
      `    fn read_system(&self, address: u64, width: u32) -> Result<[u8; 16], AccessFailure> {
        self.read_at(address, width, self.class, Intent::Natural(2))
    }`,
    ),
  ),
  native(
    'pending-ac-rejects-ext-one',
    'alignment_exception_checkpoint_accepts_only_its_zero_or_ext_error_slot',
    edit(
      exceptions,
      '            17 => matches!(self.error_code, Some(0 | 1)),',
      '            17 => self.error_code == Some(0),',
    ),
  ),
  native(
    'ac-misclassified-as-general-protection',
    'each_operand_admission_clears_a_previous_alignment_refusal',
    edit(
      'src/embedding/exports/paging.rs',
      '            None if self.alignment_check.is_some() => super::super::layout::ALIGNMENT_CHECK,',
      '            None if self.alignment_check.is_some() => super::super::layout::GENERAL_PROTECTION,',
    ),
  ),
  native(
    'successful-scalar-helper-keeps-stale-ac',
    'each_operand_admission_clears_a_previous_alignment_refusal',
    edit(
      'src/embedding/exports.rs',
      `fn core_load_impl<const OBSERVED: bool, const PAGED: bool>(address: u64, width: u32) -> u64 {
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        core.fault = None;
        if PAGED {
            core.alignment_check = None;
        }`,
      `fn core_load_impl<const OBSERVED: bool, const PAGED: bool>(address: u64, width: u32) -> u64 {
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        core.fault = None;`,
    ),
  ),
  native(
    'secondary-ac-is-contributory',
    'secondary_alignment_is_benign_after_page_or_contributory_faults',
    edit(
      exceptions,
      `        let double_fault = vector != 17
            && (self.vector == 14 || vector != 14 && matches!(self.vector, 0 | 10..=13));`,
      '        let double_fault = self.vector == 14 || vector != 14 && matches!(self.vector, 0 | 10..=13);',
    ),
  ),
  native(
    'event-entry-ac-loses-ext-bit',
    'secondary_alignment_is_benign_after_page_or_contributory_faults',
    // Distinguish the two identically named methods with the following private fn.
    edit(
      exceptions,
      `    pub fn escalate_alignment_check(self) -> EventState {
        self.secondary(17, 1, None)
    }

    fn secondary(self, vector: u8, error_code: u16, page_fault_address: Option<u64>) -> EventState {
        if self.vector == 8`,
      `    pub fn escalate_alignment_check(self) -> EventState {
        self.secondary(17, 0, None)
    }

    fn secondary(self, vector: u8, error_code: u16, page_fault_address: Option<u64>) -> EventState {
        if self.vector == 8`,
    ),
  ),
  publicCpu(
    'scalar-operand-becomes-exempt',
    granular,
    edit(
      'src/embedding/exports.rs',
      'guest_access!($core, $paged, read_aligned($address, $width, crate::x86::alignment::Intent::Natural($width)))',
      'guest_access!($core, $paged, read_aligned($address, $width, crate::x86::alignment::Intent::Exempt))',
    ),
  ),
  publicCpu(
    'scalar-helper-ignores-live-ac',
    'warm middle entries observe live AM and AC without an AM-only source fence',
    edit(
      'src/embedding/exports.rs',
      '            let flags = unsafe { (*$core.state.get()).flags };',
      '            let flags = unsafe { (*$core.state.get()).flags } & !(1 << 18);',
    ),
  ),
  publicCpu(
    'rmw-operand-becomes-exempt',
    granular,
    edit(
      'src/embedding/exports.rs',
      'guest_access!($core, $paged, read_for_write_aligned($address, $width, crate::x86::alignment::Intent::Natural($width)))',
      'guest_access!($core, $paged, read_for_write_aligned($address, $width, crate::x86::alignment::Intent::Exempt))',
    ),
  ),
  publicCpu(
    'ext80-and-bcd-use-four-byte-granule',
    granular,
    edit(
      'src/x86/x87/mod.rs',
      '            Format::F80 | Format::Bcd | Format::F64 | Format::I64 => Intent::Natural(8),',
      `            Format::F80 | Format::Bcd => Intent::Natural(4),
            Format::F64 | Format::I64 => Intent::Natural(8),`,
    ),
  ),
  publicCpu(
    'numeric-environment32-uses-two-byte-granule',
    granular,
    edit(
      'src/x86/numeric_images/mod.rs',
      '            Format::Protected32 | Format::Real32 => Intent::Natural(4),',
      '            Format::Protected32 | Format::Real32 => Intent::Natural(2),',
    ),
  ),
  publicCpu(
    'permitted-unaligned128-checks-eight-byte-granule',
    'admitted unaligned128 loads differ from mandatory16 GP',
    edit(intent, '            16 => Self::Exempt,', '            16 => Self::Natural(8),'),
  ),
  publicCpu(
    'mandatory-vector-gp-admission-skipped',
    'admitted unaligned128 loads differ from mandatory16 GP',
    edit(
      'src/embedding/exports/vector.rs',
      '        if address & u64::from(alignment - 1) != 0 {',
      '        if false && address & u64::from(alignment - 1) != 0 {',
    ),
  ),
  publicCpu(
    'mmx-qwords-inherit-mandatory16',
    granular,
    edit(
      'src/embedding/exports/mmx.rs',
      '            let alignment = if width == 16 { 16 } else { 1 };',
      '            let alignment = if width >= 8 { 16 } else { 1 };',
    ),
    edit(
      'src/embedding/exports/vector.rs',
      '        assert!(alignment == 1 || alignment == 16 && width == 16);',
      '        assert!(alignment == 1 || alignment == 16 && matches!(width, 8 | 16));',
    ),
  ),
  publicCpu(
    'zero-mask-movq-skips-nominal-alignment',
    granular,
    edit(
      'src/embedding/exports/streaming.rs',
      '                check_write_aligned(address, width, crate::x86::alignment::Intent::vector(width))',
      '                check_write_aligned(address, width, crate::x86::alignment::Intent::Exempt)',
    ),
  ),
  publicCpu(
    'ins-preflights-destination-before-provider',
    'INS retains consumed input before destination AC',
    edit(
      'src/embedding/exports/io.rs',
      '            if kind == 3 {\n                data = match guest_access!',
      `            if kind == 2 {
                if let Err(failure) = guest_access!(core, paged,
                    check_write_aligned(address, u32::from(width.bytes()), crate::x86::alignment::Intent::Natural(u32::from(width.bytes())))) {
                    core.access_failure(failure);
                    return Err(core.memory_failure_reason());
                }
            }
            if kind == 3 {
                data = match guest_access!`,
    ),
  ),
  publicCpu(
    'outs-source-becomes-exempt',
    'OUTS source refusal prevents provider output',
    edit(
      'src/embedding/exports/io.rs',
      'data = match guest_access!(core, paged, read(address, u32::from(width.bytes()))) {',
      'data = match guest_access!(core, paged, read_aligned(address, u32::from(width.bytes()), crate::x86::alignment::Intent::Exempt)) {',
    ),
  ),
  publicCpu(
    'sgdt-explicit-operand-inherits-metadata-exemption',
    granular,
    edit(
      'src/x86/protection/mod.rs',
      '                Intent::SystemRecord((length - 2) as u32),',
      '                Intent::Exempt,',
    ),
  ),
  publicCpu(
    'public-ac-packet-omitted',
    'semantic operand granules suppress all candidate numeric and RMW state',
    edit(
      'host/cpu.ts',
      "reason === 'alignment-check' ? this.#engine.alignmentCheck(reason) : undefined;",
      'undefined;',
    ),
  ),
  publicCpu(
    'public-ac-packet-address-confused-with-offset',
    'final segmented linear address reverses effective-offset residues',
    // Match the whole method tail to avoid mutating the older alignment-fault API.
    edit(
      'host/core.ts',
      `      address: BigInt(raw.address),
    };
  }

  simdException(reason: string)`,
      `      address: BigInt(raw.offset),
    };
  }

  simdException(reason: string)`,
    ),
  ),
];

export const exclusions = [
  {
    name: 'first-translated-physical-address',
    reason:
      'For current natural granules 1/2/4/8, paging and A20 preserve the relevant low bits; this is equivalent to the selected linear-address check.',
  },
  {
    name: 'pe-check-omitted-in-valid-public-state',
    reason:
      'PE0 and CPL3 cannot coexist in an admitted public context; the native capability-isolation row is not an architectural counterexample.',
  },
  {
    name: 'later-same-width-rep-or-popa-residue',
    reason:
      'A fixed context and width-preserving stride cannot invent a later natural-alignment failure.',
  },
  {
    name: 'popa-ac-only-prefix-publication',
    reason:
      'In legal static POPA geometry an alignment refusal is on the first accessed slot, so broadening only the SS-limit prefix rule to AC has an empty prefix. A later private/page refusal and an actual skipped-slot access are separate killable boundaries.',
  },
];
