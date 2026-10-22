//! Changing a window after it has opened.
//!
//! [`Window::open`](super::session::Window::open) hands the window to the
//! event loop, which owns it from then on, so the [`Window`] value is gone by
//! the time a caller wants to retitle or resize it. A [`WindowHandle`] is
//! what is kept instead: an id, cheap to copy into a handler or send to
//! another thread, whose setters queue a change for the loop to apply.
//!
//! ```no_run
//! use meo_skia_canvas::prelude::*;
//!
//! let mut win = Window::new(400.0, 300.0);
//! let handle = win.handle();
//!
//! win.on_event(move |event| {
//!     if let UiEvent::Keyboard { event, code, .. } = event
//!         && event == "keydown"
//!         && *code == Key::KeyF
//!     {
//!         handle.set_fullscreen(true);
//!     }
//! });
//!
//! // From another thread too: the loop is woken to apply it.
//! std::thread::spawn(move || handle.set_title("loaded"));
//!
//! win.open();
//! App::run().expect("the event loop runs");
//! ```
//!
//! # When a change lands
//!
//! Each change is applied through the [`Window`] setter of the same name, so
//! it is validated and parsed exactly as it would have been before opening.
//! A change made in an event handler is applied before that pass draws; one
//! made in the draw handler is applied before the frame is shown; one made on
//! another thread wakes the loop and is applied at once, without running the
//! draw handler, so it does not advance the frame count.
//!
//! Changes are applied in the order they were queued, across every thread:
//! when two threads set the same property, the one queued last wins.
//!
//! A change for a window that is not open -- never opened, or closed since --
//! is dropped, as setting a property on a closed JavaScript `Window` changes
//! nothing visible. [`WindowHandle::is_open`] tells the two apart.

use std::{
    cell::RefCell,
    collections::HashMap,
    sync::{LazyLock, Mutex, MutexGuard, PoisonError},
    thread::{self, ThreadId},
};

use super::{
    app::App,
    session::Window,
    window::{Cursor, Fit, WindowSpec},
};
use crate::canvas::Canvas;

/// One queued change to a window, named for the [`Window`] setter that
/// applies it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Edit {
    Title(String),
    Size(f32, f32),
    Position(f32, f32),
    Fullscreen(bool),
    Cursor(Option<Cursor>),
    CursorCss(String),
    Visible(bool),
    Resizable(bool),
    Borderless(bool),
    Background(String),
    Fit(Fit),
    Page(u32),
    /// Applied on the loop's thread because closing goes through the loop's
    /// own event queue, which another thread cannot reach.
    Close,
}

impl Edit {
    /// Applies this change through the setter it names.
    pub(crate) fn apply(self, window: &mut Window) {
        match self {
            Edit::Title(title) => window.set_title(&title),
            Edit::Size(width, height) => window.set_size(width, height),
            Edit::Position(left, top) => window.set_position(left, top),
            Edit::Fullscreen(on) => window.set_fullscreen(on),
            Edit::Cursor(cursor) => window.set_cursor(cursor),
            Edit::CursorCss(cursor) => window.set_cursor_css(&cursor),
            Edit::Visible(on) => window.set_visible(on),
            Edit::Resizable(on) => window.set_resizable(on),
            Edit::Borderless(on) => window.set_borderless(on),
            Edit::Background(color) => window.set_background(&color),
            Edit::Fit(fit) => window.set_fit(fit),
            Edit::Page(page) => window.set_page(page),
            Edit::Close => App::close_window(window.id()),
        }
    }
}

/// What the loop wakes to: a request to run the queued edits now.
///
/// Boxed so this module needs nothing of the event loop's own types; the
/// loop installs one when it starts and removes it when it stops.
type Wake = Box<dyn Fn() + Send + Sync>;

/// A window the registry knows to be open: queued by `open` or shown by the
/// loop, and not closed since.
struct Entry {
    /// The window as the loop last reported it.
    spec: WindowSpec,
    /// The thread holding the window, which is the only one that can hand it
    /// a [`Canvas`].
    owner: ThreadId,
}

/// Every open window's last known state, and the changes waiting for the
/// loop. One lock guards both, so a change is never queued for a window
/// that the same instant is being forgotten.
#[derive(Default)]
struct Registry {
    windows: HashMap<u32, Entry>,
    /// Queued changes, each with its place in the one order every change
    /// shares -- canvases included, though they wait elsewhere.
    edits: Vec<(u64, u32, Edit)>,
    /// The next place in that order.
    next: u64,
    wake: Option<Wake>,
    /// Whether a wake is already on its way. Edits queued while one is cost
    /// nothing more: the drain that wake triggers takes them too.
    woken: bool,
}

static REGISTRY: LazyLock<Mutex<Registry>> = LazyLock::new(Default::default);

thread_local!(
    /// Canvases waiting for the window they were given to, held on the
    /// thread that owns both: a [`Canvas`] cannot cross threads.
    static CANVASES: RefCell<Vec<(u64, u32, Canvas)>> =
        const { RefCell::new(Vec::new()) };
);

/// The registry, whether or not a thread panicked holding it: every field is
/// left consistent between statements, so a poisoned lock holds nothing torn.
fn registry() -> MutexGuard<'static, Registry> {
    REGISTRY.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Records a window as open and owned by the calling thread.
pub(crate) fn register(spec: WindowSpec) {
    registry().windows.insert(
        spec.id,
        Entry {
            spec,
            owner: thread::current().id(),
        },
    );
}

/// Updates the state a handle reads back for an open window.
pub(crate) fn record(spec: &WindowSpec) {
    if let Some(entry) = registry().windows.get_mut(&spec.id) {
        entry.spec = spec.clone();
    }
}

/// Forgets a closed window, along with any change still queued for it.
pub(crate) fn forget(id: u32) {
    let mut registry = registry();
    registry.windows.remove(&id);
    registry.edits.retain(|(_, target, _)| *target != id);
}

/// Forgets every window, as when the loop stops.
pub(crate) fn forget_all() {
    let mut registry = registry();
    registry.windows.clear();
    registry.edits.clear();
}

/// Installs or removes what wakes the loop for queued edits.
pub(crate) fn set_wake(wake: Option<Wake>) {
    let mut registry = registry();
    registry.wake = wake;
    registry.woken = false;
}

/// A change ready to apply: a queued edit or a canvas handed over.
enum Change {
    Edit(Edit),
    Canvas(Canvas),
}

/// Takes every queued change for the calling thread's windows, edits and
/// canvases merged back into the order they were made in.
///
/// Clears the pending-wake flag before releasing the lock, so a change
/// queued after this sends a fresh wake rather than waiting on one already
/// spent.
fn take_changes() -> Vec<(u32, Change)> {
    let edits = {
        let mut registry = registry();
        registry.woken = false;
        std::mem::take(&mut registry.edits)
    };
    let canvases = CANVASES.with_borrow_mut(std::mem::take);

    let mut changes: Vec<(u64, u32, Change)> = edits
        .into_iter()
        .map(|(seq, id, edit)| (seq, id, Change::Edit(edit)))
        .chain(
            canvases
                .into_iter()
                .map(|(seq, id, canvas)| (seq, id, Change::Canvas(canvas))),
        )
        .collect();
    changes.sort_by_key(|(seq, _, _)| *seq);
    changes
        .into_iter()
        .map(|(_, id, change)| (id, change))
        .collect()
}

/// Applies every queued change to the windows it names, returning the ids
/// of the windows that changed, each once.
///
/// A change for a window not among `windows` -- closed between being queued
/// and now -- is dropped.
pub(crate) fn apply_queued(windows: &mut [Window]) -> Vec<u32> {
    let mut changed = Vec::new();
    for (id, change) in take_changes() {
        if let Some(window) = windows.iter_mut().find(|w| w.id() == id) {
            match change {
                Change::Edit(edit) => edit.apply(window),
                Change::Canvas(canvas) => window.set_canvas(canvas),
            }
            if !changed.contains(&id) {
                changed.push(id);
            }
        }
    }
    changed
}

/// Queues `edit` for window `id` if it is open, waking the loop once.
fn queue(id: u32, edit: Edit) {
    let mut registry = registry();
    if !registry.windows.contains_key(&id) {
        return;
    }
    let seq = registry.next;
    registry.next += 1;
    registry.edits.push((seq, id, edit));
    wake(&mut registry);
}

fn wake(registry: &mut Registry) {
    if !registry.woken
        && let Some(wake) = registry.wake.as_ref()
    {
        wake();
        registry.woken = true;
    }
}

/// A window that can be changed after it has opened, from any thread.
///
/// Taken from [`Window::handle`] before opening, or from [`App::window`]
/// afterwards. It is an id and nothing more, so it is `Copy` and `Send`:
/// capture it in both handlers, keep it in a struct, or move it to a worker
/// thread. See the [module documentation](self) for when each change lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WindowHandle {
    id: u32,
}

impl WindowHandle {
    pub(crate) fn new(id: u32) -> Self {
        Self { id }
    }

    /// The id addressing this window, as [`Window::id`] returns it.
    pub fn id(&self) -> u32 {
        self.id
    }

    /// Whether the window is open: queued by [`Window::open`] or showing,
    /// and not closed since.
    ///
    /// Unlike [`App::window_is_open`] this answers on any thread, and counts
    /// a window queued before [`App::run`] as open.
    pub fn is_open(&self) -> bool {
        registry().windows.contains_key(&self.id)
    }

    /// The window as it currently stands, or `None` once it is closed.
    ///
    /// Size and position are the window system's, read back after each
    /// frame, so a window the user dragged or resized reports where it is
    /// rather than where it was put. A change queued through this handle
    /// shows here once the loop has applied it.
    pub fn spec(&self) -> Option<WindowSpec> {
        registry()
            .windows
            .get(&self.id)
            .map(|entry| entry.spec.clone())
    }

    /// Closes the window, as [`App::close_window`] does, from any thread.
    pub fn close(&self) {
        queue(self.id, Edit::Close);
    }

    /// As [`Window::set_title`].
    pub fn set_title(&self, title: &str) {
        queue(self.id, Edit::Title(title.to_string()));
    }

    /// As [`Window::set_size`].
    pub fn set_size(&self, width: f32, height: f32) {
        queue(self.id, Edit::Size(width, height));
    }

    /// As [`Window::set_position`].
    pub fn set_position(&self, left: f32, top: f32) {
        queue(self.id, Edit::Position(left, top));
    }

    /// As [`Window::set_fullscreen`].
    pub fn set_fullscreen(&self, fullscreen: bool) {
        queue(self.id, Edit::Fullscreen(fullscreen));
    }

    /// As [`Window::set_cursor`].
    pub fn set_cursor(&self, cursor: Option<Cursor>) {
        queue(self.id, Edit::Cursor(cursor));
    }

    /// As [`Window::set_cursor_css`].
    pub fn set_cursor_css(&self, cursor: &str) {
        queue(self.id, Edit::CursorCss(cursor.to_string()));
    }

    /// As [`Window::set_visible`].
    pub fn set_visible(&self, visible: bool) {
        queue(self.id, Edit::Visible(visible));
    }

    /// As [`Window::set_resizable`].
    pub fn set_resizable(&self, resizable: bool) {
        queue(self.id, Edit::Resizable(resizable));
    }

    /// As [`Window::set_borderless`].
    pub fn set_borderless(&self, borderless: bool) {
        queue(self.id, Edit::Borderless(borderless));
    }

    /// As [`Window::set_background`].
    pub fn set_background(&self, color: &str) {
        queue(self.id, Edit::Background(color.to_string()));
    }

    /// As [`Window::set_fit`].
    pub fn set_fit(&self, fit: Fit) {
        queue(self.id, Edit::Fit(fit));
    }

    /// As [`Window::set_page`].
    pub fn set_page(&self, page: u32) {
        queue(self.id, Edit::Page(page));
    }

    /// As [`Window::set_canvas`], on the thread that owns the window.
    ///
    /// A [`Canvas`] cannot cross threads, so only the thread that opened the
    /// window -- the one running its loop -- can give it one. Called from
    /// any other thread, or for a window that is not open, this hands the
    /// canvas back as the error rather than dropping it.
    pub fn set_canvas(&self, canvas: Canvas) -> Result<(), Canvas> {
        let mut registry = registry();
        match registry.windows.get(&self.id) {
            Some(entry) if entry.owner == thread::current().id() => {
                let seq = registry.next;
                registry.next += 1;
                CANVASES.with_borrow_mut(|canvases| {
                    canvases.push((seq, self.id, canvas));
                });
                wake(&mut registry);
                Ok(())
            }
            _ => Err(canvas),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    };

    use super::*;

    // Every test here touches the one process-wide registry, and the test
    // harness runs tests on several threads at once. Serialised so that one
    // test's `forget_all` cannot land in the middle of another's queue.
    static SERIAL: Mutex<()> = Mutex::new(());

    /// A window registered as open on the calling thread, as `open` would.
    fn opened() -> Window {
        let window = Window::new(10.0, 10.0);
        register(window.spec());
        window
    }

    fn serial() -> MutexGuard<'static, ()> {
        let guard = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        forget_all();
        set_wake(None);
        guard
    }

    #[test]
    fn a_handle_is_copy_send_and_sync() {
        fn check<T: Copy + Send + Sync + 'static>() {}
        check::<WindowHandle>();
    }

    #[test]
    fn a_change_for_a_window_never_opened_is_dropped() {
        let _serial = serial();
        let mut windows = vec![Window::new(10.0, 10.0)];
        let handle = windows[0].handle();

        handle.set_title("never");
        assert!(!handle.is_open());
        assert_eq!(apply_queued(&mut windows), Vec::<u32>::new());
        assert_eq!(windows[0].title(), "");
    }

    #[test]
    fn changes_apply_through_the_setters_in_the_order_made() {
        let _serial = serial();
        let mut windows = vec![opened()];
        let handle = windows[0].handle();

        handle.set_title("first");
        handle.set_size(300.0, 200.0);
        handle.set_title("last");
        handle.set_cursor_css("pointer");

        assert_eq!(apply_queued(&mut windows), vec![handle.id()]);
        assert_eq!(windows[0].title(), "last", "the later title wins");
        assert_eq!((windows[0].width(), windows[0].height()), (300.0, 200.0));
        assert_eq!(windows[0].cursor(), "pointer");
        assert_eq!(apply_queued(&mut windows), Vec::<u32>::new(), "drained");
    }

    #[test]
    fn a_change_made_before_run_is_the_setting_the_window_opens_with() {
        // `run` drains the queue before it opens anything, which is this
        // call on the windows `open` queued.
        let _serial = serial();
        let mut windows = vec![opened()];
        windows[0].handle().set_fullscreen(true);
        apply_queued(&mut windows);
        assert!(windows[0].fullscreen());
    }

    #[test]
    fn a_change_from_another_thread_reaches_the_window() {
        let _serial = serial();
        let mut windows = vec![opened()];
        let handle = windows[0].handle();

        std::thread::spawn(move || {
            handle.set_title("from a worker");
            handle.set_visible(false);
        })
        .join()
        .expect("the worker queues and exits");

        apply_queued(&mut windows);
        assert_eq!(windows[0].title(), "from a worker");
        assert!(!windows[0].visible());
    }

    #[test]
    fn a_change_racing_a_close_is_dropped_with_the_window() {
        // Queued, then the window closes before the loop drains: the close
        // removes the window and its queued changes under one lock, so there
        // is no moment at which the change is queued for a window that is
        // gone.
        let _serial = serial();
        let mut windows = vec![opened()];
        let handle = windows[0].handle();

        handle.set_title("too late");
        forget(handle.id());

        assert!(!handle.is_open());
        assert_eq!(apply_queued(&mut windows), Vec::<u32>::new());
        assert_eq!(windows[0].title(), "");
        assert_eq!(registry().edits.len(), 0, "nothing left behind");
    }

    #[test]
    fn a_change_after_the_loop_has_stopped_is_dropped() {
        // What `App::run` does on the way out, however the loop ended.
        let _serial = serial();
        let window = opened();
        let handle = window.handle();

        set_wake(None);
        forget_all();

        handle.set_title("after exit");
        assert!(!handle.is_open());
        assert!(handle.spec().is_none());
        assert_eq!(registry().edits.len(), 0, "nothing accumulates");
    }

    #[test]
    fn edits_queued_together_send_one_wake_and_the_next_drain_rearms_it() {
        let _serial = serial();
        let wakes = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&wakes);
        set_wake(Some(Box::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        })));

        let mut windows = vec![opened()];
        let handle = windows[0].handle();
        handle.set_title("a");
        handle.set_title("b");
        handle.set_visible(false);
        assert_eq!(wakes.load(Ordering::SeqCst), 1, "one wake for three");

        apply_queued(&mut windows);
        handle.set_title("c");
        assert_eq!(wakes.load(Ordering::SeqCst), 2, "a drain re-arms it");
        set_wake(None);
    }

    #[test]
    fn nothing_is_lost_when_the_loop_drains_while_others_push() {
        // Eight threads push 200 titles each while this thread drains as
        // fast as it can. Every edit lands exactly once, and each thread's
        // edits arrive in the order that thread made them.
        let _serial = serial();
        let windows = [opened()];
        let handle = windows[0].handle();
        let threads = 8;
        let per_thread = 200;
        let start = Arc::new(Barrier::new(threads + 1));

        let workers: Vec<_> = (0..threads)
            .map(|t| {
                let start = Arc::clone(&start);
                std::thread::spawn(move || {
                    start.wait();
                    for n in 0..per_thread {
                        handle.set_title(&format!("{t}:{n}"));
                    }
                })
            })
            .collect();

        start.wait();
        let mut seen: Vec<String> = Vec::new();
        let drain = |seen: &mut Vec<String>| {
            for (_, change) in take_changes() {
                if let Change::Edit(Edit::Title(title)) = change {
                    seen.push(title);
                }
            }
        };
        while workers.iter().any(|w| !w.is_finished()) {
            drain(&mut seen);
        }
        for worker in workers {
            worker.join().expect("a worker finishes");
        }
        drain(&mut seen);

        assert_eq!(seen.len(), threads * per_thread, "every edit, once");
        for t in 0..threads {
            let order: Vec<usize> = seen
                .iter()
                .filter_map(|title| title.strip_prefix(&format!("{t}:")))
                .map(|n| n.parse().expect("a number"))
                .collect();
            assert_eq!(order, (0..per_thread).collect::<Vec<_>>());
        }
    }

    #[test]
    fn two_threads_setting_one_property_end_with_the_later_queued() {
        let _serial = serial();
        let mut windows = vec![opened()];
        let handle = windows[0].handle();

        std::thread::spawn(move || handle.set_title("first thread"))
            .join()
            .expect("queued");
        std::thread::spawn(move || handle.set_title("second thread"))
            .join()
            .expect("queued");

        apply_queued(&mut windows);
        assert_eq!(windows[0].title(), "second thread");
    }

    #[test]
    fn a_canvas_goes_only_to_a_window_on_its_own_thread() {
        let _serial = serial();
        let mut windows = vec![opened()];
        let handle = windows[0].handle();

        // From another thread the canvas comes back rather than vanishing.
        let returned = std::thread::spawn(move || {
            handle
                .set_canvas(Canvas::new(7.0, 7.0))
                .err()
                .map(|c| c.width())
        })
        .join()
        .expect("the worker returns");
        assert_eq!(returned, Some(7.0));

        // On the owning thread it is taken, in order with the other changes.
        let mut canvas = Canvas::new(40.0, 30.0);
        canvas.new_page();
        handle.set_page(0);
        assert!(handle.set_canvas(canvas).is_ok());
        apply_queued(&mut windows);
        assert_eq!(windows[0].canvas().width(), 40.0);
        assert_eq!(windows[0].page(), 1, "the new canvas's last page");

        // A canvas then a page: the page set after the canvas wins, though
        // the canvas waited in a different queue.
        let mut canvas = Canvas::new(20.0, 20.0);
        canvas.new_page();
        assert!(handle.set_canvas(canvas).is_ok());
        handle.set_page(0);
        apply_queued(&mut windows);
        assert_eq!(windows[0].canvas().width(), 20.0);
        assert_eq!(windows[0].page(), 0, "the page set after the canvas");

        // A closed window hands it back too.
        forget(handle.id());
        assert!(handle.set_canvas(Canvas::new(5.0, 5.0)).is_err());
    }

    #[test]
    fn spec_reads_back_what_the_loop_recorded() {
        let _serial = serial();
        let window = opened();
        let handle = window.handle();
        let mut spec = window.spec();
        spec.title = "as shown".to_string();
        record(&spec);
        assert_eq!(handle.spec().map(|s| s.title), Some("as shown".into()));
    }
}
