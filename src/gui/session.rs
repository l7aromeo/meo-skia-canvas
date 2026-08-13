//! Opening a window from Rust.
//!
//! The JavaScript side has had this since the fork's ancestor: construct a
//! `Window`, attach handlers, and the runtime drives it. This is the same
//! thing for a Rust caller, over the same engine -- one window type, one set
//! of handlers, one loop.
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
//! App::run();
//! ```

use std::cell::{Cell, RefCell};

use super::{
    app::App,
    event::UiEvent,
    window::{Fit, WindowSpec},
};
use crate::{canvas::Canvas, context::page::Page, context2d::Context2D};

thread_local!(
    /// Windows handed over by [`Window::open`] and not yet opened.
    ///
    /// They wait here rather than going straight to [`App::open_window`]
    /// because a caller's handlers have to travel with the spec, and the
    /// queue that carries specs into the loop is shared with the Node
    /// binding, which has no handlers to carry.
    static PENDING: RefCell<Vec<Window>> = const { RefCell::new(Vec::new()) };

    /// Source of the ids that address windows for the rest of their lives.
    static NEXT_ID: Cell<u32> = const { Cell::new(1) };
);

/// Takes everything [`Window::open`] has queued.
pub(crate) fn take_pending() -> Vec<Window> {
    PENDING.with_borrow_mut(std::mem::take)
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
        let id = NEXT_ID.with(|next| {
            let id = next.get();
            next.set(id + 1);
            id
        });

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
                background: "rgba(16,16,16,0.85)".to_string(),
                page: 0,
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
        }
    }

    /// The id addressing this window, for [`App::close_window`].
    pub fn id(&self) -> u32 {
        self.spec.id
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

    /// Places the top-left corner, in logical pixels.
    ///
    /// Left unset, windows cascade from the last one opened.
    pub fn set_position(&mut self, left: f32, top: f32) {
        self.spec.left = Some(left);
        self.spec.top = Some(top);
    }

    /// Sets the handler drawing each frame.
    ///
    /// Called with this window's context and the frame number, counting from
    /// zero. Whatever it leaves on the canvas is what the window shows.
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

    /// Queues this window to open when the loop starts.
    ///
    /// Nothing appears until [`App::run`](super::app::App::run) is called.
    pub fn open(self) {
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

    /// Hands each event to the caller's handler.
    pub(crate) fn deliver(&mut self, events: &[UiEvent]) {
        if let Some(handler) = self.on_event.as_mut() {
            for event in events {
                handler(event);
            }
        }
    }

    /// Runs the draw handler and returns what it left behind.
    pub(crate) fn render(&mut self) -> Page {
        let Self {
            canvas,
            on_draw,
            frame,
            ..
        } = self;

        if let Some(handler) = on_draw.as_mut() {
            handler(canvas.context(), *frame);
            *frame += 1;
        }

        canvas.context().inner.get_page()
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
