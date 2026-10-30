//! End to end against a real app: needs Flutter, a booted iOS simulator and a small app whose
//! `lib/main.dart` has `const Text('Hello from the demo')` centred on screen. Run with
//!
//!   INSPECT_DEMO_APP=/path/to/app INSPECT_DEVICE=<simulator udid> \
//!     cargo test -p core-flutter --test inspector_live -- --ignored --nocapture

use std::path::PathBuf;
use std::time::{Duration, Instant};

use core_flutter::{
    DaemonOutput, FlutterDaemon, TouchPhase, parse_selected_widget, simulator_prewarm,
    simulator_touch,
};

fn wait_for<T>(
    daemon: &FlutterDaemon,
    seconds: u64,
    mut pick: impl FnMut(DaemonOutput) -> Option<T>,
) -> Option<T> {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < deadline {
        match daemon.events.try_recv() {
            Ok(output) => {
                if let Some(found) = pick(output) {
                    return Some(found);
                }
            }
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    None
}

#[test]
#[ignore = "needs Flutter, a booted simulator and the demo app"]
fn tapping_a_widget_reports_its_source_line() {
    let app = PathBuf::from(std::env::var("INSPECT_DEMO_APP").expect("INSPECT_DEMO_APP"));
    let device = std::env::var("INSPECT_DEVICE").expect("INSPECT_DEVICE");
    let daemon = FlutterDaemon::spawn(&app, &device).expect("flutter run --machine");

    let app_id = wait_for(&daemon, 300, |o| match o {
        DaemonOutput::Event { event, params } if event == "app.started" => params
            .get("appId")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        _ => None,
    })
    .expect("the demo app should start");
    std::thread::sleep(Duration::from_secs(3));

    daemon
        .inspector_enable(&app_id, &app)
        .expect("enable inspect mode");
    // The touch helper drops events sent right after it starts; let it warm up first.
    simulator_prewarm(Some(&device)).expect("start the touch helper");
    std::thread::sleep(Duration::from_millis(1500));

    simulator_touch(Some(&device), TouchPhase::Down, 0.498, 0.526).unwrap();
    std::thread::sleep(Duration::from_millis(80));
    simulator_touch(Some(&device), TouchPhase::Up, 0.498, 0.526).unwrap();
    std::thread::sleep(Duration::from_millis(500));

    let id = daemon
        .inspector_request_selected(&app_id)
        .expect("ask for the selection");
    let reply = wait_for(&daemon, 20, |o| match o {
        DaemonOutput::Response { id: got, result } if got == id => Some(result),
        _ => None,
    })
    .expect("a reply to getSelectedSummaryWidget");

    let widget = parse_selected_widget(&reply.expect("the extension call succeeds"))
        .expect("a selected widget");
    println!("selected: {widget:?}");
    assert_eq!(widget.name, "Text");
    assert!(widget.local, "created by the demo project");
    assert!(widget.path.ends_with("lib/main.dart"), "{:?}", widget.path);
    let source = std::fs::read_to_string(&widget.path).unwrap();
    let line = source.lines().nth(widget.line as usize - 1).unwrap();
    assert!(
        line.contains("Hello from the demo"),
        "line {} is {line:?}",
        widget.line
    );

    daemon.inspector_disable(&app_id).unwrap();
    let _ = daemon.stop(&app_id);
}
