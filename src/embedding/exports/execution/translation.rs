use super::*;

impl Core {
    pub(super) fn owner_alive(&self, owner: u64) -> bool {
        self.translation_owners.contains(owner) && self.image.owner_valid(owner)
    }

    pub(super) fn publish_owner(&mut self, owner: u64) -> bool {
        self.reconcile_translation_changes();
        if !self.image.owner_valid(owner) {
            return false;
        }
        let observed = self.boundary_observed();
        self.translation_owners.publish(
            owner,
            &self.context,
            observed,
            self.context_version,
            self.paging.as_ref(),
        )
    }

    pub(super) fn activate_owner(&mut self, owner: u64) -> bool {
        let paged_model = self.paging.is_some();
        if paged_model {
            self.reconcile_translation_changes();
        }
        let observed = self.boundary_observed();
        let admitted = self
            .translation_owners
            .admit(owner, &self.context, observed, |fetch| {
                if let Some(binding) = fetch
                    && !self.paging.as_ref().is_some_and(|state| {
                        binding.matches(
                            &self.image,
                            state,
                            &self.translation_cache,
                            self.context.cpl(),
                        )
                    })
                {
                    return false;
                }
                self.image.activate(owner)
            });
        if admitted && paged_model {
            self.active_owner = Some(owner);
        }
        admitted
    }

    pub(super) fn context_changed(&mut self) {
        self.cancel_pending(false);
    }

    fn cancel_pending(&mut self, fetch_changed: bool) {
        // Pending owners are cancelled even when the externally reported epoch saturates.
        self.translation_owners
            .cancel_pending(fetch_changed, |owner| self.image.release(owner));
        self.context_version = self.context_version.saturating_add(1);
    }

    pub(super) fn retire_translations(&mut self) {
        self.image.retire_sources();
        self.translation_owners.clear();
        self.context_version = self.context_version.saturating_add(1);
        self.active_owner = None;
    }

    pub(super) fn reconcile_translation_changes(&mut self) {
        if self.translation_cache.take_changed() {
            self.cancel_pending(self.paging.is_some_and(|state| state.cr0 & (1 << 31) != 0));
        }
    }

    pub(super) fn guest_root_changed(&mut self) {
        self.image.fence_active();
        self.active_owner = None;
        self.translation_cache.take_changed();
        self.cancel_pending(true);
    }

    pub(super) fn invalidate_runtime_sources(&mut self) {
        self.image.invalidate_sources();
        self.translation_owners.clear();
        self.active_owner = None;
        self.context_version = self.context_version.saturating_add(1);
    }

    pub(super) fn invalidate_pcid(&mut self, kind: u32, pcid: u16, linear: u64) {
        match kind {
            0 => self.translation_cache.invalidate_page(pcid, linear, false),
            1 => self.translation_cache.invalidate_pcid(pcid),
            2 => self.translation_cache.clear(),
            3 => self.translation_cache.invalidate_non_global(),
            _ => unreachable!("admitted INVPCID type"),
        }
        self.invalidate_runtime_sources();
    }

    pub(super) fn revoke_host_translations(&mut self) {
        self.translation_cache.clear();
        self.paging_work.clear();
        self.retire_translations();
        self.translation_cache.take_changed();
    }

    pub(super) fn memory_admin_changed(&mut self) {
        self.paging_work.clear();
        if self.paging.is_some() {
            self.translation_cache.clear();
            self.reconcile_translation_changes();
        }
    }
}
