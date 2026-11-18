use super::*;
use crate::x86::{context::ExecutionContext, exception::ExceptionEvent};

#[test]
fn execute_and_general_detect_are_distinct_saved_images_and_secondary_drops_debug() {
    let c = ExecutionContext::default();
    let mut e = ExceptionEvent::fault(1, 0x1000, 2, &c, None);
    e.saved_flags = 2;
    e.debug = Some(Source::Execute(1));
    assert!(e.validate(0x1000, 2, &c).is_ok());
    e.saved_flags = 0x10002;
    assert!(e.validate(0x1000, 2, &c).is_err());
    e.debug = Some(Source::GeneralDetect);
    assert!(e.validate(0x1000, 2, &c).is_ok());
    let crate::x86::exception::EventState::Pending(next) = e.escalate(13, 0) else {
        panic!("secondary")
    };
    assert_eq!(
        (next.vector, next.saved_flags, next.debug),
        (13, 0x10002, None)
    );
}

#[test]
fn task_switch_clears_local_enables_and_selects_bt_at_the_incoming_pc() {
    let mut dr = State {
        dr7: 0x20ff_07ff,
        ..State::default()
    };
    dr.switch_task();
    assert_eq!(dr.dr7, 0x20ff_07aa);
    let before = dr.dr6;
    dr.select_source(Some(Source::TaskSwitch(0)));
    assert_eq!(dr.dr6, (before & !15) | (1 << 15));

    dr.select_source(Some(Source::TaskSwitch(5)));
    assert_eq!(dr.dr6, (before & !15) | (1 << 15) | 5);
    let mut context = ExecutionContext::default();
    context.set_mode_and_cpl(crate::x86::context::Mode::Protected, 0);
    for flags in [2, 0x10102] {
        let trap = Selection::new(0x2345, 0x2345, false, flags, Some(Source::TaskSwitch(0)));
        assert_eq!(trap.resume_flags, flags);
        let mut event = ExceptionEvent::fault(1, 0x2345, flags, &context, None);
        event.debug = trap.source;
        event.saved_flags = trap.resume_flags;
        assert!(event.validate(0x2345, flags, &context).is_ok());
        assert!(
            event
                .validate(0x2345, flags, &ExecutionContext::default())
                .is_err()
        );
        event.instruction_pc -= 1;
        assert!(event.validate(0x2345, flags, &context).is_err());
    }
}
