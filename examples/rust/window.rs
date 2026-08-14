//! Open a window and animate it, driven entirely from Rust.
//!
//! Run with:
//!
//!     cargo run --example window --features "window,metal"
//!
//! Swap `metal` for `vulkan` on Linux and Windows. The `window` feature is
//! what makes any of this compile; without a GPU backend there is nothing to
//! present a frame with.
//!
//! Escape or closing the window ends the loop.

use std::{cell::Cell, rc::Rc};

use meo_skia_canvas::prelude::*;

fn main() {
    let mut win = Window::new(480.0, 320.0);
    win.set_title("meo-skia-canvas");
    win.set_background("#101014");

    // Follows the pointer, so it is visibly reacting to events rather than
    // just animating on a timer.
    //
    // Shared through an `Rc<Cell<_>>` because the two handlers are separate
    // closures: each `move` would otherwise capture its own copy of a plain
    // local, and the one the draw handler read would never be the one the
    // event handler wrote.
    let pointer = Rc::new(Cell::new(Point::new(240.0, 160.0)));

    let seen = Rc::clone(&pointer);
    win.on_event(move |event| {
        if let UiEvent::Mouse { point, .. } = event {
            seen.set(*point);
        }
    });

    win.on_draw(move |ctx, frame| {
        let pointer = pointer.get();
        ctx.set_fill_style(RgbaLinear::opaque(0.06, 0.06, 0.08));
        ctx.fill_rect(0.0, 0.0, 480.0, 320.0);

        // The frame counter is handed in rather than tracked here, so the
        // animation does not need any state of its own.
        let phase = frame as f32 / 30.0;
        let radius = 40.0 + 12.0 * phase.sin();

        ctx.set_fill_style_css("skyblue").ok();
        ctx.begin_path();
        ctx.arc(
            pointer.x,
            pointer.y,
            radius,
            0.0,
            std::f32::consts::TAU,
            false,
        )
        .ok();
        ctx.fill(FillRule::NonZero);
    });

    win.open();

    // Blocks until the last window closes. On macOS this has to be the main
    // thread, which is why it is called here rather than from a worker.
    App::run();
}
