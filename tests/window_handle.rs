//! A window changed after it has opened, from another thread, end to end.
//!
//! The registry and its races are pinned without a display in
//! `src/gui/handle.rs`. This is the part those cannot reach: a real window,
//! a real loop, and a change from a worker thread arriving at the window
//! system while the loop waits for its next frame.

use std::{
    process::ExitCode,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use meo_skia_canvas::prelude::*;

/// How long a change may take to arrive before the test calls it lost. The
/// loop is woken for it and measured in microseconds, so anything near this
/// is a change that never arrived rather than a slow one.
const PATIENCE: Duration = Duration::from_secs(10);

fn main() -> ExitCode {
    let required =
        std::env::var_os("MEO_SKIA_CANVAS_REQUIRE_WINDOWING").is_some();

    // Slow enough that a change waiting for the next frame would be seen to
    // wait: a second between frames.
    App::set_fps(1.0);

    let window = Window::new(64.0, 48.0);
    let handle = window.handle();
    window.open();

    let outcome: Arc<Mutex<Option<Result<Duration, String>>>> =
        Arc::new(Mutex::new(None));
    let report = Arc::clone(&outcome);

    std::thread::spawn(move || {
        // Wait for the loop to show the window before changing it.
        let shown = Instant::now();
        while handle.spec().is_none_or(|spec| spec.left.is_none()) {
            if shown.elapsed() > PATIENCE {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        let asked = Instant::now();
        handle.set_title("changed from a worker");
        handle.set_size(120.0, 90.0);

        let result = loop {
            let spec = handle.spec();
            match spec {
                Some(spec)
                    if spec.title == "changed from a worker"
                        && (spec.width, spec.height) == (120.0, 90.0) =>
                {
                    break Ok(asked.elapsed());
                }
                _ if asked.elapsed() > PATIENCE => {
                    break Err(format!(
                        "not applied within {PATIENCE:?}: {spec:?}"
                    ));
                }
                _ => std::thread::sleep(Duration::from_millis(1)),
            }
        };
        *report.lock().expect("the report is not poisoned") = Some(result);
        handle.close();
    });

    match App::run() {
        Ok(()) => {}
        Err(why) if !required => {
            println!("window_handle: skipped, no window can open here ({why})");
            return ExitCode::SUCCESS;
        }
        Err(why) => {
            eprintln!("window_handle: the loop did not run: {why}");
            return ExitCode::FAILURE;
        }
    }

    match outcome.lock().expect("the report is not poisoned").take() {
        Some(Ok(took)) => {
            println!(
                "window_handle: a worker's title and size arrived in {took:?}"
            );
            ExitCode::SUCCESS
        }
        Some(Err(why)) => {
            eprintln!("window_handle: {why}");
            ExitCode::FAILURE
        }
        None => {
            eprintln!(
                "window_handle: the loop ended before the worker reported"
            );
            ExitCode::FAILURE
        }
    }
}
