use dg_xch_core::compute::{self, Phase};

#[test]
fn one_worker_runs_every_phase_without_an_extra_archive_worker() {
    compute::configure(1).unwrap();
    assert_eq!(compute::worker_count(), 1);
    for phase in Phase::ALL {
        let names = compute::map(phase, &[0], |_| {
            std::thread::current().name().unwrap().to_owned()
        });
        assert_eq!(names, ["dg-compute-0"]);
    }
}
