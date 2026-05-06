//! Render a small scene through the native facade and write the result
//! as a binary PPM (P6) so the example stays dependency-free. View the
//! output with most image viewers, or convert via:
//!
//!     convert basic_render.ppm basic_render.png
//!
//! Run with:
//!
//!     cargo run --example basic_render --no-default-features \
//!         --features vulkan,freetype --release

use std::fs::File;
use std::io::{BufWriter, Write};

use skia_canvas::native::{
    LinearColorSpace, NativeBackend, NativePaint, NativePath, FillRule, Rect, RgbaLinear,
    SurfaceOptions,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let backend = NativeBackend::new();
    let mut surface = backend.create_surface(
        320,
        180,
        SurfaceOptions {
            color_space: LinearColorSpace::Srgb,
            ..SurfaceOptions::default()
        },
    )?;

    let triangle = NativePath::from_svg("M40 140 L160 30 L280 140 Z", FillRule::NonZero)?;

    surface.with_canvas(|canvas| {
        canvas.clear(RgbaLinear::opaque(0.05, 0.06, 0.10));

        let mut tri_paint = NativePaint::fill(RgbaLinear::opaque(0.95, 0.45, 0.20));
        tri_paint.set_anti_alias(true);
        canvas.draw_path(&triangle, &tri_paint);

        canvas.draw_rect(
            Rect::from_xywh(20.0, 20.0, 80.0, 40.0),
            &NativePaint::fill(RgbaLinear::opaque(0.2, 0.7, 1.0)),
        );
    });

    let frame = surface.read_pixels()?;
    let (w, h, stride) = (frame.width() as usize, frame.height() as usize, frame.stride());
    let pixels = frame.pixels();

    let path = "basic_render.ppm";
    let mut out = BufWriter::new(File::create(path)?);
    write!(out, "P6\n{w} {h}\n255\n")?;
    for y in 0..h {
        let row = &pixels[y * stride..y * stride + w * 4];
        for chunk in row.chunks_exact(4) {
            out.write_all(&chunk[..3])?;
        }
    }
    out.flush()?;

    println!("wrote {w}x{h} PPM to {path}");
    Ok(())
}
