//! Hardware integration tests. Run explicitly:
//!   cargo test --test hardware -- --ignored --nocapture --test-threads=1

use visor::actions::{Resolver, ddc::DdcMonitor, monitors};
use visor::config::DisplayConfig;
use visor::core::types::DisplayLevel;
use visor::core::types::FaceResult;
use visor::sense::camera::{Camera, WinRtCamera};

/// Opens the first monitor over DDC/CI, or prints why not and returns `None`.
///
/// These are probes of what the hardware can do, not assertions that it must:
/// a panel with no DDC/CI is a supported configuration (VISOR falls back to the
/// overlay for dimming and to `SC_MONITORPOWER` for power), so "this monitor
/// does not speak DDC/CI" is a result to report, not a test failure.
fn first_ddc_monitor() -> Option<DdcMonitor> {
    let ms = monitors::enumerate();
    let Some(m) = ms.first() else {
        println!("SKIP: no monitors enumerated");
        return None;
    };
    match DdcMonitor::open(m.handle) {
        Some(d) => {
            println!("opened DDC/CI on {}", m.description);
            Some(d)
        }
        None => {
            println!("SKIP: {} does not speak DDC/CI.", m.description);
            println!("  VISOR dims with the overlay and powers off with SC_MONITORPOWER instead.");
            println!("  If you expected DDC: check for a DDC/CI toggle in the monitor's own");
            println!("  on-screen menu. Most KVMs and some USB-C docks do not pass DDC through.");
            None
        }
    }
}

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
    let Some(mut d) = first_ddc_monitor() else {
        return;
    };

    let Some(saved) = d.saved_brightness() else {
        println!("SKIP: monitor speaks DDC/CI but will not report brightness");
        return;
    };
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
    let Some(mut d) = first_ddc_monitor() else {
        return;
    };

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
    let Some(baseline) = first_ddc_monitor().and_then(|d| d.saved_brightness()) else {
        println!("SKIP: needs a monitor that reports brightness over DDC/CI");
        return;
    };
    println!("baseline brightness: {baseline}");

    let mut r = Resolver::new(&DisplayConfig::default());
    r.apply(DisplayLevel::Dim(20));
    std::thread::sleep(std::time::Duration::from_millis(500));
    r.rescan();
    r.apply(DisplayLevel::Full);
    std::thread::sleep(std::time::Duration::from_millis(500));

    let after = first_ddc_monitor()
        .and_then(|d| d.saved_brightness())
        .expect("brightness was readable a moment ago");
    println!("brightness after dim -> rescan -> restore: {after}");
    assert!(
        after.abs_diff(baseline) <= 5,
        "rescan captured the dim as the restore point: {baseline} -> {after}"
    );
}

#[test]
#[ignore = "requires a webcam; opens the camera and copies frames"]
fn the_preview_path_produces_a_real_picture() {
    // A length check alone would pass on a buffer of zeros, which is exactly
    // what a broken copy produces -- and a black preview looks like a covered
    // lens, not like a bug. So this asserts the frame has actual variation in
    // it, and that the rows are not sheared (a sheared frame still has plenty
    // of variation, so the row-start check earns its place too).
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_test_writer()
        .try_init();

    let mut cam = WinRtCamera::new("");
    cam.set_preview(true);
    cam.open();
    std::thread::sleep(std::time::Duration::from_millis(600));

    let mut got = None;
    for _ in 0..12 {
        let verdict = cam.probe();
        if let Some(f) = cam.take_preview() {
            got = Some((f, verdict));
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
    cam.close();

    let Some((frame, verdict)) = got else {
        panic!("no preview frame after 12 probes");
    };
    println!(
        "preview {}x{}, {} bytes, {} face(s), probe said {:?}",
        frame.width,
        frame.height,
        frame.luma.len(),
        frame.faces.len(),
        verdict
    );

    assert!(frame.width > 0 && frame.height > 0, "empty frame");
    assert_eq!(
        frame.luma.len(),
        (frame.width * frame.height) as usize,
        "luma must be tightly packed: width * height, no row padding"
    );

    let min = *frame.luma.iter().min().unwrap();
    let max = *frame.luma.iter().max().unwrap();
    println!("luminance range {min}..{max}");
    assert!(
        max - min > 20,
        "a real frame has tonal range; {min}..{max} is a blank buffer"
    );

    if let Some(r) = frame.largest_ratio() {
        println!("largest face ratio {r:.3}");
        assert!(r > 0.0 && r <= 1.0, "implausible ratio {r}");
    }
}
