//! A flat colour on an eight-bit canvas in a wide gamut lands as the colour
//! it names, converted exactly into the canvas's space, and a PDF or SVG of
//! the canvas writes it as its exact sRGB encoding.
//!
//! Skia keeps a paint's colour in sRGB, converting it when it is set, and
//! converts it into the canvas's space when it is drawn, both through
//! skcms's approximate transfer function. The two together moved a channel
//! by up to about 1e-4, which is a level wherever it crossed a rounding
//! boundary: on a Display P3 canvas `color(display-p3 0.5 0.5 0.5)` filled
//! 127,127,127 where an sRGB canvas fills the same grey 128,128,128, and
//! `color(srgb 0.5 0.5 0.5)` filled 128,127,127; and a PDF of the canvas
//! wrote the grey as `.4999 .5 .5`.

use meo_skia_canvas::prelude::*;

/// A colour space a case is named in or drawn onto, with the conversion a
/// CSS colour in it takes, held here independently of the crate's own.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Space {
    Srgb,
    DisplayP3,
    Rec2020,
}

impl Space {
    fn css(self) -> &'static str {
        match self {
            Space::Srgb => "srgb",
            Space::DisplayP3 => "display-p3",
            Space::Rec2020 => "rec2020",
        }
    }

    fn pixels(self) -> PixelColorSpace {
        match self {
            Space::Srgb => PixelColorSpace::Srgb,
            Space::DisplayP3 => PixelColorSpace::DisplayP3,
            Space::Rec2020 => PixelColorSpace::Rec2020,
        }
    }

    /// The transfer function, encoded to linear: the sRGB curve for sRGB and
    /// Display P3, CSS Color 4 section 10.8's pure 2.4 gamma for `rec2020`.
    fn decode(self, v: f64) -> f64 {
        match self {
            Space::Srgb | Space::DisplayP3 => match v.abs() <= 0.04045 {
                true => v / 12.92,
                false => v.signum() * ((v.abs() + 0.055) / 1.055).powf(2.4),
            },
            Space::Rec2020 => v.signum() * v.abs().powf(2.4),
        }
    }

    fn encode(self, v: f64) -> f64 {
        match self {
            Space::Srgb | Space::DisplayP3 => match v.abs() <= 0.0031308 {
                true => v * 12.92,
                false => v.signum() * (1.055 * v.abs().powf(1.0 / 2.4) - 0.055),
            },
            Space::Rec2020 => v.signum() * v.abs().powf(1.0 / 2.4),
        }
    }

    /// Linear values in this space to CIE XYZ (D65), and back: CSS Color 4
    /// section 18's matrices, in their rational forms.
    fn linear_to_xyz(self) -> [[f64; 3]; 3] {
        match self {
            Space::Srgb => [
                [506752.0 / 1228815.0, 87881.0 / 245763.0, 12673.0 / 70218.0],
                [87098.0 / 409605.0, 175762.0 / 245763.0, 12673.0 / 175545.0],
                [7918.0 / 409605.0, 87881.0 / 737289.0, 1001167.0 / 1053270.0],
            ],
            Space::DisplayP3 => [
                [
                    608311.0 / 1250200.0,
                    189793.0 / 714400.0,
                    198249.0 / 1000160.0,
                ],
                [
                    35783.0 / 156275.0,
                    247089.0 / 357200.0,
                    198249.0 / 2500400.0,
                ],
                [0.0, 32229.0 / 714400.0, 5220557.0 / 5000800.0],
            ],
            Space::Rec2020 => [
                [
                    63426534.0 / 99577255.0,
                    20160776.0 / 139408157.0,
                    47086771.0 / 278816314.0,
                ],
                [
                    26158966.0 / 99577255.0,
                    472592308.0 / 697040785.0,
                    8267143.0 / 139408157.0,
                ],
                [0.0, 19567812.0 / 697040785.0, 295819943.0 / 278816314.0],
            ],
        }
    }

    fn xyz_to_linear(self) -> [[f64; 3]; 3] {
        match self {
            Space::Srgb => [
                [12831.0 / 3959.0, -329.0 / 214.0, -1974.0 / 3959.0],
                [
                    -851781.0 / 878810.0,
                    1648619.0 / 878810.0,
                    36519.0 / 878810.0,
                ],
                [705.0 / 12673.0, -2585.0 / 12673.0, 705.0 / 667.0],
            ],
            Space::DisplayP3 => [
                [
                    446124.0 / 178915.0,
                    -333277.0 / 357830.0,
                    -72051.0 / 178915.0,
                ],
                [-14852.0 / 17905.0, 63121.0 / 35810.0, 423.0 / 17905.0],
                [11844.0 / 330415.0, -50337.0 / 660830.0, 316169.0 / 330415.0],
            ],
            Space::Rec2020 => [
                [
                    30757411.0 / 17917100.0,
                    -6372589.0 / 17917100.0,
                    -4539589.0 / 17917100.0,
                ],
                [
                    -19765991.0 / 29648200.0,
                    47925759.0 / 29648200.0,
                    467509.0 / 29648200.0,
                ],
                [
                    792561.0 / 44930125.0,
                    -1921689.0 / 44930125.0,
                    42328811.0 / 44930125.0,
                ],
            ],
        }
    }

    /// `rgb`, named in `self`, converted exactly into `into`: as it is into
    /// its own space, which a round trip through the curves would move by an
    /// ulp. A neutral stays neutral by construction: the three spaces share
    /// their white, so a grey is only re-encoded, where the matrices, in
    /// floating point, would move one channel off the others by an ulp and
    /// break a tie -- and between two spaces on one curve, sRGB's and
    /// Display P3's, it is unchanged.
    fn into(self, into: Space, rgb: [f64; 3]) -> [f64; 3] {
        let one_curve = (self == Space::Rec2020) == (into == Space::Rec2020);
        let neutral = rgb[0] == rgb[1] && rgb[1] == rgb[2];
        if self == into || (neutral && one_curve) {
            return rgb;
        }
        if neutral {
            return [into.encode(self.decode(rgb[0])); 3];
        }
        let apply = |m: [[f64; 3]; 3], v: [f64; 3]| {
            m.map(|row| row[0] * v[0] + row[1] * v[1] + row[2] * v[2])
        };
        let linear = rgb.map(|v| self.decode(v));
        apply(into.xyz_to_linear(), apply(self.linear_to_xyz(), linear))
            .map(|v| into.encode(v))
    }
}

/// The byte an eight-bit canvas stores a channel as: rounded half up, as an
/// sRGB canvas stores a colour named in sRGB -- `color(srgb 0.5 0.5 0.5)`
/// fills 128.
fn byte(v: f64) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5).floor() as u8
}

fn css(space: Space, rgb: [f64; 3]) -> String {
    format!("color({} {} {} {})", space.css(), rgb[0], rgb[1], rgb[2])
}

/// Both engines: the raster backend, and the GPU's, whose shader stages
/// can carry a colour at half precision.
const ENGINES: [bool; 2] = [false, true];

fn page(space: Space, gpu: bool) -> Canvas {
    let mut canvas = Canvas::with_options(
        1.0,
        1.0,
        CanvasOptions {
            color_space: space.pixels(),
            ..CanvasOptions::default()
        },
    )
    .expect("a canvas");
    canvas.set_gpu(gpu);
    canvas
}

/// What `draw` leaves on a one-pixel canvas in `space`, read in that space.
fn pixel(space: Space, gpu: bool, draw: impl Fn(&mut Context2D)) -> [u8; 3] {
    let mut canvas = page(space, gpu);
    draw(canvas.context());
    let data = canvas
        .context()
        .get_image_data_as(
            0.0,
            0.0,
            1.0,
            1.0,
            PixelExportOptions {
                color_space: space.pixels(),
                ..PixelExportOptions::default()
            },
        )
        .expect("a pixel");
    let p = data.pixels();
    [p[0], p[1], p[2]]
}

/// Greys at every tie between two bytes, `(k + 0.5) / 255`, where a channel
/// an approximation moves by any amount lands a level off; and 300 colours
/// from a fixed sequence.
fn colours() -> Vec<[f64; 3]> {
    let ties = (0..255).map(|k| [(k as f64 + 0.5) / 255.0; 3]);
    let mut seed: u64 = 7;
    let mut next = move || {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((seed >> 11) as f64 / (1u64 << 53) as f64 * 1e6).round() / 1e6
    };
    let random = (0..300)
        .map(|_| [next(), next(), next()])
        .collect::<Vec<_>>();
    ties.chain(random).collect()
}

/// Every case `named` names, filled onto a canvas in `page`, against its
/// exact conversion: the cases that part from it.
fn parted(page_space: Space, named: Space, gpu: bool) -> Vec<String> {
    colours()
        .into_iter()
        .filter_map(|rgb| {
            let colour = css(named, rgb);
            let got = pixel(page_space, gpu, |ctx| {
                ctx.set_fill_style_css(&colour).expect("a colour");
                ctx.fill_rect(0.0, 0.0, 1.0, 1.0);
            });
            let want = named.into(page_space, rgb).map(byte);
            (got != want)
                .then(|| format!("gpu {gpu}, {colour}: {got:?} for {want:?}"))
        })
        .collect()
}

/// The control: on an sRGB canvas a colour named in sRGB lands as its own
/// encoding rounded half up, which holds the reference and the rounding here
/// to what the canvas already does, and holds an sRGB canvas as it was.
#[test]
fn an_eight_bit_srgb_canvas_fills_a_colour_as_named() {
    // The raster engine alone. On the GPU an sRGB canvas draws Skia's own
    // paint colour, and 127 of the 255 ties -- every one from 128.5 up --
    // store the byte below; that is Skia's GPU route, not one this crate
    // places.
    let parted = parted(Space::Srgb, Space::Srgb, false);
    assert!(parted.is_empty(), "{} parted: {parted:#?}", parted.len());
}

/// On an eight-bit Display P3 or Rec2020 canvas a colour named in a space
/// the canvas holds lands as its exact conversion into the canvas's space.
#[test]
fn an_eight_bit_wide_gamut_canvas_fills_a_colour_as_its_exact_conversion() {
    let mut failures = vec![];
    for (page_space, named) in [
        (Space::DisplayP3, Space::DisplayP3),
        (Space::DisplayP3, Space::Srgb),
        (Space::Rec2020, Space::Rec2020),
        (Space::Rec2020, Space::DisplayP3),
        (Space::Rec2020, Space::Srgb),
    ] {
        let parted: Vec<String> = ENGINES
            .into_iter()
            .flat_map(|gpu| parted(page_space, named, gpu))
            .collect();
        if !parted.is_empty() {
            failures.push(format!(
                "{page_space:?} canvas, named in {named:?}: {} of {} parted, {:?}",
                parted.len(),
                ENGINES.len() * colours().len(),
                &parted[..parted.len().min(3)]
            ));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// The alphas a translucent case is drawn at: ties between two bytes among
/// them, 0.3 and 0.7 -- 76.5 and 178.5 levels -- where a store rounding
/// ties to even parts from a paint's colour, which rounds them up.
const ALPHAS: [f64; 6] = [0.1, 0.25, 0.3, 0.5, 0.7, 0.9];

/// `css` at alpha `a`.
fn css_at(space: Space, rgb: [f64; 3], a: f64) -> String {
    format!(
        "color({} {} {} {} / {a})",
        space.css(),
        rgb[0],
        rgb[1],
        rgb[2]
    )
}

/// What `draw` leaves at the origin of a canvas `size` pixels square in
/// `space`, read premultiplied in that space: the bytes the canvas stores.
fn stored(
    space: Space,
    size: f32,
    gpu: bool,
    draw: impl Fn(&mut Context2D),
) -> [u8; 4] {
    let mut canvas = Canvas::with_options(
        size,
        size,
        CanvasOptions {
            color_space: space.pixels(),
            ..CanvasOptions::default()
        },
    )
    .expect("a canvas");
    canvas.set_gpu(gpu);
    draw(canvas.context());
    let data = canvas
        .context()
        .get_image_data_as(
            0.0,
            0.0,
            1.0,
            1.0,
            PixelExportOptions {
                color_space: space.pixels(),
                premultiplied: true,
                ..PixelExportOptions::default()
            },
        )
        .expect("a pixel");
    let p = data.pixels();
    [p[0], p[1], p[2], p[3]]
}

/// On an eight-bit Display P3 or Rec2020 canvas a translucent colour, and
/// one under a global alpha, stores its exact premultiplied conversion
/// rounded, and its alpha on the byte a paint's colour stores it on --
/// rounded half up, as Chrome's Display P3 canvas does. Not an sRGB
/// canvas's bytes: Skia blits a colour onto one through a legacy path that
/// rounds the colour to a byte before it premultiplies.
#[test]
fn a_translucent_colour_on_an_eight_bit_wide_gamut_canvas_stores_its_exact_product()
 {
    let mut failures = vec![];
    for (page_space, named) in [
        (Space::DisplayP3, Space::DisplayP3),
        (Space::DisplayP3, Space::Srgb),
        (Space::Rec2020, Space::Rec2020),
        (Space::Rec2020, Space::Srgb),
    ] {
        let mut parted = vec![];
        let mut cases = 0;
        for (gpu, rgb) in ENGINES.into_iter().flat_map(|gpu| {
            colours().into_iter().step_by(5).map(move |rgb| (gpu, rgb))
        }) {
            for a in ALPHAS {
                for global in [1.0, 0.6] {
                    cases += 1;
                    let colour = css_at(named, rgb, a);
                    let got = stored(page_space, 1.0, gpu, |ctx| {
                        ctx.set_global_alpha(global);
                        ctx.set_fill_style_css(&colour).expect("a colour");
                        ctx.fill_rect(0.0, 0.0, 1.0, 1.0);
                    });
                    // The alpha Skia holds is the product in `f32`.
                    let alpha = a as f32 * global;
                    let exact = named
                        .into(page_space, rgb)
                        .map(|c| c.clamp(0.0, 1.0) * f64::from(alpha) * 255.0);
                    let alpha_byte = (alpha * 255.0 + 0.5) as u8;
                    let within = (0..3).all(|i| {
                        (f64::from(got[i]) - exact[i]).abs() <= 0.5 + 1e-6
                    });
                    if !within || got[3] != alpha_byte {
                        parted.push(format!(
                            "gpu {gpu}, {colour} under {global}: {got:?} for \
                             {exact:?}, \
                             {alpha_byte}"
                        ));
                    }
                }
            }
        }
        if !parted.is_empty() {
            failures.push(format!(
                "{page_space:?} canvas, named in {named:?}: {} of {cases} \
                 parted, {:?}",
                parted.len(),
                &parted[..parted.len().min(3)]
            ));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A drop-shadow on an eight-bit Display P3 canvas draws a fill's bytes,
/// opaque and translucent.
#[test]
fn a_drop_shadow_on_an_eight_bit_wide_gamut_canvas_draws_the_fill_s_bytes() {
    let mut failures = vec![];
    for (gpu, rgb) in ENGINES.into_iter().flat_map(|gpu| {
        colours().into_iter().step_by(7).map(move |rgb| (gpu, rgb))
    }) {
        for (named, a) in
            [Space::DisplayP3, Space::Srgb]
                .into_iter()
                .flat_map(|named| {
                    [1.0].into_iter().chain(ALPHAS).map(move |a| (named, a))
                })
        {
            let colour = css_at(named, rgb, a);
            let fill = stored(Space::DisplayP3, 1.0, gpu, |ctx| {
                ctx.set_fill_style_css(&colour).expect("a colour");
                ctx.fill_rect(0.0, 0.0, 1.0, 1.0);
            });
            let shadow = stored(Space::DisplayP3, 1.0, gpu, |ctx| {
                ctx.set_filter_css(&format!("drop-shadow(10px 0 0 {colour})"))
                    .expect("a filter");
                ctx.fill_rect(-10.0, 0.0, 1.0, 1.0);
            });
            if shadow != fill {
                failures.push(format!(
                    "gpu {gpu}, {colour}: {shadow:?} for {fill:?}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// The fill colours a PDF and an SVG of `canvas` write: the operands of the
/// PDF's `rg` and the SVG's `fill` attributes.
fn written(canvas: &mut Canvas) -> (Vec<String>, Vec<String>) {
    let pdf = canvas
        .to_buffer(ImageFormat::Pdf, &EncodeOptions::default())
        .expect("a PDF");
    let svg = canvas
        .to_buffer(ImageFormat::Svg, &EncodeOptions::default())
        .expect("an SVG");
    let mut operands = vec![];
    let bytes = pdf.as_slice();
    let mut at = 0;
    while let Some(start) = find(&bytes[at..], b"stream\n").map(|i| at + i + 7)
    {
        let end = find(&bytes[start..], b"endstream")
            .map_or(bytes.len(), |i| start + i);
        // Deflated, or -- a stream too small to gain by it -- written as it
        // is; one that is neither text is a font or an image.
        let mut text = String::new();
        let inflated = std::io::Read::read_to_string(
            &mut flate2::read::ZlibDecoder::new(&bytes[start..end]),
            &mut text,
        )
        .is_ok();
        let plain = std::str::from_utf8(&bytes[start..end]).ok();
        if let Some(text) = inflated.then_some(text.as_str()).or(plain) {
            // Each `r g b rg`: the three numbers before the operator.
            let words: Vec<&str> = text.split_whitespace().collect();
            operands.extend(
                words
                    .windows(4)
                    .filter(|w| w[3] == "rg")
                    .map(|w| w[..3].join(" ")),
            );
        }
        at = end;
    }
    let svg = String::from_utf8_lossy(&svg).to_string();
    let fills = svg
        .split("fill=\"")
        .skip(1)
        .filter_map(|rest| rest.split('"').next())
        .map(str::to_string)
        .collect();
    (operands, fills)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// A PDF or SVG of an eight-bit wide-gamut canvas writes a fill in a colour
/// inside sRGB as an sRGB canvas writes it: as the colour's exact sRGB
/// encoding. A document is written in sRGB, and the colour reached it
/// through Skia's approximate conversion, `.4999 .5 .5` for a grey of 0.5.
#[test]
fn a_document_of_a_wide_gamut_canvas_writes_a_colour_s_exact_srgb_encoding() {
    let mut failures = vec![];
    for page_space in [Space::DisplayP3, Space::Rec2020] {
        for rgb in
            [[0.5; 3], [0.25, 0.5, 0.75], [0.2, 0.6, 0.4], [1.0 / 3.0; 3]]
        {
            for named in [Space::Srgb, Space::DisplayP3] {
                let colour = css(named, rgb);
                let triangle = |ctx: &mut Context2D| {
                    ctx.set_fill_style_css(&colour).expect("a colour");
                    ctx.begin_path();
                    ctx.move_to(0.1, 0.1);
                    ctx.line_to(0.9, 0.2);
                    ctx.line_to(0.4, 0.9);
                    ctx.fill(FillRule::NonZero);
                };
                let mut wide = page(page_space, false);
                triangle(wide.context());
                let mut srgb = page(Space::Srgb, false);
                triangle(srgb.context());
                let (got, want) = (written(&mut wide), written(&mut srgb));
                if got != want {
                    failures.push(format!(
                        "{page_space:?}, {colour}: {got:?} for {want:?}"
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// The font a glyph's interior is drawn in.
const OSWALD: &str = "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf";
const FAMILY: &str = "WideGamutEightBitOswald";

/// Text on an eight-bit Display P3 canvas stores a fill's bytes where a
/// glyph covers a pixel whole, opaque and translucent.
#[test]
fn text_on_an_eight_bit_wide_gamut_canvas_stores_the_fill_s_bytes_inside_a_glyph()
 {
    FontLibrary::new()
        .use_paths(Some(FAMILY), [OSWALD])
        .expect("Oswald");
    let font = format!("100px {FAMILY}");
    // A pixel of the H's stem: covered whole, as its neighbours are.
    let size = 120;
    let mut canvas = Canvas::new(size as f32, size as f32);
    canvas.set_gpu(false);
    let ctx = canvas.context();
    assert!(ctx.set_font_css(&font), "a font");
    ctx.fill_text("H", 10.0, 110.0, None);
    let alpha = ctx
        .get_image_data(0.0, 0.0, size as f32, size as f32)
        .expect("pixels")
        .pixels()
        .iter()
        .skip(3)
        .step_by(4)
        .copied()
        .collect::<Vec<u8>>();
    let whole = |x: usize, y: usize| {
        (y - 1..=y + 1)
            .all(|y| (x - 1..=x + 1).all(|x| alpha[y * size + x] == 255))
    };
    let (x, y) = (1..size - 1)
        .flat_map(|y| (1..size - 1).map(move |x| (x, y)))
        .find(|&(x, y)| whole(x, y))
        .expect("a pixel the glyph covers whole");
    let mut failures = vec![];
    for (gpu, rgb) in ENGINES.into_iter().flat_map(|gpu| {
        colours().into_iter().step_by(7).map(move |rgb| (gpu, rgb))
    }) {
        for (named, a) in
            [Space::DisplayP3, Space::Srgb]
                .into_iter()
                .flat_map(|named| {
                    [1.0].into_iter().chain(ALPHAS).map(move |a| (named, a))
                })
        {
            let colour = css_at(named, rgb, a);
            let fill = stored(Space::DisplayP3, 1.0, gpu, |ctx| {
                ctx.set_fill_style_css(&colour).expect("a colour");
                ctx.fill_rect(0.0, 0.0, 1.0, 1.0);
            });
            let text = stored(Space::DisplayP3, 1.0, gpu, |ctx| {
                assert!(ctx.set_font_css(&font), "a font");
                ctx.set_fill_style_css(&colour).expect("a colour");
                ctx.fill_text("H", 10.0 - x as f32, 110.0 - y as f32, None);
            });
            if text != fill {
                failures.push(format!(
                    "gpu {gpu}, {colour}: {text:?} for {fill:?}"
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} parted: {failures:#?}",
        failures.len()
    );
}

/// A blurred draw on an eight-bit Display P3 canvas stores a fill's bytes
/// where the blur leaves it whole: a rectangle under `blur()` and a
/// rectangle's blurred shadow.
#[test]
fn a_blurred_draw_on_an_eight_bit_wide_gamut_canvas_stores_the_fill_s_bytes_inside()
 {
    let mut failures = vec![];
    for (gpu, rgb) in ENGINES.into_iter().flat_map(|gpu| {
        colours().into_iter().step_by(7).map(move |rgb| (gpu, rgb))
    }) {
        for (named, a) in
            [Space::DisplayP3, Space::Srgb]
                .into_iter()
                .flat_map(|named| {
                    [1.0].into_iter().chain(ALPHAS).map(move |a| (named, a))
                })
        {
            let colour = css_at(named, rgb, a);
            let fill = stored(Space::DisplayP3, 1.0, gpu, |ctx| {
                ctx.set_fill_style_css(&colour).expect("a colour");
                ctx.fill_rect(0.0, 0.0, 1.0, 1.0);
            });
            let rect = stored(Space::DisplayP3, 1.0, gpu, |ctx| {
                ctx.set_filter_css("blur(1px)").expect("a filter");
                ctx.set_fill_style_css(&colour).expect("a colour");
                ctx.fill_rect(-40.0, -40.0, 80.0, 80.0);
            });
            let shadow = stored(Space::DisplayP3, 1.0, gpu, |ctx| {
                ctx.set_shadow_color_css(&colour).expect("a colour");
                ctx.set_shadow_blur(2.0);
                ctx.set_shadow_offset(100.0, 0.0);
                ctx.fill_rect(-140.0, -40.0, 80.0, 80.0);
            });
            for (route, got) in [("rect", rect), ("shadow", shadow)] {
                if got != fill {
                    failures.push(format!(
                        "{route} {colour}: {got:?} for {fill:?}"
                    ));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} parted: {failures:#?}",
        failures.len()
    );
}

/// A document of a Display P3 or Rec2020 canvas, eight-bit or float, writes
/// a paragraph's colour in its exact sRGB encoding, as a document of an
/// sRGB canvas writes a fill of it, where the paints the paragraph was built
/// with hold Skia's.
#[test]
fn a_document_of_a_wide_gamut_canvas_writes_a_paragraph_s_colour_s_exact_srgb_encoding()
 {
    let fonts = FontLibrary::new();
    fonts.use_paths(Some(FAMILY), [OSWALD]).expect("Oswald");
    let engine = TextEngine::new(&fonts);
    let mut failures = vec![];
    for (page_space, depth) in [Space::DisplayP3, Space::Rec2020]
        .into_iter()
        .flat_map(|s| [PixelDepth::Uint8, PixelDepth::F32].map(|d| (s, d)))
    {
        for i in 1..=10 {
            let v = i as f32 / 11.0;
            let colour =
                RgbaLinear::new_premultiplied(v, v * 0.5, 1.0 - v, 1.0);
            let paragraph = engine.layout_text(
                "H",
                &TextStyle {
                    font_families: vec![FAMILY.into()],
                    font_size: 30.0,
                    color: colour,
                    ..TextStyle::default()
                },
                40.0,
            );
            let sized = |space: Space, depth: PixelDepth| {
                Canvas::with_options(
                    40.0,
                    40.0,
                    CanvasOptions {
                        color_space: space.pixels(),
                        color_type: depth,
                        gpu: false,
                        ..CanvasOptions::default()
                    },
                )
                .expect("a canvas")
            };
            let mut text = sized(page_space, depth);
            text.context().draw_paragraph(&paragraph, 0.0, 0.0);
            let mut fill = sized(Space::Srgb, PixelDepth::Uint8);
            let ctx = fill.context();
            ctx.set_fill_style(colour);
            ctx.begin_path();
            ctx.move_to(4.0, 4.0);
            ctx.line_to(36.0, 8.0);
            ctx.line_to(16.0, 36.0);
            ctx.fill(FillRule::NonZero);
            let ((text_pdf, _), (fill_pdf, _)) =
                (written(&mut text), written(&mut fill));
            if text_pdf.is_empty() || text_pdf != fill_pdf {
                failures.push(format!(
                    "{page_space:?} {depth:?}, {colour:?}: {text_pdf:?} for \
                     {fill_pdf:?}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// The eight-bit pages a placed colour is checked on, by the `color()`
/// space that names a colour in each one's own space.
const PLACED_PAGES: [(PixelColorSpace, &str); 3] = [
    (PixelColorSpace::DisplayP3, "display-p3"),
    (PixelColorSpace::Rec2020, "rec2020"),
    (PixelColorSpace::SrgbLinear, "srgb-linear"),
];

/// Each premultiplied byte `p` under each alpha byte `a`, as three colours
/// in a page's own space: a grey; beside a full and a complementary channel;
/// and skewed across the three. On every page in [`PLACED_PAGES`] a colour
/// is drawn as a paint's own colour wherever Skia lands it on its bytes, and
/// through a shader where it does not -- a dark channel beside bright ones
/// landed up to three levels off on Rec. 2020 -- so each must store its
/// bytes whichever it took.
fn sweep() -> Vec<[u32; 4]> {
    (1..=255u32)
        .flat_map(|a| {
            (0..=a).flat_map(move |p| {
                [
                    [p, p, p, a],
                    [p, a, a - p, a],
                    [a - p, p, (p * 7) % (a + 1), a],
                ]
            })
        })
        .collect()
}

/// What `want`, each premultiplied bytes, store when each is filled, named
/// in `name`, onto its own pixel of an eight-bit `space` canvas: the
/// cases that part.
fn parted_bytes(
    space: PixelColorSpace,
    name: &str,
    gpu: bool,
    want: &[[u32; 4]],
) -> Vec<String> {
    let w = 256;
    let h = want.len().div_ceil(w);
    let mut canvas = Canvas::with_options(
        w as f32,
        h as f32,
        CanvasOptions {
            color_space: space,
            ..CanvasOptions::default()
        },
    )
    .expect("a canvas");
    canvas.set_gpu(gpu);
    let ctx = canvas.context();
    for (i, [r, g, b, a]) in want.iter().enumerate() {
        let a = f64::from(*a);
        let css = format!(
            "color({name} {} {} {} / {})",
            f64::from(*r) / a,
            f64::from(*g) / a,
            f64::from(*b) / a,
            a / 255.0
        );
        ctx.set_fill_style_css(&css).expect("a colour");
        ctx.fill_rect((i % w) as f32, (i / w) as f32, 1.0, 1.0);
    }
    let data = ctx
        .get_image_data_as(
            0.0,
            0.0,
            w as f32,
            h as f32,
            PixelExportOptions {
                color_space: space,
                premultiplied: true,
                ..PixelExportOptions::default()
            },
        )
        .expect("pixels");
    want.iter()
        .zip(data.pixels().as_chunks::<4>().0)
        .filter(|(want, got)| (0..4).any(|i| u32::from(got[i]) != want[i]))
        .map(|(want, got)| format!("{name} gpu {gpu}: {got:?} for {want:?}"))
        .collect()
}

/// Every premultiplied byte under every alpha byte stores as named on an
/// eight-bit wide-gamut canvas, on both engines, through the route a fill
/// takes -- a paint's own colour where Skia lands it, a shader where not.
#[test]
fn every_premultiplied_byte_under_every_alpha_byte_stores_as_named() {
    let want = sweep();
    let mut failures = vec![];
    for (space, name) in PLACED_PAGES {
        for gpu in ENGINES {
            let parted = parted_bytes(space, name, gpu, &want);
            if !parted.is_empty() {
                failures.push(format!(
                    "{} of {} parted, {:?}",
                    parted.len(),
                    want.len(),
                    &parted[..parted.len().min(3)]
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A colour Skia lands a level off as a paint's colour, and no move within
/// its byte brings back, stores its bytes: Rec. 2020's `1 1 0` at an alpha
/// of 89, which the shader draws.
#[test]
fn a_colour_skia_would_land_off_its_byte_stores_its_bytes() {
    for gpu in ENGINES {
        let parted = parted_bytes(
            PixelColorSpace::Rec2020,
            "rec2020",
            gpu,
            &[[89, 89, 0, 89]],
        );
        assert!(parted.is_empty(), "{parted:?}");
    }
}

/// Colours named outside an eight-bit wide-gamut canvas's gamut, or far
/// from its primaries, store their exact product, the float canvas in the
/// same space giving the exact value: their conversion runs through extended
/// sRGB values, which `color(a98-rgb 0 1 0)` landed a level off on a
/// Display P3 canvas as a paint's colour.
#[test]
fn a_saturated_colour_on_an_eight_bit_wide_gamut_canvas_stores_its_exact_product()
 {
    let named = [
        "color(a98-rgb 0 1 0)",
        "color(a98-rgb 1 0 0)",
        "color(a98-rgb 0 0 1)",
        "color(prophoto-rgb 0 1 0)",
        "color(prophoto-rgb -0.2 0.5 1.1)",
        "color(rec2020 0 1 0)",
        "color(rec2020 1 0 0)",
        "color(display-p3 0 1 0)",
        "color(display-p3 0.002 0.9 0.95)",
        "color(srgb-linear 0 0.001 1)",
    ];
    let mut failures = vec![];
    for (space, _) in PLACED_PAGES {
        for css in named {
            let mut exact = Canvas::with_options(
                1.0,
                1.0,
                CanvasOptions {
                    color_space: space,
                    color_type: PixelDepth::F32,
                    gpu: false,
                    ..CanvasOptions::default()
                },
            )
            .expect("a canvas");
            exact.context().set_fill_style_css(css).expect("a colour");
            exact.context().fill_rect(0.0, 0.0, 1.0, 1.0);
            let floats = exact
                .context()
                .get_image_data_as(
                    0.0,
                    0.0,
                    1.0,
                    1.0,
                    PixelExportOptions {
                        color_space: space,
                        depth: PixelDepth::F32,
                        ..PixelExportOptions::default()
                    },
                )
                .expect("a pixel");
            let (channels, _) = floats.pixels().as_chunks::<4>();
            // The product in `f32`, rounded half up, as a placed colour is:
            // a tie such as 0.9 of 255 rounds up here as it does there.
            let rgb: Vec<f32> = channels[..3]
                .iter()
                .map(|c| f32::from_le_bytes(*c).clamp(0.0, 1.0))
                .collect();
            for (a, gpu) in [1.0, 0.5, 0.2]
                .into_iter()
                .flat_map(|a| ENGINES.map(|gpu| (a, gpu)))
            {
                let alpha = (a as f32 * 255.0 + 0.5) as u8;
                let want =
                    rgb.iter().map(|c| (c * a as f32 * 255.0 + 0.5) as u8);
                let mut canvas = Canvas::with_options(
                    1.0,
                    1.0,
                    CanvasOptions {
                        color_space: space,
                        ..CanvasOptions::default()
                    },
                )
                .expect("a canvas");
                canvas.set_gpu(gpu);
                let ctx = canvas.context();
                ctx.set_global_alpha(a as f32);
                ctx.set_fill_style_css(css).expect("a colour");
                ctx.fill_rect(0.0, 0.0, 1.0, 1.0);
                let got = ctx
                    .get_image_data_as(
                        0.0,
                        0.0,
                        1.0,
                        1.0,
                        PixelExportOptions {
                            color_space: space,
                            premultiplied: true,
                            ..PixelExportOptions::default()
                        },
                    )
                    .expect("a pixel")
                    .pixels()
                    .to_vec();
                let want: Vec<u8> = want.chain([alpha]).collect();
                let within = got
                    .iter()
                    .zip(&want)
                    .take(3)
                    .all(|(g, w)| g.abs_diff(*w) == 0);
                if !within || got[3] != want[3] {
                    failures.push(format!(
                        "{space:?} gpu {gpu} {css} at {a}: {got:?} for {want:?}"
                    ));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} parted: {failures:#?}",
        failures.len()
    );
}
