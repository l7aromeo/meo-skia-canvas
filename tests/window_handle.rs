//! A window changed after it has opened, from another thread, end to end,
//! and a closed window's id given to another.
//!
//! The registry and its races are pinned without a display in
//! `src/gui/handle.rs`, and the id bookkeeping in `src/gui/session.rs`. This
//! is the part those cannot reach: a real window, a real loop, a change from
//! a worker thread arriving at the window system while the loop waits for
//! its next frame, and a window closing under a loop that keeps running.

use std::{
    cell::{Cell, RefCell},
    num::NonZeroU32,
    process::ExitCode,
    rc::Rc,
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
            match a_closed_windows_id_is_free() {
                Ok(said) => {
                    println!("window_handle: {said}");
                    ExitCode::SUCCESS
                }
                Err(why) => {
                    eprintln!("window_handle: {why}");
                    ExitCode::FAILURE
                }
            }
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

/// The id a closed window gave up, in a range nothing else here uses.
const REUSED: u32 = 7_000_001;

/// Closes one of two open windows from a worker, takes its id for a new
/// window from the other's draw handler while the loop runs on, and opens
/// that window in a second run.
fn a_closed_windows_id_is_free() -> Result<String, String> {
    App::set_fps(30.0);
    let id = NonZeroU32::new(REUSED).expect("not zero");

    // Closed by a worker once shown, counting the frames it draws.
    let mut closing = Window::new(40.0, 30.0);
    closing.set_id(id).map_err(|e| e.to_string())?;
    let closing_handle = closing.handle();
    let drawn_by_closing = Rc::new(Cell::new(0u64));
    let count = Rc::clone(&drawn_by_closing);
    closing.on_draw(move |_, _| count.set(count.get() + 1));

    // Watches for the close, then takes the id for a window of its own.
    let mut watcher = Window::new(40.0, 30.0);
    let watcher_handle = watcher.handle();
    let taken: Rc<RefCell<Option<Result<Window, String>>>> =
        Rc::new(RefCell::new(None));
    let after_close = Rc::new(Cell::new(None::<u64>));
    let frames_since = Rc::new(Cell::new(0u32));
    {
        let (taken, after_close, frames_since, drawn) = (
            Rc::clone(&taken),
            Rc::clone(&after_close),
            Rc::clone(&frames_since),
            Rc::clone(&drawn_by_closing),
        );
        watcher.on_draw(move |_, _| {
            if closing_handle.is_open() {
                return;
            }
            if taken.borrow().is_none() {
                after_close.set(Some(drawn.get()));
                let mut reuse = Window::new(50.0, 20.0);
                let result = reuse.set_id(id).map(|()| reuse);
                *taken.borrow_mut() = Some(result.map_err(|e| e.to_string()));
            }
            // A few more frames, to see that the closed one draws no more.
            frames_since.set(frames_since.get() + 1);
            if frames_since.get() > 10 {
                watcher_handle.close();
            }
        });
    }
    closing.open();
    watcher.open();

    let closer = closing_handle;
    std::thread::spawn(move || {
        let shown = Instant::now();
        while closer.spec().is_none_or(|spec| spec.left.is_none()) {
            if shown.elapsed() > PATIENCE {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        closer.close();
    });

    App::run().map_err(|e| format!("the second run did not run: {e}"))?;

    let reuse = taken
        .borrow_mut()
        .take()
        .ok_or("the loop ended before the closed window's id was tried")?
        .map_err(|e| format!("a closed window's id was refused: {e}"))?;
    let at_close = after_close.get().unwrap_or_default();
    if drawn_by_closing.get() != at_close {
        return Err(format!(
            "the closed window drew {} more frames after it closed",
            drawn_by_closing.get() - at_close
        ));
    }

    // The window holding the reused id, in a run of its own: it is the one
    // drawn and the one its handle reports.
    let mut reuse = reuse;
    reuse.set_title("holds the reused id");
    let drawn_by_reuse = Rc::new(Cell::new(0u64));
    let title_seen: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let reuse_handle = reuse.handle();
    {
        let (count, title_seen) =
            (Rc::clone(&drawn_by_reuse), Rc::clone(&title_seen));
        reuse.on_draw(move |_, frame| {
            count.set(count.get() + 1);
            *title_seen.borrow_mut() =
                reuse_handle.spec().map(|spec| spec.title);
            if frame > 5 {
                reuse_handle.close();
            }
        });
    }
    reuse.open();
    App::run().map_err(|e| format!("the third run did not run: {e}"))?;
    if drawn_by_reuse.get() == 0 {
        return Err(format!("the window given id {REUSED} never drew"));
    }
    let title = title_seen.borrow().clone();
    if title.as_deref() != Some("holds the reused id") {
        return Err(format!("its handle reported the title {title:?}"));
    }
    Ok(format!(
        "a closed window's id went to a new window while the loop ran, the \
         closed one drew nothing after closing ({at_close} frames before), \
         and the new one drew {} frames under id {REUSED}",
        drawn_by_reuse.get()
    ))
}
