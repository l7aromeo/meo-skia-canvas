use serde_json::{Map, Value, json};
use winit::{
    dpi::{LogicalPosition, LogicalSize},
    event::WindowEvent,
    event_loop::ActiveEventLoop,
    window::WindowId,
};

use super::window::{Window, WindowSpec};
use crate::context::page::Page;

/// The set of open windows, and where to place the next one.
///
/// New windows cascade from the last one opened rather than stacking exactly
/// on top of each other.
#[derive(Default)]
pub struct WindowManager {
    windows: Vec<Window>,
    last: Option<LogicalPosition<f32>>,
}

impl WindowManager {
    /// Opens a window for `spec`, rendering `page`, positioned by cascade
    /// unless the spec pins a location.
    pub fn add(
        &mut self,
        event_loop: &ActiveEventLoop,
        spec: WindowSpec,
        page: Page,
    ) {
        let mut window = Window::new(event_loop, spec, &page);

        // make sure mouse events use canvas-relative coordinates (in case win
        // size doesn't match)
        window.update_fit();

        // cascade the windows based on the position of the most recently opened
        let dpr = window.handle.scale_factor();
        if let Ok(auto_loc) = window
            .handle
            .outer_position()
            .map(|pt| pt.to_logical::<f32>(dpr))
            && let Ok(inset) = window
                .handle
                .inner_position()
                .map(|pt| pt.to_logical::<f32>(dpr))
        {
            let delta = inset.y - auto_loc.y;
            let reference = self.last.unwrap_or(auto_loc);
            let (left, top) = (
                window.spec.left.unwrap_or(reference.x),
                window.spec.top.unwrap_or(reference.y),
            );

            window
                .handle
                .set_outer_position(LogicalPosition::new(left, top));
            window.handle.set_visible(true);

            window.spec.left = Some(left);
            window.spec.top = Some(top);
            self.last = Some(LogicalPosition::new(left + delta, top + delta));
        }

        self.windows.push(window);
    }

    /// Drops the window with this winit id, closing it.
    pub fn remove(&mut self, window_id: &WindowId) {
        self.windows.retain(|win| win.id() != *window_id);
    }

    /// Drops the window with this caller-assigned id, closing it.
    pub fn remove_by_token(&mut self, token: u32) {
        self.windows.retain(|win| win.spec.id != token);
    }

    /// Closes every window.
    pub fn remove_all(&mut self) {
        self.windows.clear();
    }

    /// Applies a new spec to the window it names, resizing and re-rendering
    /// only where the spec actually changed.
    pub fn update_window(&mut self, mut spec: WindowSpec, page: Page) {
        if let Some(win) =
            self.windows.iter_mut().find(|win| win.spec.id == spec.id)
        {
            if spec.width != win.spec.width || spec.height != win.spec.height {
                win.set_size(LogicalSize::new(
                    spec.width as u32,
                    spec.height as u32,
                ));
            }

            if let (Some(left), Some(top)) = (spec.left, spec.top)
                && (spec.left != win.spec.left || spec.top != win.spec.top)
            {
                win.set_position(LogicalPosition::new(left as i32, top as i32));
            }

            if spec.title != win.spec.title {
                win.set_title(&spec.title);
            }

            if spec.visible != win.spec.visible {
                win.set_visible(spec.visible);
            }

            if spec.fullscreen != win.spec.fullscreen {
                win.set_fullscreen(spec.fullscreen);
                win.sieve.go_fullscreen(spec.fullscreen);
            }

            if spec.resizable != win.spec.resizable {
                win.set_resizable(spec.resizable);
            }

            if spec.borderless != win.spec.borderless {
                win.set_borderless(spec.borderless);
            }

            if spec.cursor != win.spec.cursor {
                win.set_cursor(&spec.cursor);
            }

            if spec.fit != win.spec.fit {
                win.set_fit(spec.fit);
            }

            if spec.background != win.spec.background
                && !win.set_background(&spec.background)
            {
                // Unparseable: echo the current value back so the JS side's
                // spec does not keep a color the window never adopted.
                spec.background = win.spec.background.clone();
            }

            win.set_page(page);

            win.spec = spec;
        }
    }

    /// Runs `f` against the window with this winit id, if it is still open.
    pub fn find<F>(&mut self, id: &WindowId, f: F)
    where
        F: FnMut(&mut Window),
    {
        self.windows.iter_mut().find(|win| win.id() == *id).map(f);
    }

    /// Returns `true` when any window has events waiting to be delivered.
    pub fn has_ui_changes(&self) -> bool {
        self.windows.iter().any(|win| !win.sieve.is_empty())
    }

    /// Drains pending events from every window and returns them as JSON,
    /// alongside each window's current state.
    ///
    /// Also re-renders any window still showing the bitmap cache it used
    /// during a resize.
    pub fn get_ui_changes(&mut self) -> Value {
        let mut ui = Map::new();
        let mut state = Map::new();
        self.windows.iter_mut().for_each(|win| {
            // collect new UI events
            if !win.sieve.is_empty() {
                ui.insert(win.spec.id.to_string(), win.sieve.collect());
            }
            state.insert(win.spec.id.to_string(), json!(win.spec));

            // rerender frame from vector sources after using bitmap cache
            // during resize
            win.redraw_if_resized();
        });
        json!({ "ui": ui, "state": state })
    }

    /// Returns each window's on-screen position as JSON.
    pub fn get_geometry(&mut self) -> Value {
        let mut positions = Map::new();
        self.windows.iter_mut().for_each(|win| {
            positions.insert(
                win.spec.id.to_string(),
                json!({"left":win.spec.left, "top":win.spec.top}),
            );
        });
        json!({"geom":positions})
    }

    /// Returns `true` when no windows are open.
    pub fn is_empty(&self) -> bool {
        self.windows.len() == 0
    }
}
