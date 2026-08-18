use neon::prelude::*;
use serde_json::Value;
use std::{
    cell::RefCell,
    iter::zip,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};
use winit::{
    event::{ElementState, Event, KeyEvent, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    keyboard::{KeyCode, PhysicalKey},
    platform::{
        pump_events::EventLoopExtPumpEvents,
        run_on_demand::EventLoopExtRunOnDemand,
    },
};

use super::{
    event::AppEvent, session, window::WindowSpec, window_mgr::WindowManager,
};
use crate::context::{BoxedContext2D, page::Page};

/// Frames per second a window animates at until told otherwise.
///
/// Sixty, which is what a display has been by default for long enough that
/// it is what a caller means by "smooth". Higher panels exist; a canvas
/// cannot ask the display what it is, and drawing faster than the compositor
/// presents costs power for nothing.
const DEFAULT_FRAME_RATE: u64 = 60;

/// Nanoseconds in a second, for turning a frame rate into a frame duration.
const NANOS_PER_SECOND: u64 = 1_000_000_000;

/// How long before a frame's deadline to wake and start spinning, at most.
///
/// A millisecond and a half. `Instant`-based sleeping overshoots by
/// something on the order of a millisecond on every platform this runs on,
/// so the loop wakes early and busy-waits the rest; this is how much of the
/// frame it is willing to spend doing that. See `Frame::pacing`, which caps
/// it at a tenth of the frame so a slow rate does not spin for longer than
/// it needs to.
const SPIN_MARGIN_NANOS: u64 = 1_500_000;

thread_local!(
    static APP: RefCell<App> = RefCell::new(App::default());
    /// The winit event loop, where there is a display to give it one.
    ///
    /// `None` where there is not: a container, a CI runner, an `ssh`
    /// session with nothing forwarded. That is not an unsupported platform
    /// and not a broken build -- it is the ordinary state of a server, and
    /// the comment that used to sit here said creation "only fails on
    /// unsupported platforms" and `expect`ed on the strength of it. It fails
    /// on Linux with no `WAYLAND_DISPLAY` and no `DISPLAY` too, and the
    /// panic crossed the binding as `internal error in Neon module`, naming
    /// neither the display nor the window that wanted one.
    static EVENT_LOOP: RefCell<Option<EventLoop<AppEvent>>> =
        RefCell::new(EventLoop::with_user_event().build().ok());
    static PROXY: RefCell<Option<EventLoopProxy<AppEvent>>> =
        RefCell::new(EVENT_LOOP.with_borrow(|event_loop| {
            event_loop.as_ref().map(EventLoop::create_proxy)
        }));
);

static RENDER_CALLBACK: OnceLock<Arc<Root<JsFunction>>> = OnceLock::new();

/// Which runtime drives the event loop.
#[derive(Copy, Clone)]
pub enum LoopMode {
    /// winit owns the thread and blocks in its own loop.
    Native,
    /// The Node event loop drives frames, and winit is pumped from it.
    Node,
}

/// Which moment in the loop a dispatch is serving.
///
/// The two differ in what the consumer is expected to want, not in what it is
/// allowed to do: a window has just appeared and its geometry is settled, or
/// a frame is due and there are events to deliver and content to collect.
#[derive(Copy, Clone, PartialEq, Debug)]
pub(crate) enum Frame {
    /// A window was just opened.
    Opened,
    /// The cadence reached the next frame.
    Tick,
}

/// The process-wide application: the event loop, the open windows, and the
/// frame cadence driving them.
///
/// One instance per thread, held in thread-local state; every method here is
/// an associated function operating on the calling thread's instance.
pub struct App {
    /// Which runtime drives the loop.
    pub mode: LoopMode,
    windows: WindowManager,
    cadence: Cadence,
}

impl Default for App {
    fn default() -> Self {
        Self {
            windows: WindowManager::default(),
            cadence: Cadence::default(),
            mode: LoopMode::Native,
        }
    }
}

fn add_event(event: AppEvent) {
    PROXY.with_borrow_mut(|proxy| {
        proxy
            .as_mut()
            .and_then(|proxy| proxy.send_event(event).ok())
    });
}

impl App {
    // `register` and `activate` take `neon` types, so they stay crate-private
    // like the other binding entry points -- the public API does not expose
    // them.
    pub(crate) fn register(callback: Root<JsFunction>) {
        RENDER_CALLBACK.get_or_init(|| Arc::new(callback));
    }

    /// Sets which runtime drives the event loop.
    pub fn set_mode(mode: LoopMode) {
        APP.with_borrow_mut(|app| app.mode = mode);
    }

    /// The windows the loop currently has open.
    ///
    /// Each is the spec as it stands now, which is not always the spec it
    /// was opened with: a window moves and resizes without being asked, and
    /// [`Fit::Resize`](super::window::Fit::Resize) changes the canvas to
    /// match.
    ///
    /// Empty before [`run`](Self::run) and after the last window closes.
    pub fn windows() -> Vec<WindowSpec> {
        APP.with_borrow(|app| app.windows.specs())
    }

    /// Whether the window with this id is still open.
    ///
    /// The readback for [`close_window`](Self::close_window), and the
    /// counterpart to the JavaScript `Window.closed` -- inverted, because a
    /// window that has never been opened is not the same as one that has
    /// closed, and "is open" is false for both without claiming otherwise.
    ///
    /// Lives here rather than on [`Window`](super::session::Window) because
    /// `Window::open` consumes the window: once the loop has it, an id is
    /// the only handle a caller still holds.
    pub fn window_is_open(id: u32) -> bool {
        APP.with_borrow(|app| {
            app.windows.specs().iter().any(|spec| spec.id == id)
        })
    }

    /// Whether the loop currently has any window open.
    ///
    /// False before [`run`](Self::run) is called and after the last window
    /// closes -- which in [`LoopMode::Native`] is also when `run` returns.
    pub fn running() -> bool {
        APP.with_borrow(|app| !app.windows.is_empty())
    }

    /// Whether every open window is idle -- none animating, none with
    /// events waiting.
    ///
    /// An idle loop is one that will not redraw until something arrives, so
    /// this is what a caller polls to know it can stop pumping.
    pub fn idle() -> bool {
        APP.with_borrow(|app| {
            app.windows.is_empty() || !app.windows.has_ui_changes()
        })
    }

    /// Sets the target frame rate for animated windows.
    ///
    /// # Panics
    ///
    /// The first call on a thread creates the event loop, and panics if the
    /// platform will not provide one. The same applies to
    /// [`Window::open`](super::session::Window::open),
    /// [`App::close_window`] and [`App::quit`].
    pub fn set_fps(fps: f32) {
        add_event(AppEvent::FrameRate(fps as u64));
    }

    /// Queues a new window to be opened on the next loop iteration,
    /// rendering `page`.
    /// Crate-internal: `Page` is `pub(crate)`, so an outside caller cannot
    /// name the second argument and could never have called this. The route
    /// in from outside is [`Window::open`](super::session::Window::open),
    /// which builds the page and comes here.
    pub(crate) fn open_window(spec: WindowSpec, page: Page) {
        add_event(AppEvent::Open(spec, page));
    }

    /// Queues the window with this id to be closed.
    pub fn close_window(token: u32) {
        add_event(AppEvent::Close(token));
    }

    /// Closes every window and stops the event loop.
    pub fn quit() {
        APP.with_borrow_mut(|app| app.windows.remove_all());
        add_event(AppEvent::Quit);
    }

    /// Opens every window queued by [`Window::open`](super::session::Window)
    /// and drives them until the last one closes.
    ///
    /// Blocks, and takes over the calling thread -- winit's loop owns a
    /// thread for its lifetime, and on macOS that thread has to be the main
    /// one. This is the Rust counterpart to the Node binding's `activate`,
    /// which reaches the same loop by scheduling it onto the JavaScript main
    /// thread instead.
    ///
    /// Returns when no windows remain, or when [`App::quit`] is called from a
    /// handler.
    ///
    /// # Panics
    ///
    /// Panics if the platform will not provide an event loop.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use meo_skia_canvas::prelude::*;
    ///
    /// let mut win = Window::new(300.0, 150.0);
    /// win.on_draw(|ctx, _frame| {
    ///     ctx.set_fill_style_css("tomato").ok();
    ///     ctx.fill_rect(0.0, 0.0, 300.0, 150.0);
    /// });
    /// win.open();
    ///
    /// App::run();
    /// ```
    // Same deprecation as `activate` below, for the same reason: winit's
    // replacement takes an `ApplicationHandler` trait object, and this loop
    // is driven by a closure that borrows the caller's windows.
    #[allow(deprecated)]
    pub fn run() {
        let mut windows = session::take_pending();

        // Queued before the loop starts, which is the only way in: winit
        // creates windows inside a running loop, so these are picked up as
        // `AppEvent::Open` on the first pass rather than opened here.
        for window in &mut windows {
            App::open_window(window.spec(), window.render());
        }

        APP.with_borrow_mut(|app| {
            EVENT_LOOP.with_borrow_mut(|event_loop| {
                // Nothing to run on, which `activate` refuses before it gets
                // this far; a window opened without one simply never draws.
                let Some(event_loop) = event_loop.as_mut() else {
                    return;
                };
                let dispatch = |_frame: Frame, manager: &mut WindowManager| {
                    for (id, events) in manager.take_ui_events() {
                        if let Some(window) =
                            windows.iter_mut().find(|w| w.spec().id == id)
                        {
                            window.deliver(&events);
                        }
                    }

                    // The specs come back before the frame is drawn, so a
                    // handler reading its window mid-draw sees the size it
                    // actually has rather than the one it was opened with.
                    for spec in manager.specs() {
                        if let Some(window) =
                            windows.iter_mut().find(|w| w.spec().id == spec.id)
                        {
                            window.adopt(spec);
                        }
                    }

                    for window in windows.iter_mut() {
                        let page = window.render();
                        manager.update_window(window.spec(), page);
                    }
                };

                let handler = app.event_handler(dispatch);
                event_loop.set_control_flow(ControlFlow::Wait);
                event_loop.run_on_demand(handler).ok();
            })
        });
    }

    /// Whether this process has a display to open a window on.
    ///
    /// Asked by [`crate::gui::activate`] so that a machine without one is
    /// told so, rather than left with a window that never draws.
    pub(crate) fn has_display() -> bool {
        EVENT_LOOP.with_borrow(|event_loop| event_loop.is_some())
    }

    #[allow(deprecated)]
    pub(crate) fn activate(channel: Channel, deferred: neon::types::Deferred) {
        std::thread::spawn(move || {
            loop {
                // schedule a callback on the node event loop
                let keep_running = channel
                    .send(move |mut cx| {
                        // define closure to relay events to js and
                        // receive canvas updates in return
                        let dispatch =
                            |frame: Frame, windows: &mut WindowManager| {
                                let payload = match frame {
                                    Frame::Opened => windows.get_geometry(),
                                    Frame::Tick => windows.get_ui_changes(),
                                };
                                // The `.ok()` is where the discard now
                                // happens, and it is the same discard as
                                // before. A throw means the JS handler
                                // raised, and that frame is already lost --
                                // the loop's own state is untouched, so it
                                // carries on to the next one.
                                App::dispatch_events(
                                    &mut cx,
                                    payload,
                                    Some(windows),
                                )
                                .ok();
                            };

                        // run the winit event loop (either once or until all
                        // windows are closed depending on mode)
                        APP.with_borrow_mut(|app| {
                            EVENT_LOOP.with_borrow_mut(|event_loop| {
                                // Nothing to run on. `activate` refuses
                                // before reaching here, so this is the
                                // unreachable half of the same guard: stop
                                // rather than spin on a loop that is not
                                // there.
                                let Some(event_loop) = event_loop.as_mut()
                                else {
                                    return Ok(false);
                                };
                                match app.mode {
                                    LoopMode::Native => {
                                        let handler =
                                            app.event_handler(dispatch);
                                        event_loop.set_control_flow(
                                            ControlFlow::Wait,
                                        );
                                        event_loop.run_on_demand(handler).ok();
                                        Ok(false) // final window was closed
                                    }
                                    LoopMode::Node => {
                                        let poll_time =
                                            app.cadence.next_wakeup()
                                                - Instant::now();
                                        let handler =
                                            app.event_handler(dispatch);
                                        event_loop.pump_events(
                                            Some(poll_time),
                                            handler,
                                        );
                                        Ok(app.cadence.should_continue()
                                            || !app.windows.is_empty())
                                    }
                                }
                            })
                        })
                    })
                    .join();

                match keep_running {
                    Ok(true) => continue,
                    _ => break,
                }
            }

            // resolve the promise
            deferred.settle_with(&channel, move |mut cx| Ok(cx.undefined()));
        });
    }

    fn dispatch_events(
        cx: &mut TaskContext,
        events: Value,
        window_mgr: Option<&mut WindowManager>,
    ) -> NeonResult<()> {
        // window_mgr is only present if it's time to collect updated canvas
        // contents from js
        let is_render = window_mgr.is_some();

        // js callback is passed render flag & json-encoded event queue
        let mut call = match RENDER_CALLBACK.get() {
            None => return Ok(()),
            Some(callback) => callback.to_inner(cx).call_with(cx),
        };
        call.arg(cx.boolean(is_render))
            .arg(cx.string(events.to_string()));

        match window_mgr {
            None => call.exec(cx)?, /* if this is just a UI-event delivery, */
            // fire & forget
            Some(window_mgr) => {
                // for a full roundtrip, first pass events to js
                let response = call
                    .apply::<JsValue, _>(cx)?
                    .downcast::<JsArray, _>(cx)
                    .or_throw(cx)?
                    .to_vec(cx)?;

                // then unpack the returned window specs & contexts
                let specs_json = response[0]
                    .downcast::<JsString, _>(cx)
                    .or_throw(cx)?
                    .value(cx);
                let specs: Vec<WindowSpec> = serde_json::from_str(&specs_json)
                    .or_else(|err| {
                        cx.throw_error(format!(
                            "Malformed response from window event handler: {}",
                            err
                        ))
                    })?;

                let contexts = response[1]
                    .downcast::<JsArray, _>(cx)
                    .or_throw(cx)?
                    .to_vec(cx)?;
                let pages = contexts.iter().map(|boxed| {
                    boxed
                        .downcast::<BoxedContext2D, _>(cx)
                        .ok()
                        .map(|ctx| ctx.borrow().get_page())
                });

                // update each window with its new state & content
                zip(specs, pages)
                    .filter_map(|(spec, page)| page.map(|page| (spec, page)))
                    .for_each(|(spec, page)| {
                        window_mgr.update_window(spec, page)
                    });
            }
        };

        Ok(())
    }

    /// Builds the winit handler, with `dispatch` standing in for whatever
    /// consumes a frame's events and supplies the next one's content.
    ///
    /// `dispatch` is the only part of the loop that knows which runtime it is
    /// serving. The Node path sends the payload across a `neon` channel and
    /// feeds the returned specs and pages back through
    /// [`WindowManager::update_window`]; a Rust path calls the window's own
    /// handlers and does the same. Everything else -- the sieve, the cadence,
    /// window lifecycle -- is common.
    ///
    /// It returns nothing because every call site discarded the result
    /// already: a `NeonResult` here meant the error was constructed, `.ok()`d
    /// away, and the frame carried on regardless. Dropping it from the bound
    /// is what lets a non-neon caller supply one.
    ///
    /// It is handed the manager rather than a payload built from it, because
    /// building the payload is destructive: `get_ui_changes` drains every
    /// sieve into JSON, so a Rust dispatch handed the result could no longer
    /// reach the events themselves. Each runtime now takes what it can use --
    /// JSON on one side, [`WindowManager::take_ui_events`] on the other --
    /// and [`Frame`] says which moment it is.
    fn event_handler<F>(
        &mut self,
        mut dispatch: F,
    ) -> impl FnMut(Event<AppEvent>, &ActiveEventLoop) + use<'_, F>
    where
        F: FnMut(Frame, &mut WindowManager),
    {
        move |event, event_loop| match event {
            Event::WindowEvent {
                event: ref win_event,
                window_id,
            } => {
                self.windows
                    .find(&window_id, |win| win.sieve.capture(win_event));

                match win_event {
                    WindowEvent::Destroyed | WindowEvent::CloseRequested => {
                        self.windows.remove(&window_id);

                        // after the last window is closed, either exit (in
                        // run_on_demand mode)
                        // or wait for the window destructor to run (in
                        // pump_events mode)
                        if self.windows.is_empty() {
                            match self.mode {
                                LoopMode::Native => event_loop.exit(),
                                LoopMode::Node => self.cadence.loop_again(),
                            }
                        }
                    }

                    WindowEvent::KeyboardInput {
                        event:
                            KeyEvent {
                                physical_key: PhysicalKey::Code(KeyCode::Escape),
                                state: ElementState::Pressed,
                                repeat: false,
                                ..
                            },
                        ..
                    } => {
                        self.windows
                            .find(&window_id, |win| win.set_fullscreen(false));
                    }

                    WindowEvent::Moved(loc) => {
                        self.windows.find(&window_id, |win| win.did_move(*loc));
                    }

                    WindowEvent::Resized(size) => {
                        self.windows
                            .find(&window_id, |win| win.did_resize(*size));
                    }

                    #[cfg(target_os = "macos")]
                    WindowEvent::Occluded(is_hidden) => {
                        self.windows.find(&window_id, |win| {
                            win.set_redrawing_suspended(*is_hidden)
                        });
                    }

                    WindowEvent::RedrawRequested => {
                        self.windows.find(&window_id, |win| win.redraw());
                    }

                    _ => {}
                }
            }

            Event::UserEvent(app_event) => match app_event {
                AppEvent::Open(spec, page) => {
                    self.windows.add(event_loop, spec, page);
                    dispatch(Frame::Opened, &mut self.windows);
                }
                AppEvent::Close(token) => {
                    self.windows.remove_by_token(token);
                }
                AppEvent::FrameRate(fps) => self.cadence.set_frame_rate(fps),
                AppEvent::Quit => {
                    event_loop.exit();
                }
            },

            Event::AboutToWait => {
                event_loop.set_control_flow(
                    // let the cadence decide when to switch to poll-mode or
                    // sleep the thread
                    self.cadence.on_next_frame(self.mode, || {
                        // relay UI-driven state changes to js and render the
                        // next frame in the (active) cadence
                        dispatch(Frame::Tick, &mut self.windows);
                    }),
                );
            }
            _ => {}
        }
    }
}

struct Cadence {
    rate: u64,
    last: Instant,
    needs_cleanup: Option<bool>,
}

impl Default for Cadence {
    fn default() -> Self {
        Self {
            rate: DEFAULT_FRAME_RATE,
            last: Instant::now(),
            needs_cleanup: Some(true), // ensure at least one post-Init loop
        }
    }
}

impl Cadence {
    fn loop_again(&mut self) {
        // flag that a clean-up event-loop pass is necessary (e.g., for
        // reflecting window closures)
        self.needs_cleanup = Some(true)
    }

    fn should_continue(&mut self) -> bool {
        self.needs_cleanup.take().is_some()
    }

    fn set_frame_rate(&mut self, rate: u64) {
        self.rate = rate;
    }

    pub fn next_wakeup(&self) -> Instant {
        let (frame_time, watch_interval) = self.pacing();
        let wakeup = Duration::from_nanos(frame_time - watch_interval);
        self.last + wakeup
    }

    /// How long a frame lasts, and how long before its deadline to wake and
    /// start spinning, both in nanoseconds.
    ///
    /// One place, because the two callers computed it identically and a
    /// change to one would have quietly desynchronised the wakeup from the
    /// deadline it exists to anticipate.
    ///
    /// The margin is a millisecond and a half, or a tenth of the frame --
    /// whichever is shorter. The tenth is what matters at low frame rates,
    /// where 1.5ms would be a needlessly early wake; the fixed ceiling is
    /// what matters above about 66fps, where a tenth of a frame is less
    /// than the scheduler can reliably deliver.
    fn pacing(&self) -> (u64, u64) {
        let frame_time = NANOS_PER_SECOND / self.rate.max(1);
        (frame_time, SPIN_MARGIN_NANOS.min(frame_time / 10))
    }

    pub fn on_next_frame<F: FnMut()>(
        &mut self,
        mode: LoopMode,
        mut draw: F,
    ) -> ControlFlow {
        // determine the upcoming deadlines for actually rendering and for
        // spinning in preparation
        let (frame_time, watch_interval) = self.pacing();
        let render = Duration::from_nanos(frame_time);
        let wakeup = Duration::from_nanos(frame_time - watch_interval);

        // if node is handling the event loop, we can't use polling to wait for
        // the render deadline. so instead we'll pause the thread for
        // the last 10% of the inter-frame time (up to 1.5ms), making
        // sure we can then draw immediately after
        let dt = self.last.elapsed();
        if matches!(mode, LoopMode::Node)
            && dt >= wakeup
            && dt < render
            && let Some(sleep_time) = render.checked_sub(self.last.elapsed())
        {
            spin_sleep::sleep(sleep_time);
        }

        // call the draw callback if it's time & make sure the next deadline is
        // in the future
        if self.last.elapsed() >= render {
            draw();
            while self.last < Instant::now() - render {
                self.last += render
            }
        }

        // if winit is in control, we can use waiting & polling to hit the
        // deadline
        match self.last.elapsed() < wakeup {
            true => ControlFlow::WaitUntil(self.last + wakeup),
            false => ControlFlow::Poll,
        }
    }
}
