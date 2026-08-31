//! Hardware integration tests. Run explicitly:
//!   cargo test --test hardware -- --ignored --nocapture --test-threads=1

use visor::actions::{ddc::DdcMonitor, monitors};

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
