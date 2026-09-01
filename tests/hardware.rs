//! Hardware integration tests. Run explicitly:
//!   cargo test --test hardware -- --ignored --nocapture --test-threads=1

use visor::actions::{Resolver, ddc::DdcMonitor, monitors};
use visor::config::DisplayConfig;
use visor::core::types::DisplayLevel;
use visor::core::types::FaceResult;
use visor::sense::camera::{Camera, WinRtCamera};

#[test]
#[ignore = "requires a webcam and a person in front of it"]
fn detects_a_face_and_reports_a_plausible_ratio() {
    let mut cam = WinRtCamera::new("");
    cam.open();
    std::thread::sleep(std::time::Duration::from_millis(500));

    let mut saw_face = false;
    for _ in 0..10 {
        match cam.probe() {
            FaceResult::Face {
                count,
                largest_ratio,
            } => {
                println!("count={count} ratio={largest_ratio:.3}");
                assert!(largest_ratio > 0.0 && largest_ratio <= 1.0);
                saw_face = true;
            }
            FaceResult::NoFace => println!("no face"),
            FaceResult::Unknown => println!("unknown (error)"),
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    cam.close();
    assert!(saw_face, "sit in front of the camera while running this");
}

#[test]
#[ignore = "requires a DDC/CI-capable monitor"]
fn brightness_round_trips_and_reports_honestly() {
    let ms = monitors::enumerate();
    let m = ms.first().expect("no monitors");
    let mut d = DdcMonitor::open(m.handle).expect("monitor does not speak DDC/CI");

    let saved = d.saved_brightness().expect("could not read brightness");
    println!("saved brightness: {saved}");

    let took = d.set_brightness(30);
    println!("set_brightness(30) confirmed by readback: {took}");
    // With Windows HDR ON this is expected to be false — that is the whole
    // point of the readback, and it is not a test failure.

    // NOTE: `saved` is an absolute VCP value, not a percentage, so
    // `set_brightness(saved as u8)` (as originally drafted in the plan) is
    // wrong — it would ask for `saved`% of `saved`, not "restore to
    // `saved`". Restoring must go through `restore_brightness()`.
    d.restore_brightness();
}

#[test]
#[ignore = "physically powers the monitor off and on"]
fn power_off_and_on_round_trips() {
    let ms = monitors::enumerate();
    let m = ms.first().expect("no monitors");
    let mut d = DdcMonitor::open(m.handle).expect("monitor does not speak DDC/CI");

    assert!(d.set_power(false), "power off rejected");
    std::thread::sleep(std::time::Duration::from_secs(5));
    assert!(d.set_power(true), "power on rejected");
    println!(
        "if the panel did not come back, DDC wake is unreliable here — \
              see spec §12 and set deep_after very high"
    );
}

#[test]
#[ignore = "requires a DDC/CI-capable monitor; visibly dims the panel"]
fn a_rescan_while_dimmed_does_not_capture_the_dim_as_the_restore_point() {
    // The compounding failure this guards against: `DdcMonitor::open` takes
    // whatever brightness the panel currently reports as the restore point, so
    // a `WM_DISPLAYCHANGE` arriving while VISOR is dimming would re-open at
    // the dim value and never be able to get back. Repeat it and the panel
    // walks down to black.
    let baseline = {
        let m = monitors::enumerate();
        let m = m.first().expect("no monitors");
        let d = DdcMonitor::open(m.handle).expect("monitor does not speak DDC/CI");
        d.saved_brightness().expect("could not read brightness")
    };
    println!("baseline brightness: {baseline}");

    let mut r = Resolver::new(&DisplayConfig::default());
    r.apply(DisplayLevel::Dim(20));
    std::thread::sleep(std::time::Duration::from_millis(500));
    r.rescan();
    r.apply(DisplayLevel::Full);
    std::thread::sleep(std::time::Duration::from_millis(500));

    let after = {
        let m = monitors::enumerate();
        let m = m.first().expect("no monitors");
        let d = DdcMonitor::open(m.handle).expect("monitor does not speak DDC/CI");
        d.saved_brightness().expect("could not read brightness")
    };
    println!("brightness after dim -> rescan -> restore: {after}");
    assert!(
        after.abs_diff(baseline) <= 5,
        "rescan captured the dim as the restore point: {baseline} -> {after}"
    );
}
