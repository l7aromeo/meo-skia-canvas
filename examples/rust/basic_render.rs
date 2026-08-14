//! Draw a small scene through the Canvas facade and write it as a PNG.
//!
//! Run with:
//!
//!     cargo run --example basic_render

use meo_skia_canvas::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut canvas = Canvas::new(320.0, 180.0);

    {
        let ctx = canvas.context();

        ctx.set_fill_style(RgbaLinear::opaque(0.05, 0.06, 0.10));
        ctx.fill_rect(0.0, 0.0, 320.0, 180.0);

        // A triangle from SVG path data, filled.
        let triangle =
            Path2D::from_svg("M40 140 L160 30 L280 140 Z", FillRule::NonZero)?;
        ctx.set_fill_style(RgbaLinear::opaque(0.95, 0.45, 0.20));
        ctx.fill_path(&triangle, FillRule::NonZero);

        // A rounded rectangle, stroked, built segment by segment.
        let mut frame = PathBuilder::new();
        frame.round_rect(20.0, 20.0, 280.0, 140.0, [12.0; 4])?;
        ctx.set_stroke_style(RgbaLinear::opaque(1.0, 1.0, 1.0));
        ctx.set_line_width(3.0);
        ctx.stroke_path(&frame.build(FillRule::NonZero));

        // And a line of text over the top.
        ctx.set_font(&Font::new("Helvetica", 24.0).weight(700));
        ctx.set_text_align(TextAlign::Center);
        ctx.set_fill_style(RgbaLinear::opaque(1.0, 1.0, 1.0));
        ctx.fill_text("meo-skia-canvas", 160.0, 165.0, None);
    }

    canvas.to_file("basic_render.png", &EncodeOptions::default())?;
    println!("wrote basic_render.png");
    Ok(())
}
