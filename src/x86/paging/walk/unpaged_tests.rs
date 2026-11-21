use super::*;

#[test]
fn compact_unpaged_identity_has_literal_domain_and_fault_provenance_for_every_class() {
    for class in [
        AccessClass::User,
        AccessClass::Supervisor { ac: false },
        AccessClass::Supervisor { ac: true },
        AccessClass::ImplicitSupervisor,
    ] {
        for access in [Access::Read, Access::Write, Access::Execute] {
            for (linear, admitted) in [
                (0, Some(0)),
                (0xffff_ffff, Some(0xffff_ffff)),
                (0x1_0000_0000, None),
                (u64::MAX, None),
            ] {
                let request = Request {
                    linear,
                    access,
                    class,
                };
                let expected = match admitted {
                    Some(physical) => Ok(physical),
                    None => Err(Failure::Physical(PhysicalFault {
                        linear,
                        access,
                        physical: Fault {
                            address: linear,
                            width: 1,
                            access,
                            cause: crate::memory::Cause::AddressRange,
                            failing: 0x1_0000_0000,
                        },
                        phase: PhysicalPhase::FinalAccess,
                    })),
                };
                assert_eq!(
                    request.unpaged_physical(),
                    expected,
                    "{access:?}/{class:?}/{linear:x}"
                );
            }
        }
    }
}
