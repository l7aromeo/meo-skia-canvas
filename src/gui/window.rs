use serde::{Deserialize, Serialize};
use skia_safe::{
    Color, Matrix, PixelGeometry, SurfaceProps, SurfacePropsFlags,
};
use std::{
    str::FromStr,
    sync::Arc,
    time::{Duration, Instant},
};
use winit::{
    dpi::{LogicalPosition, LogicalSize, PhysicalPosition, PhysicalSize},
    event_loop::ActiveEventLoop,
    window::{CursorIcon, Fullscreen, Window as WinitWindow, WindowId},
};

use super::event::Sieve;
use crate::{context::page::Page, gpu::Renderer, utils::css_to_color};

/// Everything that describes a window, serialized to and from the JS side.
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct WindowSpec {
    /// Caller-assigned identifier, used to address this window later.
    pub id: u32,
    /// Left edge in logical pixels, or `None` to cascade from the last window.
    pub left: Option<f32>,
    /// Top edge in logical pixels, or `None` to cascade from the last window.
    pub top: Option<f32>,
    /// Title-bar text.
    pub title: String,
    /// Whether the window is shown.
    pub visible: bool,
    /// Whether the user can resize the window.
    pub resizable: bool,
    /// Whether the title bar and frame are hidden.
    pub borderless: bool,
    /// Whether the window occupies the whole screen.
    pub fullscreen: bool,
    /// Background as a CSS color string; invalid values fall back to a dark
    /// translucent grey.
    pub background: String,
    /// Index of the page being displayed.
    pub page: u32,
    /// Canvas width in logical pixels.
    pub width: f32,
    /// Canvas height in logical pixels.
    pub height: f32,
    /// Cursor name, matching the CSS `cursor` keywords.
    pub cursor: String,
    /// How the canvas is scaled when the window's aspect ratio differs.
    pub fit: Fit,
    /// Text contrast enhancement, `0.0` to `1.0`.
    pub text_contrast: f32,
    /// Gamma correction applied to text rendering.
    pub text_gamma: f32,
}

/// How the canvas is scaled when its aspect ratio differs from the window's.
///
/// Mirrors the CSS `object-fit` keywords, plus `Resize`, which changes the
/// canvas rather than scaling it.
#[derive(Copy, Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Fit {
    /// Draws at native size, centred, with no scaling.
    None,
    /// Scales to fit the width, allowing vertical overflow.
    ContainX,
    /// Scales to fit the height, allowing horizontal overflow.
    ContainY,
    /// Scales until the whole canvas fits, letterboxing the remainder.
    Contain,
    /// Scales until the canvas covers the window, cropping the overflow.
    Cover,
    /// Stretches to the window, ignoring the aspect ratio.
    Fill,
    /// Like `Contain`, but never enlarges beyond native size.
    ScaleDown,
    /// Resizes the canvas itself to match the window.
    Resize,
}

/// Serde shim mirroring winit's `CursorIcon` so a cursor can be named by its
/// CSS keyword.
///
/// Exists only to derive `Serialize`/`Deserialize` for a foreign type; the
/// variants carry the meanings the CSS `cursor` property gives them.
#[non_exhaustive]
#[derive(Copy, Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", remote = "CursorIcon")]
#[allow(missing_docs)]
pub enum Cursor {
    Alias,
    AllScroll,
    Cell,
    ColResize,
    ContextMenu,
    Copy,
    Crosshair,
    Default,
    EResize,
    EwResize,
    Grab,
    Grabbing,
    Help,
    Move,
    NeResize,
    NeswResize,
    NoDrop,
    NotAllowed,
    NResize,
    NsResize,
    NwResize,
    NwseResize,
    Pointer,
    Progress,
    RowResize,
    SeResize,
    SResize,
    SwResize,
    Text,
    VerticalText,
    Wait,
    WResize,
    ZoomIn,
    ZoomOut,
}

// timeout for triggering a full vector re-render after the last resize event
static RESIZE_CLEANUP_INTERVAL: Duration = Duration::from_millis(100);

/// One open window: its winit handle, its spec, and the renderer drawing
/// into it.
pub struct Window {
    /// The underlying winit window.
    pub handle: Arc<WinitWindow>,
    /// The spec this window was created from, kept current as it changes.
    pub spec: WindowSpec,
    /// Accumulates window events until the event loop drains them.
    pub sieve: Sieve,
    renderer: Renderer,
    background: Color,
    page: Page,
    suspended: bool,
    resized_at: Option<Instant>,
}

impl Window {
    /// Creates a window from `spec`, showing `page`.
    ///
    /// # Panics
    ///
    /// Panics if the window system refuses to create the window.
    pub fn new(
        event_loop: &ActiveEventLoop,
        mut spec: WindowSpec,
        page: &Page,
    ) -> Self {
        let size: LogicalSize<i32> =
            LogicalSize::new(spec.width as i32, spec.height as i32);
        let background = match css_to_color(&spec.background) {
            Some(color) => color,
            None => {
                spec.background = "rgba(16,16,16,0.85)".to_string();
                // SAFETY: Hardcoded color string "rgba(16,16,16,0.85)" always
                // parses.
                css_to_color(&spec.background).unwrap()
            }
        };

        let window_attributes = WinitWindow::default_attributes()
            .with_fullscreen(if spec.fullscreen {
                Some(Fullscreen::Borderless(None))
            } else {
                None
            })
            .with_inner_size(size)
            .with_transparent(background.a() < 255)
            .with_title(spec.title.clone())
            .with_visible(false)
            .with_resizable(spec.resizable)
            .with_decorations(!spec.borderless);

        let handle = Arc::new(
            event_loop
                .create_window(window_attributes)
                // SAFETY: Window creation only fails if the event loop is
                // invalid.
                .expect("Failed to create window"),
        );
        let renderer = Renderer::for_window(event_loop, handle.clone());
        let sieve = Sieve::new(handle.scale_factor());

        let cursor_icon = CursorIcon::from_str(&spec.cursor).ok();
        handle.set_cursor(cursor_icon.unwrap_or_default());
        handle.set_cursor_visible(cursor_icon.is_some());

        if let (Some(left), Some(top)) = (spec.left, spec.top) {
            handle.set_outer_position(LogicalPosition::new(left, top));
        }

        Self {
            spec,
            handle,
            sieve,
            renderer,
            page: page.clone(),
            suspended: false,
            resized_at: None,
            background,
        }
    }

    /// Returns the winit id identifying this window to the event loop.
    pub fn id(&self) -> WindowId {
        self.handle.id()
    }

    /// Handles a resize from the window system, re-fitting the canvas.
    pub fn resize(&mut self, size: PhysicalSize<u32>) {
        self.resized_at = Some(Instant::now());
        self.renderer.resize(size);
        self.reposition_ime(size);
        self.update_fit();

        let LogicalSize { width, height } = self
            .handle
            .inner_size()
            .to_logical::<f32>(self.handle.scale_factor());
        let is_fullscreen = self.handle.fullscreen().is_some()
            && width >= self.spec.width
            && height >= self.spec.height;

        self.spec = WindowSpec {
            width,
            height,
            ..self.spec.clone()
        };
        if self.spec.fullscreen != is_fullscreen {
            self.sieve.go_fullscreen(is_fullscreen);
            self.spec.fullscreen = is_fullscreen;
        }

        #[cfg(feature = "vulkan")]
        self.handle.request_redraw();
    }

    /// Handles a move from the window system.
    pub fn reposition(&mut self, loc: LogicalPosition<i32>) {
        self.spec.left = Some(loc.x as _);
        self.spec.top = Some(loc.y as _);
    }

    /// Recomputes the canvas-to-window transform after a size or fit change.
    pub fn update_fit(&mut self) {
        if let Some(fit) = self.fitting_matrix().invert() {
            self.sieve.use_transform(fit);
        }
    }

    /// Pins the input-method candidate window to the bottom-left of the
    /// window, so it does not cover the drawn content. It does not track a
    /// caret.
    pub fn reposition_ime(&mut self, size: PhysicalSize<u32>) {
        // place the input region in the bottom left corner so the UI doesn't
        // cover the window
        let dpr = self.handle.scale_factor();
        let window_height = size.to_logical::<i32>(dpr).height;
        self.handle.set_ime_allowed(true);
        self.handle.set_ime_cursor_area(
            LogicalPosition::new(15, window_height - 20),
            LogicalSize::new(100, 15),
        );
    }

    /// Returns the transform mapping canvas coordinates onto the window, as
    /// determined by [`WindowSpec::fit`].
    pub fn fitting_matrix(&self) -> Matrix {
        let dpr = self.handle.scale_factor();
        let size = self.handle.inner_size().to_logical::<f32>(dpr);
        let dims = self.page.bounds.size();
        let fit_x = size.width / dims.width;
        let fit_y = size.height / dims.height;

        let sf = match self.spec.fit {
            Fit::Cover => fit_x.max(fit_y),
            Fit::ScaleDown => fit_x.min(fit_y).min(1.0),
            Fit::Contain => fit_x.min(fit_y),
            Fit::ContainX => fit_x,
            Fit::ContainY => fit_y,
            _ => 1.0,
        };

        let (x_scale, y_scale) = match self.spec.fit {
            Fit::Fill => (fit_x, fit_y),
            _ => (sf, sf),
        };

        let (x_shift, y_shift) = match self.spec.fit {
            Fit::Resize => (0.0, 0.0),
            _ => (
                (size.width - dims.width * x_scale) / 2.0,
                (size.height - dims.height * y_scale) / 2.0,
            ),
        };

        let mut matrix = Matrix::new_identity();
        matrix.set_scale_translate((x_scale, y_scale), (x_shift, y_shift));
        matrix
    }

    /// Returns the Skia surface properties for this window, carrying the
    /// text contrast and gamma from the spec. Subpixel geometry is left
    /// unspecified.
    pub fn suface_props(&self) -> SurfaceProps {
        SurfaceProps::new_with_text_properties(
            SurfacePropsFlags::default(),
            PixelGeometry::Unknown,
            self.spec.text_contrast,
            self.spec.text_gamma,
        )
    }

    /// Renders the current page into the window.
    pub fn redraw(&mut self) {
        if !self.suspended {
            self.renderer.draw(
                self.page.clone(),
                self.fitting_matrix(),
                self.suface_props(),
                self.background,
            );
        }
    }

    /// Replaces the page being displayed and redraws.
    pub fn set_page(&mut self, page: Page) {
        if self.page != page {
            self.handle.request_redraw();
        }
        self.page = page;
    }

    /// Shows or hides the window.
    pub fn set_visible(&mut self, flag: bool) {
        self.handle.set_visible(flag);
    }

    /// Allows or forbids user resizing.
    pub fn set_resizable(&mut self, flag: bool) {
        self.handle.set_resizable(flag);
    }

    /// Shows or hides the title bar and frame.
    pub fn set_borderless(&mut self, flag: bool) {
        self.handle.set_decorations(!flag);
    }

    /// Sets the title-bar text.
    pub fn set_title(&mut self, title: &str) {
        self.handle.set_title(title);
    }

    /// Sets the cursor by CSS keyword.
    ///
    /// An unrecognised name resets the icon to the default *and hides the
    /// cursor*, which is how `cursor: none` is expressed.
    pub fn set_cursor(&mut self, icon: &str) {
        let cursor_icon = CursorIcon::from_str(icon).ok();
        self.handle.set_cursor(cursor_icon.unwrap_or_default());
        self.handle.set_cursor_visible(cursor_icon.is_some());
    }

    /// Sets how the canvas scales to the window.
    pub fn set_fit(&mut self, mode: Fit) {
        self.spec.fit = mode;
    }

    /// Sets the color drawn behind the canvas.
    pub fn set_background(&mut self, color: Color) {
        if self.background != color {
            self.background = color;
            self.handle.request_redraw();
        }
    }

    /// Resizes the window.
    pub fn set_size(&mut self, size: LogicalSize<u32>) {
        let size: PhysicalSize<u32> =
            size.to_physical(self.handle.scale_factor());
        if let Some(to_size) = self.handle.request_inner_size(size) {
            self.resize(to_size);
        }
    }

    /// Moves the window.
    pub fn set_position(&mut self, loc: LogicalPosition<i32>) {
        self.handle.set_outer_position(loc);
        self.reposition(loc);
    }

    /// Enters or leaves fullscreen.
    pub fn set_fullscreen(&mut self, to_fullscreen: bool) {
        match to_fullscreen {
            true => self
                .handle
                .set_fullscreen(Some(Fullscreen::Borderless(None))),
            false => self.handle.set_fullscreen(None),
        }
    }

    /// Records a move reported by the window system.
    pub fn did_move(&mut self, size: PhysicalPosition<i32>) {
        self.reposition(size.to_logical(self.handle.scale_factor()));
    }

    /// Records a resize reported by the window system.
    pub fn did_resize(&mut self, size: PhysicalSize<u32>) {
        self.resize(size);
    }

    /// Re-renders from vector sources once resizing has settled.
    pub fn redraw_if_resized(&mut self) {
        if let Some(resize) = self.resized_at
            && resize.elapsed() > RESIZE_CLEANUP_INTERVAL
        {
            self.resized_at = None;
            self.handle.request_redraw();
        }
    }

    /// Suspends or resumes redrawing, used while the window is occluded or
    /// minimised.
    pub fn set_redrawing_suspended(&mut self, suspended: bool) {
        self.suspended = suspended;
        if !suspended {
            self.handle.request_redraw();
        }
    }
}
