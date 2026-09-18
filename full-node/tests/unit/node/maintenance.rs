use super::*;

#[test]
fn maintenance_is_serial_and_retries_without_a_new_sync_edge() {
    let controller = Arc::new(IndexMaintenance::default());
    let now = Instant::now();
    let build = controller.schedule_at(true, false, now).unwrap();
    assert_eq!(build.action, IndexAction::Build);
    assert!(controller.schedule_at(false, true, now).is_none());
    assert!(controller.schedule_at(true, false, now).is_none());
    build.finish(false);
    assert!(controller.schedule_at(true, false, now).is_none());
    let retry = controller
        .schedule_at(true, false, now + Duration::from_secs(31))
        .unwrap();
    retry.finish(true);
    assert!(
        controller
            .schedule_at(true, false, now + Duration::from_secs(32))
            .is_none()
    );
    let shed = controller
        .schedule_at(false, true, now + Duration::from_secs(32))
        .unwrap();
    assert_eq!(shed.action, IndexAction::Shed);
    assert!(
        controller
            .schedule_at(true, false, now + Duration::from_secs(32))
            .is_none()
    );
    shed.finish(true);
    assert_eq!(
        controller
            .schedule_at(true, false, now + Duration::from_secs(33))
            .unwrap()
            .action,
        IndexAction::Build
    );
}

#[test]
fn cancelling_maintenance_does_not_pin_the_controller() {
    let controller = Arc::new(IndexMaintenance::default());
    let now = Instant::now();
    drop(controller.schedule_at(true, false, now).unwrap());
    assert!(controller.schedule_at(true, false, now).is_none());
    assert!(
        controller
            .schedule_at(true, false, now + Duration::from_secs(31))
            .is_some()
    );
}
