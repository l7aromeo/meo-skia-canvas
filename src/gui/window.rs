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
use crate::{
    context::page::Page, context2d::affine_to_matrix, geometry::Affine,
    gpu::Renderer, utils::css_to_color,
};

/// How far in from the window's left edge the IME candidate area sits.
const IME_INSET: i32 = 15;

/// How far up from the window's bottom edge the same area sits.
const IME_BOTTOM_OFFSET: i32 = 20;

/// The IME candidate area's size, about one line of text.
const IME_WIDTH: i32 = 100;
const IME_HEIGHT: i32 = 15;

/// What a window paints behind the canvas when the caller names no
/// background, or names one that does not parse.
///
/// Near-black at 85% opacity, so a canvas with transparency shows the
/// desktop faintly through it rather than sitting on a hard black plate.
/// Stated once: this was written out at both sites that need it -- the
/// default a session starts from and the fallback a window falls back to --
/// which is two places for one decision to be changed in, and only one of
/// them would have been found by searching for the other.
pub const DEFAULT_BACKGROUND: &str = "rgba(16,16,16,0.85)";

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

/// The cursors a window can show, by their CSS `cursor` keywords.
///
/// The typed form of
/// [`Window::set_cursor`](super::session::Window::set_cursor);
/// [`set_cursor_css`](super::session::Window::set_cursor_css) takes the
/// keyword as a string for a caller porting one from CSS or from the
/// JavaScript binding.
///
/// One CSS keyword is missing on purpose. `none` hides the cursor rather than
/// naming one, so it is [`None`] in the `Option` those setters take, and
/// hiding is a thing the platform does separately from choosing an icon.
///
/// This began as a serde shim mirroring winit's `CursorIcon` -- the doc said
/// it existed only to derive `Serialize`/`Deserialize` for a foreign type,
/// and nothing ever used the derive it generated. Thirty-four public
/// variants that no signature mentioned.
#[non_exhaustive]
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
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

impl Cursor {
    /// Every variant, for a caller enumerating them and for the test below.
    pub const ALL: &'static [Self] = &[
        Self::Alias,
        Self::AllScroll,
        Self::Cell,
        Self::ColResize,
        Self::ContextMenu,
        Self::Copy,
        Self::Crosshair,
        Self::Default,
        Self::EResize,
        Self::EwResize,
        Self::Grab,
        Self::Grabbing,
        Self::Help,
        Self::Move,
        Self::NeResize,
        Self::NeswResize,
        Self::NoDrop,
        Self::NotAllowed,
        Self::NResize,
        Self::NsResize,
        Self::NwResize,
        Self::NwseResize,
        Self::Pointer,
        Self::Progress,
        Self::RowResize,
        Self::SeResize,
        Self::SResize,
        Self::SwResize,
        Self::Text,
        Self::VerticalText,
        Self::Wait,
        Self::WResize,
        Self::ZoomIn,
        Self::ZoomOut,
    ];

    /// The CSS keyword for this cursor.
    ///
    /// The same spelling winit's `CursorIcon::from_str` reads back, which is
    /// what [`Window::set_cursor`](super::session::Window::set_cursor)
    /// relies on: it stores the keyword, and the window resolves it when the
    /// event loop opens. `every_cursor_keyword_round_trips` holds the two
    /// halves together.
    pub fn as_css(self) -> &'static str {
        match self {
            Self::Alias => "alias",
            Self::AllScroll => "all-scroll",
            Self::Cell => "cell",
            Self::ColResize => "col-resize",
            Self::ContextMenu => "context-menu",
            Self::Copy => "copy",
            Self::Crosshair => "crosshair",
            Self::Default => "default",
            Self::EResize => "e-resize",
            Self::EwResize => "ew-resize",
            Self::Grab => "grab",
            Self::Grabbing => "grabbing",
            Self::Help => "help",
            Self::Move => "move",
            Self::NeResize => "ne-resize",
            Self::NeswResize => "nesw-resize",
            Self::NoDrop => "no-drop",
            Self::NotAllowed => "not-allowed",
            Self::NResize => "n-resize",
            Self::NsResize => "ns-resize",
            Self::NwResize => "nw-resize",
            Self::NwseResize => "nwse-resize",
            Self::Pointer => "pointer",
            Self::Progress => "progress",
            Self::RowResize => "row-resize",
            Self::SeResize => "se-resize",
            Self::SResize => "s-resize",
            Self::SwResize => "sw-resize",
            Self::Text => "text",
            Self::VerticalText => "vertical-text",
            Self::Wait => "wait",
            Self::WResize => "w-resize",
            Self::ZoomIn => "zoom-in",
            Self::ZoomOut => "zoom-out",
        }
    }
}

// timeout for triggering a full vector re-render after the last resize event
static RESIZE_CLEANUP_INTERVAL: Duration = Duration::from_millis(100);

/// One open window: its winit handle, its spec, and the renderer drawing
/// into it.
///
/// Internal. This is the live, winit-backed window, which only exists once
/// the event loop is running -- winit creates windows inside `resumed`, not
/// before. The [`Window`](super::session::Window) a caller configures up
/// front is a different thing, and holds the name.
pub(crate) struct OpenWindow {
    /// The underlying winit window.
    ///
    /// Internal: winit is not part of this crate's public surface, so handing
    /// its window out would make every winit release a breaking one here.
    pub(crate) handle: Arc<WinitWindow>,
    /// The spec this window was created from, kept current as it changes.
    pub spec: WindowSpec,
    /// Accumulates window events until the event loop drains them.
    pub(crate) sieve: Sieve,
    renderer: Renderer,
    background: Color,
    page: Page,
    suspended: bool,
    resized_at: Option<Instant>,
}

impl OpenWindow {
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
                spec.background = DEFAULT_BACKGROUND.to_string();
                // SAFETY: `DEFAULT_BACKGROUND` is a literal this crate
                // writes and a test parses, so it cannot fail here.
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
        if let Some(fit) = self.fitting_matrix_skia().invert() {
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
        // Where the platform puts its candidate list while composing. A
        // canvas has no text cursor to anchor it to -- there are no editable
        // regions, only pixels -- so it goes bottom-left, out of the way of
        // most drawings, at about the size of one line of text.
        self.handle.set_ime_cursor_area(
            LogicalPosition::new(IME_INSET, window_height - IME_BOTTOM_OFFSET),
            LogicalSize::new(IME_WIDTH, IME_HEIGHT),
        );
    }

    /// Returns the transform mapping canvas coordinates onto the window, as
    /// determined by [`WindowSpec::fit`].
    ///
    /// This is the transform already applied to the `point` field of a mouse
    /// [`UiEvent`](super::event::UiEvent), so hit-testing against drawn
    /// content needs no further conversion. It is here for the cases that do
    /// -- projecting a rectangle, or sizing something to the window.
    pub fn fitting_matrix(&self) -> Affine {
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

        Affine {
            a: x_scale,
            d: y_scale,
            tx: x_shift,
            ty: y_shift,
            ..Affine::IDENTITY
        }
    }

    /// The same transform as Skia's matrix.
    ///
    /// The renderer and the pointer-coordinate inverse both need one, and
    /// `Affine` carries no `invert`. Kept beside the public form rather than
    /// converted at each call site, so the two cannot drift.
    pub(crate) fn fitting_matrix_skia(&self) -> Matrix {
        affine_to_matrix(self.fitting_matrix())
    }

    /// Returns the Skia surface properties for this window, carrying the
    /// text contrast and gamma from the spec. Subpixel geometry is left
    /// unspecified.
    pub(crate) fn surface_props(&self) -> SurfaceProps {
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
                // Borrowed, not cloned. `draw` only ever read through it --
                // the bounds, the layer list, and twice as `&page` into the
                // render cache -- so the clone bought nothing and cost two
                // vector allocations plus a refcount bump per `Picture` on
                // every frame the window drew.
                &self.page,
                self.fitting_matrix_skia(),
                self.surface_props(),
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

    /// Sets the color drawn behind the canvas, as a CSS color string.
    ///
    /// Returns `false` when `color` does not parse, leaving the background
    /// as it was. [`WindowSpec::background`] is a string for the same reason
    /// -- this is the one place a window takes a color, and taking it in the
    /// form the spec already carries means no caller has to reach for a
    /// parser to change it.
    pub fn set_background(&mut self, color: &str) -> bool {
        let Some(parsed) = css_to_color(color) else {
            return false;
        };

        // Outside the redraw check on purpose. Two strings can parse to one
        // colour -- "red" and "#ff0000" -- and only the second half of that
        // pair needs no repaint. Recording it regardless keeps `spec` the
        // string the caller last set rather than the last one that happened
        // to change the pixels.
        self.spec.background = color.to_string();

        if self.background != parsed {
            self.background = parsed;
            self.handle.request_redraw();
        }
        true
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, to_value};

    /// Every keyword `Cursor::as_css` hands out must be one winit reads back.
    ///
    /// The two spellings live apart -- ours in a match, winit's in its
    /// `FromStr` -- and a window resolves the second from the first when it
    /// opens. A typo here would not fail to compile; it would hide the
    /// cursor, because an unparseable name is what hiding looks like.
    #[test]
    fn every_cursor_keyword_round_trips() {
        for cursor in Cursor::ALL {
            let keyword = cursor.as_css();
            assert!(
                CursorIcon::from_str(keyword).is_ok(),
                "{cursor:?} spells itself {keyword:?}, which winit rejects"
            );
        }
    }

    /// And no two variants claim the same keyword, which would make one of
    /// them unreachable through the string form.
    #[test]
    fn no_two_cursors_share_a_keyword() {
        let mut seen = std::collections::HashSet::new();
        for cursor in Cursor::ALL {
            assert!(
                seen.insert(cursor.as_css()),
                "{:?} is spelled the same as an earlier variant",
                cursor
            );
        }
        assert_eq!(seen.len(), Cursor::ALL.len());
    }

    /// `none` is not a cursor, it is the absence of one, so it is `None`
    /// rather than a variant -- and it has to reach the window as the
    /// keyword that hides it.
    #[test]
    fn hiding_the_cursor_is_not_a_variant() {
        assert!(Cursor::ALL.iter().all(|c| c.as_css() != "none"));
        assert!(CursorIcon::from_str("none").is_err());
    }

    fn spec() -> WindowSpec {
        WindowSpec {
            id: 1,
            left: None,
            top: None,
            title: "t".to_string(),
            visible: true,
            resizable: true,
            borderless: false,
            fullscreen: false,
            background: "white".to_string(),
            page: 0,
            width: 512.0,
            height: 512.0,
            cursor: "default".to_string(),
            fit: Fit::Contain,
            text_contrast: 0.0,
            text_gamma: 1.4,
        }
    }

    // `WindowSpec` crosses the bridge in both directions -- serialized into
    // the `state` envelope `lib/classes/gui.js` spreads onto the window with
    // `Object.assign`, and deserialized back from the JSON the JS side sends
    // when a property changes. Nothing pinned either direction.
    //
    // The two renamed fields are the ones worth naming explicitly: gui.js
    // reads `textContrast` and `textGamma`, which only match because of the
    // `rename_all = "camelCase"` on the struct.
    #[test]
    fn the_spec_reaches_javascript_in_the_names_it_reads() {
        let json = to_value(spec()).unwrap();

        assert_eq!(json["textContrast"], json!(0.0));
        // `1.4_f32` and not `1.4`: the field is f32 and JSON numbers are f64,
        // so the default reaches JavaScript as 1.399999976158142. Harmless --
        // it is fed back to Skia as an f32 -- but the literal has to be
        // written as the same width or the comparison is against a number
        // this never produces.
        assert_eq!(json["textGamma"], json!(1.4_f32));
        // Absent rather than omitted: gui.js's setters test for null.
        assert_eq!(json["left"], json!(null));
        assert_eq!(json["width"], json!(512.0));
    }

    #[test]
    fn the_spec_survives_the_round_trip_javascript_puts_it_through() {
        let there = serde_json::to_string(&spec()).unwrap();
        let back: WindowSpec = serde_json::from_str(&there).unwrap();

        assert_eq!(back.text_contrast, 0.0);
        assert_eq!(back.text_gamma, 1.4);
        assert_eq!(back.fit, Fit::Contain);
        assert!(back.left.is_none());
    }

    // These eight strings are duplicated in `parseFit` in
    // lib/classes/css.js, which validates the mode before it is sent. The
    // two lists have to stay set-equal: a spelling that fails there never
    // arrives, and one that fails here is a deserialize error at the bridge.
    #[test]
    fn every_fit_mode_spells_itself_the_way_the_css_parser_expects() {
        let modes = [
            (Fit::None, "none"),
            (Fit::ContainX, "contain-x"),
            (Fit::ContainY, "contain-y"),
            (Fit::Contain, "contain"),
            (Fit::Cover, "cover"),
            (Fit::Fill, "fill"),
            (Fit::ScaleDown, "scale-down"),
            (Fit::Resize, "resize"),
        ];

        for (mode, name) in modes {
            assert_eq!(to_value(mode).unwrap(), json!(name));
            assert_eq!(
                serde_json::from_value::<Fit>(json!(name)).unwrap(),
                mode,
                "{name} must survive the trip back"
            );
        }
    }
}
