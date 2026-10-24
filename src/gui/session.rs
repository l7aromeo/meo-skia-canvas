//! Opening a window from Rust.
//!
//! The JavaScript side has had this for a long time: construct a `Window`,
//! attach handlers, and the runtime drives it. This is the same thing for a
//! Rust caller, over the same engine -- one window type, one set of handlers,
//! one loop.
//!
//! The shape differs in one place, and winit decides it. Windows can only be
//! created inside a running event loop, so the object a caller configures is
//! not yet a window: it is what a window will be made from. [`Window::open`]
//! hands it over, [`App::run`](super::app::App::run) starts the loop, and the
//! real window appears once the loop is running.
//!
//! ```no_run
//! use meo_skia_canvas::prelude::*;
//!
//! let mut win = Window::new(512.0, 512.0);
//! win.set_title("Hello");
//!
//! win.on_draw(|ctx, frame| {
//!     ctx.set_fill_style_css("skyblue").ok();
//!     ctx.fill_rect(0.0, 0.0, 100.0 + frame as f32, 100.0);
//! });
//!
//! win.open();
//! App::run().expect("the event loop runs");
//! ```

use std::{
    cell::RefCell,
    collections::HashSet,
    fmt,
    num::NonZeroU32,
    sync::{
        LazyLock, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicU32, Ordering},
    },
};

use super::{
    app::App,
    event::UiEvent,
    handle::{self, WindowHandle},
    window::{Cursor, Fit, WindowSpec},
};
use crate::{
    canvas::Canvas, context::page::Page, context2d::Context2D,
    gui::window::DEFAULT_BACKGROUND,
};

thread_local!(
    /// Windows handed over by [`Window::open`] and not yet opened.
    ///
    /// They wait here rather than going straight to [`App::open_window`]
    /// because a caller's handlers have to travel with the spec, and the
    /// queue that carries specs into the loop is shared with the Node
    /// binding, which has no handlers to carry.
    static PENDING: RefCell<Vec<Window>> = const { RefCell::new(Vec::new()) };
);

/// Source of the ids that address windows for the rest of their lives.
///
/// Process-wide rather than per thread: a [`WindowHandle`] can be used from
/// any thread, so the id it carries has to name one window wherever it is
/// read.
static NEXT_ID: AtomicU32 = AtomicU32::new(1);

/// The id of every [`Window`] that exists, whether or not it has opened.
///
/// What [`Window::set_id`] checks a chosen id against. A window queued or
/// open is still a `Window`, so this covers those; and it covers two windows
/// built and given one id before either opens, which a check of the open
/// ones alone would let through, the second to open then taking the first's
/// place. A window leaves it when it closes, through [`close_on`],
/// though the loop keeps the closed `Window` until it stops.
static HELD: LazyLock<Mutex<HashSet<u32>>> = LazyLock::new(Default::default);

/// The held ids, whether or not a thread panicked holding them: the set is
/// left consistent between statements.
fn held() -> MutexGuard<'static, HashSet<u32>> {
    HELD.lock().unwrap_or_else(PoisonError::into_inner)
}

/// An id [`Window::set_id`] was asked for that another window holds.
///
/// A window holds its id from the moment it is built until it closes, or is
/// dropped without having opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WindowIdTaken {
    /// The id that was asked for.
    pub id: u32,
}

impl fmt::Display for WindowIdTaken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "window id {} is held by another window", self.id)
    }
}

impl std::error::Error for WindowIdTaken {}

/// The index of a canvas's newest page, which a window shows until told
/// otherwise -- JavaScript's `canvas.pages.length`, counted from zero.
fn last_page(canvas: &Canvas) -> u32 {
    canvas.page_count().saturating_sub(1) as u32
}

/// Takes everything [`Window::open`] has queued.
pub(crate) fn take_pending() -> Vec<Window> {
    PENDING.with_borrow_mut(std::mem::take)
}

/// Marks every window in `windows` that `open` named on an earlier pass
/// and no longer does as closed, and frees its id.
///
/// Called by the loop at the top of each pass with the ids its window
/// manager still has. The id is freed here and nowhere else, so no other
/// window can be given it before the closed one is marked -- which is
/// what lets the loop's lookups by id pass over closed windows and never
/// hand one window's events to another.
pub(crate) fn close_on(windows: &mut [Window], open: &[u32]) {
    for window in windows.iter_mut().filter(|w| !w.closed) {
        match open.contains(&window.spec.id) {
            true => window.shown = true,
            // Absent before it was ever shown is not yet open.
            false if window.shown => {
                window.closed = true;
                held().remove(&window.spec.id);
            }
            false => {}
        }
    }
}

/// The draw handler: this window's context, and the frame number.
type DrawHandler = Box<dyn FnMut(&mut Context2D, u64)>;

/// The event handler, called once per event in arrival order.
type EventHandler = Box<dyn FnMut(&UiEvent)>;

/// A window to be opened, and the handlers that will drive it.
///
/// Holds its own [`Canvas`] unless given one. The draw handler is called with
/// that canvas's context, so a window is self-contained: nothing has to be
/// wired up between the surface drawn on and the surface shown.
pub struct Window {
    spec: WindowSpec,
    canvas: Canvas,
    on_draw: Option<DrawHandler>,
    on_event: Option<EventHandler>,
    frame: u64,
    /// Whether the loop's window manager has had this window open. Until it
    /// has, its absence there means it has not opened yet, not that it
    /// closed.
    shown: bool,
    /// Whether the loop has closed this window. A closed window stays in the
    /// loop's list until [`App::run`] returns, but has given up its id, so
    /// every lookup by id passes over it.
    closed: bool,
}

impl Window {
    /// Creates a window `width` by `height`, with a canvas to match.
    pub fn new(width: f32, height: f32) -> Self {
        Self::with_canvas(Canvas::new(width, height))
    }

    /// Creates a window showing an existing canvas, sized to it.
    ///
    /// The window takes the canvas: a window draws into the surface it
    /// shows, and sharing one between two owners would mean deciding which
    /// of them a frame belongs to. Reach it again through
    /// [`Window::canvas_mut`].
    pub fn with_canvas(canvas: Canvas) -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        held().insert(id);
        let page = last_page(&canvas);

        Self {
            spec: WindowSpec {
                id,
                left: None,
                top: None,
                title: String::new(),
                visible: true,
                resizable: true,
                borderless: false,
                fullscreen: false,
                background: DEFAULT_BACKGROUND.to_string(),
                page,
                width: canvas.width(),
                height: canvas.height(),
                cursor: "default".to_string(),
                fit: Fit::ContainX,
                text_contrast: 0.0,
                text_gamma: 1.4,
            },
            canvas,
            on_draw: None,
            on_event: None,
            frame: 0,
            shown: false,
            closed: false,
        }
    }

    /// The id addressing this window, for [`App::close_window`].
    pub fn id(&self) -> u32 {
        self.spec.id
    }

    /// Addresses this window by an id of the caller's choosing rather than
    /// the next one counted out, as JavaScript's `new Window({ id })` does.
    ///
    /// The id has to be free: one another window holds -- built and not yet
    /// dropped or closed, which covers one open or queued to open -- is
    /// refused, and this window keeps the id it had. A closed window's id is
    /// free at once, as JavaScript's is. Once chosen, the counter that numbers
    /// windows moves past it, so no window built later is handed it.
    ///
    /// ```no_run
    /// use meo_skia_canvas::prelude::*;
    /// use std::num::NonZeroU32;
    ///
    /// let mut window = Window::new(320.0, 240.0);
    /// window.set_id(NonZeroU32::new(5).expect("not zero"))?;
    /// assert_eq!(window.id(), 5);
    ///
    /// let mut other = Window::new(320.0, 240.0);
    /// let taken = other.set_id(NonZeroU32::new(5).expect("not zero"));
    /// assert_eq!(taken.map_err(|e| e.id), Err(5));
    /// # Ok::<(), WindowIdTaken>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`WindowIdTaken`] naming the id when another window holds it.
    pub fn set_id(&mut self, id: NonZeroU32) -> Result<(), WindowIdTaken> {
        let id = id.get();
        if id == self.spec.id {
            return Ok(());
        }
        let mut held = held();
        if held.contains(&id) {
            return Err(WindowIdTaken { id });
        }
        held.remove(&self.spec.id);
        held.insert(id);
        if let Some(next) = id.checked_add(1) {
            NEXT_ID.fetch_max(next, Ordering::Relaxed);
        }
        self.spec.id = id;
        Ok(())
    }

    /// A handle that changes this window after it has opened.
    ///
    /// Take it before [`open`](Self::open), which consumes the window; it
    /// can also be had afterwards from [`App::window`]. Changes made through
    /// it before the window is opened are dropped -- until then, set the
    /// window directly.
    pub fn handle(&self) -> WindowHandle {
        WindowHandle::new(self.spec.id)
    }

    /// The canvas this window shows.
    pub fn canvas(&self) -> &Canvas {
        &self.canvas
    }

    /// The canvas this window shows, mutably.
    ///
    /// For drawing outside a frame -- setting up state before the window
    /// opens, or from a handler for something other than a draw. Ordinary
    /// per-frame drawing goes in [`Window::on_draw`], which is handed the
    /// same canvas's context.
    pub fn canvas_mut(&mut self) -> &mut Canvas {
        &mut self.canvas
    }

    /// Sets the title-bar text.
    pub fn set_title(&mut self, title: &str) {
        self.spec.title = title.to_string();
    }

    /// Sets the color drawn behind the canvas, as a CSS color string.
    ///
    /// An unparseable value is replaced with a dark translucent grey when the
    /// window opens, which is what the JavaScript side does.
    pub fn set_background(&mut self, color: &str) {
        self.spec.background = color.to_string();
    }

    /// Sets how the canvas is scaled when the window's aspect ratio differs.
    pub fn set_fit(&mut self, fit: Fit) {
        self.spec.fit = fit;
    }

    /// Sets whether the user can resize the window.
    pub fn set_resizable(&mut self, resizable: bool) {
        self.spec.resizable = resizable;
    }

    /// Sets whether the window opens fullscreen.
    pub fn set_fullscreen(&mut self, fullscreen: bool) {
        self.spec.fullscreen = fullscreen;
    }

    /// Sets the window's size, in logical pixels.
    ///
    /// The window's own size, not the canvas's: the canvas is scaled into it
    /// by [`fit`](Self::fit), as setting `width` and `height` on a JavaScript
    /// `Window` does. A window starts at its canvas's size.
    pub fn set_size(&mut self, width: f32, height: f32) {
        self.spec.width = width;
        self.spec.height = height;
    }

    /// Replaces the canvas the window shows, displaying its last page.
    ///
    /// What assigning `canvas` on a JavaScript `Window` does: the window
    /// keeps its size and [`fit`](Self::fit), and shows the newest page of
    /// the canvas it was given. The text contrast and gamma stay the
    /// window's, since a Rust [`Canvas`] carries neither.
    pub fn set_canvas(&mut self, canvas: Canvas) {
        self.spec.page = last_page(&canvas);
        self.canvas = canvas;
    }

    /// Places the top-left corner, in logical pixels.
    ///
    /// Left unset, windows cascade from the last one opened.
    pub fn set_position(&mut self, left: f32, top: f32) {
        self.spec.left = Some(left);
        self.spec.top = Some(top);
    }

    /// Sets the handler drawing each frame.
    ///
    /// Called with the context of the page the window shows -- see
    /// [`page`](Self::page) -- and the frame number, counting from zero.
    /// Whatever it leaves there is what the window shows.
    pub fn on_draw<F>(&mut self, handler: F)
    where
        F: FnMut(&mut Context2D, u64) + 'static,
    {
        self.on_draw = Some(Box::new(handler));
    }

    /// Sets the handler receiving input and lifecycle events.
    ///
    /// Called once per event, in the order they arrived, before the frame
    /// they preceded is drawn.
    pub fn on_event<F>(&mut self, handler: F)
    where
        F: FnMut(&UiEvent) + 'static,
    {
        self.on_event = Some(Box::new(handler));
    }

    /// The window's title.
    pub fn title(&self) -> &str {
        &self.spec.title
    }

    /// The window's background, as the CSS colour string it was set with.
    pub fn background(&self) -> &str {
        &self.spec.background
    }

    /// How the canvas is scaled when the window's aspect ratio differs.
    pub fn fit(&self) -> Fit {
        self.spec.fit
    }

    /// Whether the user can resize the window.
    pub fn resizable(&self) -> bool {
        self.spec.resizable
    }

    /// Whether the window occupies the whole screen.
    pub fn fullscreen(&self) -> bool {
        self.spec.fullscreen
    }

    /// The canvas width in logical pixels.
    ///
    /// Read back from the window system rather than repeated from what was
    /// asked for: a window moves and resizes without being told to, and
    /// [`Fit::Resize`] changes the canvas to match.
    pub fn width(&self) -> f32 {
        self.spec.width
    }

    /// The canvas height in logical pixels. As [`width`](Self::width).
    pub fn height(&self) -> f32 {
        self.spec.height
    }

    /// The window's left edge in logical pixels, or `None` before it has
    /// been placed.
    pub fn left(&self) -> Option<f32> {
        self.spec.left
    }

    /// The window's top edge in logical pixels, or `None` before it has
    /// been placed.
    pub fn top(&self) -> Option<f32> {
        self.spec.top
    }

    /// The cursor shown over the window, by its CSS keyword.
    pub fn cursor(&self) -> &str {
        &self.spec.cursor
    }

    /// Sets the cursor shown over the window.
    ///
    /// [`None`] hides it, which is what CSS spells `none` -- hiding is not
    /// one of the icons, so it is not one of the [`Cursor`] variants either.
    ///
    /// # Examples
    ///
    /// ```
    /// # use meo_skia_canvas::prelude::*;
    /// # use meo_skia_canvas::gui::window::Cursor;
    /// let mut window = Window::new(400.0, 300.0);
    /// window.set_cursor(Some(Cursor::Crosshair));
    /// assert_eq!(window.cursor(), "crosshair");
    ///
    /// window.set_cursor(None);
    /// assert_eq!(window.cursor(), "none");
    /// ```
    pub fn set_cursor(&mut self, cursor: Option<Cursor>) {
        self.spec.cursor = match cursor {
            Some(cursor) => cursor.as_css().to_string(),
            None => "none".to_string(),
        };
    }

    /// Sets the cursor from a CSS `cursor` keyword.
    ///
    /// The string form of [`set_cursor`](Self::set_cursor), for a keyword
    /// copied from a stylesheet or from the JavaScript binding. A name the
    /// platform does not know hides the cursor, which is both what `"none"`
    /// is for and where an unrecognised name lands.
    pub fn set_cursor_css(&mut self, cursor: &str) {
        self.spec.cursor = cursor.to_string();
    }

    /// Whether the title bar and frame are hidden.
    pub fn borderless(&self) -> bool {
        self.spec.borderless
    }

    /// Hides or shows the title bar and frame.
    pub fn set_borderless(&mut self, borderless: bool) {
        self.spec.borderless = borderless;
    }

    /// Whether the window is shown.
    pub fn visible(&self) -> bool {
        self.spec.visible
    }

    /// Shows or hides the window without closing it.
    pub fn set_visible(&mut self, visible: bool) {
        self.spec.visible = visible;
    }

    /// Which page of the canvas is displayed, `0` being the first.
    ///
    /// The canvas's last page when the window was made, or given its canvas,
    /// until [`set_page`](Self::set_page) chooses another. A page the canvas
    /// gains afterwards is not shown until chosen, as in JavaScript, where
    /// `window.page` stays where it was when `canvas.newPage()` is called.
    pub fn page(&self) -> u32 {
        self.spec.page
    }

    /// Displays a different page of the canvas, `0` being the first.
    ///
    /// The canvas takes that page's size, as assigning `window.page` does in
    /// JavaScript, and the draw handler draws on it. A page past the last
    /// changes nothing: JavaScript refuses it with a `RangeError`, and this
    /// leaves the window showing what it showed.
    pub fn set_page(&mut self, page: u32) {
        if (page as usize) < self.canvas.page_count() {
            self.spec.page = page;
            self.canvas.adopt_page_size(page as usize);
        }
    }

    /// How much the rasterizer thickens small text, from `0.0` to `1.0`.
    pub fn text_contrast(&self) -> f32 {
        self.spec.text_contrast
    }

    /// The gamma glyph coverage is corrected against.
    pub fn text_gamma(&self) -> f32 {
        self.spec.text_gamma
    }

    /// Queues this window to open when the loop starts.
    ///
    /// Nothing appears until [`App::run`](super::app::App::run) is called.
    ///
    /// This consumes the window: the loop owns it from here, and the
    /// handlers it carries are the only things that run afterwards. So take
    /// a [`handle`](Self::handle) first if the window has to be changed or
    /// closed later, from a handler or from another thread; its
    /// [`is_open`](WindowHandle::is_open) is what the JavaScript
    /// `Window.closed` answers, inverted.
    ///
    /// ```no_run
    /// use meo_skia_canvas::prelude::*;
    ///
    /// let window = Window::new(400.0, 300.0);
    /// let handle = window.handle();
    /// window.open();
    ///
    /// // ... from a handler, or another thread:
    /// handle.close();
    /// ```
    pub fn open(self) {
        handle::register(self.spec());
        PENDING.with_borrow_mut(|pending| pending.push(self));
    }

    /// The spec describing this window as it currently stands.
    pub(crate) fn spec(&self) -> WindowSpec {
        self.spec.clone()
    }

    /// Adopts the spec the loop reports, which is the one that has been
    /// through the window system.
    ///
    /// A window moves and resizes without asking: the user drags it, tiles
    /// it, or takes it fullscreen. Reading it back keeps the handle honest
    /// rather than repeating what it asked for.
    pub(crate) fn adopt(&mut self, spec: WindowSpec) {
        self.spec = spec;
    }

    /// Whether the loop has closed this window.
    pub(crate) fn is_closed(&self) -> bool {
        self.closed
    }

    /// Hands each event to the caller's handler.
    pub(crate) fn deliver(&mut self, events: &[UiEvent]) {
        if let Some(handler) = self.on_event.as_mut() {
            for event in events {
                handler(event);
            }
        }
    }

    /// The context of the page the window shows.
    fn shown(&mut self) -> &mut Context2D {
        // `set_page` refuses an index past the last and a canvas never loses
        // pages, so the clamp only matters for a spec adopted from the loop.
        let last = self.canvas.page_count().saturating_sub(1);
        let index = (self.spec.page as usize).min(last);
        // SAFETY: `index` is at most the last page's, and a canvas always
        // has at least one page -- `Canvas::new` seeds one and nothing
        // removes one.
        self.canvas
            .page(index)
            .expect("the clamped index names a page")
    }

    /// The page the window currently shows, without running the draw
    /// handler -- what a change made between frames re-shows.
    pub(crate) fn current_page(&mut self) -> Page {
        self.shown().inner.page()
    }

    /// Runs the draw handler and returns what it left behind.
    pub(crate) fn render(&mut self) -> Page {
        // Taken out for the call so the handler can be given the shown page,
        // which is borrowed from the same `self`.
        let mut on_draw = self.on_draw.take();
        if let Some(handler) = on_draw.as_mut() {
            let frame = self.frame;
            handler(self.shown(), frame);
            self.frame += 1;
        }
        self.on_draw = on_draw;

        self.current_page()
    }
}

impl Drop for Window {
    /// Frees the window's id for another to choose, unless closing it
    /// already did -- by now another window may hold that id.
    fn drop(&mut self) {
        if !self.closed {
            held().remove(&self.spec.id);
        }
    }
}

impl std::fmt::Debug for Window {
    // Hand-written because the handlers are boxed closures, which have no
    // `Debug` of their own and nothing useful to say if they did.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Window")
            .field("id", &self.spec.id)
            .field("title", &self.spec.title)
            .field("width", &self.spec.width)
            .field("height", &self.spec.height)
            .field("on_draw", &self.on_draw.is_some())
            .field("on_event", &self.on_event.is_some())
            .finish()
    }
}

#[cfg(test)]
mod window_accessor_tests {
    use super::*;

    #[test]
    fn every_setter_has_a_getter_that_reads_it_back() {
        // The surface was setter-only, so a caller could configure a window
        // and never ask what it had configured. These pairs are what the
        // spec already carried; nothing here is new state.
        // Four pages, so page 3 is one the canvas has: a page past the last
        // is refused, as `page_selection` below pins.
        let mut canvas = Canvas::new(200.0, 100.0);
        for _ in 0..3 {
            canvas.new_page();
        }
        let mut window = Window::with_canvas(canvas);

        window.set_title("a title");
        window.set_background("rgb(1, 2, 3)");
        window.set_resizable(false);
        window.set_fullscreen(true);
        window.set_cursor(Some(Cursor::Crosshair));
        window.set_borderless(true);
        window.set_visible(false);
        window.set_page(3);
        window.set_fit(Fit::Cover);

        assert_eq!(window.title(), "a title");
        assert_eq!(window.background(), "rgb(1, 2, 3)");
        assert!(!window.resizable());
        assert!(window.fullscreen());
        assert_eq!(window.cursor(), "crosshair");
        window.set_cursor_css("pointer");
        assert_eq!(window.cursor(), "pointer");
        window.set_cursor(None);
        assert_eq!(window.cursor(), "none", "None hides it");
        assert!(window.borderless());
        assert!(!window.visible());
        assert_eq!(window.page(), 3);
        assert_eq!(window.fit(), Fit::Cover);
    }

    #[test]
    fn the_geometry_starts_from_the_canvas_and_is_unplaced() {
        // Width and height come from the canvas the window was built
        // around; position is `None` until the window system places it,
        // which is what lets a caller tell "at the origin" from "not yet
        // anywhere".
        let mut window = Window::new(320.0, 240.0);
        assert_eq!((window.width(), window.height()), (320.0, 240.0));
        assert_eq!((window.left(), window.top()), (None, None));

        window.set_position(50.0, 60.0);
        assert_eq!((window.left(), window.top()), (Some(50.0), Some(60.0)));
    }

    #[test]
    fn a_window_keeps_its_id_across_being_opened() {
        // `open` consumes the window, so the id taken before it is the only
        // handle left afterwards -- which is why `close_window` and
        // `window_is_open` take one rather than living on the type.
        let window = Window::new(100.0, 100.0);
        let id = window.id();
        assert!(id > 0, "ids start at one so zero can mean unassigned");

        // Nothing is open before the loop runs, so nothing reports as open.
        assert!(!App::window_is_open(id));
        window.open();
        // Still nothing: `open` only queues, and `App::run` is what takes
        // the queue. The distinction is the reason this is not called
        // `closed` -- never-opened and closed are not the same state.
        assert!(!App::window_is_open(id));

        // And the queue did receive it.
        assert_eq!(take_pending().len(), 1);
    }

    #[test]
    fn the_defaults_are_the_ones_a_window_opens_with() {
        let window = Window::new(100.0, 100.0);
        assert!(window.visible(), "a new window is shown");
        assert!(window.resizable());
        assert!(!window.borderless());
        assert!(!window.fullscreen());
        assert_eq!(window.page(), 0, "the first page");
        assert_eq!(window.background(), crate::gui::window::DEFAULT_BACKGROUND);
    }
}

/// Which page a window shows, measured against the JavaScript `Window` on a
/// live window before this was written: built on a two-page canvas it shows
/// page 2 of 2; `canvas.newPage()` while it runs leaves it on page 2;
/// `window.page = 1` shows page 1 and the canvas reports that page's size;
/// `window.page = 9` on three pages is a `RangeError` and changes nothing.
/// JavaScript counts pages from one and Rust from zero, so each case is the
/// same page one lower.
#[cfg(test)]
mod page_selection {
    use super::*;

    /// A canvas whose pages are told apart by size: 100x50, then 200x80.
    fn two_pages() -> Canvas {
        let mut canvas = Canvas::new(100.0, 50.0);
        canvas.new_page_with(200.0, 80.0);
        canvas
    }

    /// The size of the page the draw handler was given on one frame.
    fn drawn_on(window: &mut Window) -> (f32, f32) {
        let seen = std::rc::Rc::new(std::cell::Cell::new((0.0, 0.0)));
        let into = std::rc::Rc::clone(&seen);
        window.on_draw(move |ctx, _| {
            let size = ctx.inner.bounds.size();
            into.set((size.width, size.height));
        });
        window.render();
        seen.get()
    }

    #[test]
    fn a_window_shows_its_canvas_last_page_when_made() {
        let mut window = Window::with_canvas(two_pages());
        assert_eq!(window.page(), 1, "the second of two");
        assert_eq!(drawn_on(&mut window), (200.0, 80.0));
    }

    #[test]
    fn a_page_added_afterwards_is_not_shown_until_chosen() {
        let mut window = Window::with_canvas(two_pages());
        window.canvas_mut().new_page_with(300.0, 120.0);
        assert_eq!(window.page(), 1, "still the second");
        assert_eq!(drawn_on(&mut window), (200.0, 80.0));
    }

    #[test]
    fn choosing_a_page_shows_it_and_takes_its_size() {
        let mut window = Window::with_canvas(two_pages());
        window.set_page(0);
        assert_eq!(window.page(), 0);
        assert_eq!(
            (window.canvas().width(), window.canvas().height()),
            (100.0, 50.0),
            "the canvas reports the page shown, as in JavaScript"
        );
        assert_eq!(drawn_on(&mut window), (100.0, 50.0));
    }

    #[test]
    fn a_page_past_the_last_changes_nothing() {
        let mut window = Window::with_canvas(two_pages());
        window.set_page(8);
        assert_eq!(window.page(), 1);
        assert_eq!(drawn_on(&mut window), (200.0, 80.0));
    }

    #[test]
    fn a_new_canvas_is_shown_at_its_last_page() {
        let mut window = Window::new(10.0, 10.0);
        window.set_canvas(two_pages());
        assert_eq!(window.page(), 1);
        assert_eq!(drawn_on(&mut window), (200.0, 80.0));
    }
}

#[cfg(test)]
mod chosen_ids {
    use super::*;

    /// Ids far from anything the counter reaches in this test binary, a
    /// range per test: the held set and the counter are process-wide and the
    /// tests run in parallel.
    fn id(n: u32) -> NonZeroU32 {
        NonZeroU32::new(3_000_000_000 + n).expect("not zero")
    }

    #[test]
    fn a_free_id_is_taken_and_reported() {
        let mut window = Window::new(10.0, 10.0);
        assert_eq!(window.set_id(id(1)), Ok(()));
        assert_eq!(window.id(), id(1).get());
        assert_eq!(window.handle().id(), id(1).get());
        assert_eq!(window.set_id(id(1)), Ok(()), "its own id is free to it");
    }

    #[test]
    fn an_id_another_window_holds_is_refused_by_name() {
        let mut holder = Window::new(10.0, 10.0);
        holder.set_id(id(10)).expect("free");
        let mut other = Window::new(10.0, 10.0);
        let before = other.id();
        let taken = other.set_id(id(10));
        assert_eq!(taken, Err(WindowIdTaken { id: id(10).get() }));
        assert_eq!(other.id(), before, "keeps the id it had");
        let message = taken.expect_err("refused").to_string();
        assert!(message.contains(&id(10).get().to_string()), "{message}");
    }

    #[test]
    fn a_counted_id_is_held_too() {
        // Two windows built and given one id before either opens would both
        // pass a check of the open ones alone.
        let counted = Window::new(10.0, 10.0);
        let mut other = Window::new(10.0, 10.0);
        let taken = NonZeroU32::new(counted.id()).expect("counted from 1");
        assert!(other.set_id(taken).is_err());
    }

    #[test]
    fn a_dropped_window_frees_its_id() {
        let mut first = Window::new(10.0, 10.0);
        first.set_id(id(20)).expect("free");
        drop(first);
        let mut second = Window::new(10.0, 10.0);
        assert_eq!(second.set_id(id(20)), Ok(()));
    }

    #[test]
    fn changing_an_id_frees_the_old_one() {
        let mut window = Window::new(10.0, 10.0);
        window.set_id(id(30)).expect("free");
        window.set_id(id(31)).expect("free");
        let mut other = Window::new(10.0, 10.0);
        assert_eq!(other.set_id(id(30)), Ok(()));
    }

    #[test]
    fn a_queued_window_holds_its_id() {
        let mut queued = Window::new(10.0, 10.0);
        queued.set_id(id(40)).expect("free");
        queued.open();
        let mut other = Window::new(10.0, 10.0);
        assert!(other.set_id(id(40)).is_err());
        // Nothing runs the loop here, so take it back off the queue and
        // close it, which forgets it as the loop would.
        drop(take_pending());
        handle::forget(id(40).get());
        assert_eq!(other.set_id(id(40)), Ok(()));
    }

    #[test]
    fn a_closed_window_frees_its_id_and_dropping_it_frees_nothing_else() {
        let mut closing = vec![Window::new(10.0, 10.0)];
        closing[0].set_id(id(60)).expect("free");

        // Not yet in the manager: not open yet, rather than closed.
        close_on(&mut closing, &[]);
        assert!(!closing[0].is_closed());
        let mut other = Window::new(10.0, 10.0);
        assert!(other.set_id(id(60)).is_err(), "still held");

        // Shown, then gone: closed, and its id free.
        close_on(&mut closing, &[id(60).get()]);
        close_on(&mut closing, &[]);
        assert!(closing[0].is_closed());
        assert_eq!(other.set_id(id(60)), Ok(()));

        // Dropping the closed one leaves the new claim alone.
        drop(closing);
        let mut third = Window::new(10.0, 10.0);
        assert!(third.set_id(id(60)).is_err(), "the new window holds it");
    }

    #[test]
    fn the_counter_moves_past_a_chosen_id() {
        let mut window = Window::new(10.0, 10.0);
        window.set_id(id(50)).expect("free");
        let later = Window::new(10.0, 10.0);
        assert!(later.id() > id(50).get(), "{} after {}", later.id(), id(50));
    }
}
