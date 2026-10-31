//! A CanvasKit filter as each page draws it.
//!
//! A filter holding a colour -- a blend colour filter, a drop shadow -- is
//! built before the page it lands on is known, and Skia converts the colour
//! it is handed through skcms's approximate transfer function, which landed
//! a colour 6.3e-5 from its encoding on an sRGB F32 page and up to 1.8e-4
//! from its value on a linear, P3 or Rec2020 one. So such a filter keeps how
//! to build itself beside the filter it built, and the page draws the one
//! built for it, as a fill colour is handed over for the page it fills --
//! see `skia_paint_color_onto`.
//!
//! A float page in a space other than sRGB keeps a recording for pixels
//! beside one for documents -- see `page::Recorders` -- and no colour handed
//! to Skia lands exactly there. Its pixels take a filter built
//! [`Onto::exact`]: the colour converted into the page's space on this side
//! and held where nothing converts it again, in [`exact_blend`], which the
//! CPU draws to the bit as Skia's own blend filter does. Its documents take
//! the filter as built, a plain colour a backend can write.

use std::{cell::RefCell, rc::Rc};

use skia_safe::{
    BlendMode, Blender, Color4f, ColorFilter, ColorSpace, Data, FilterMode,
    ImageFilter, Matrix, RuntimeEffect, SamplingOptions, Vector, color_filters,
    image_filters, runtime_effect::ChildPtr,
};

use crate::{
    node::utils::{
        color_in_canvas_space, skia_paint_color, skia_paint_color_onto,
    },
    pixels::PixelColorSpace,
};

/// The page a filter is built for.
#[derive(Clone, PartialEq)]
pub(crate) struct Onto {
    /// The space the page's pixels are in.
    pub destination: ColorSpace,
    /// Whether the filter is built for a page's pixels recording, with its
    /// colours converted here and held unconverted -- see [`Self::exact`].
    pub exact: bool,
}

impl Onto {
    /// A filter drawn onto a page in `destination` that keeps one
    /// recording, its colours handed over as a fill's are.
    pub fn shared(destination: ColorSpace) -> Self {
        Self {
            destination,
            exact: false,
        }
    }

    /// A filter drawn into the pixels recording of a page in
    /// `destination`: its colours converted into that space here and held
    /// where Skia does not convert them again.
    pub fn exact(destination: ColorSpace) -> Self {
        Self {
            destination,
            exact: true,
        }
    }

    /// `color` in `space` as Skia is handed it in a filter drawn onto this
    /// page -- see [`skia_paint_color_onto`].
    pub fn color(
        &self,
        color: Color4f,
        space: &ColorSpace,
    ) -> (Color4f, ColorSpace) {
        skia_paint_color_onto(color, space, &self.destination)
    }

    /// `color` in `space`, unpremultiplied, converted exactly into the
    /// page's space, for a filter built [`Self::exact`]. `None` for any
    /// other, and where the page's space is not one this crate converts.
    fn exact_color(
        &self,
        color: Color4f,
        space: &ColorSpace,
    ) -> Option<Color4f> {
        if !self.exact {
            return None;
        }
        let canvas = PixelColorSpace::matching(&self.destination)?;
        let [r, g, b] =
            color_in_canvas_space(color, space, canvas)?.map(|c| c as f32);
        Some(Color4f::new(r, g, b, color.a))
    }
}

/// A filter blending `color`, unpremultiplied in `space`, with what it
/// filters through `mode`, built for `onto`: exactly where it is built for
/// a page's pixels -- see [`exact_blend`] -- and as Skia's own blend filter
/// otherwise, handed the colour as a fill's is.
pub(crate) fn blend_filter(
    onto: Option<&Onto>,
    color: Color4f,
    space: &ColorSpace,
    mode: BlendMode,
) -> Option<ColorFilter> {
    if let Some(exact) = onto.and_then(|onto| onto.exact_color(color, space)) {
        return exact_blend(exact, mode);
    }
    let (color, space) = match onto {
        Some(onto) => onto.color(color, space),
        None => skia_paint_color(color, space),
    };
    color_filters::blend_with_color_space(color, space, mode)
}

/// A drop shadow at `offset`, blurred by `sigma`, in `color`,
/// unpremultiplied in `space`, of `input`, cropped to `crop`; with the
/// input drawn over it unless `only`. Built for `onto` as [`blend_filter`]
/// builds a colour: on a page's pixels as Skia builds the shadow
/// (`make_drop_shadow_graph`, `SkDropShadowImageFilter.cpp`) -- blurred,
/// tinted by a source-in blend, moved by a linear-sampled translation and
/// merged under the input -- with an [`exact_blend`] as its tint.
#[allow(clippy::too_many_arguments)]
pub(crate) fn shadow_filter(
    onto: Option<&Onto>,
    offset: Vector,
    sigma: (f32, f32),
    color: Color4f,
    space: &ColorSpace,
    input: Option<ImageFilter>,
    crop: image_filters::CropRect,
    only: bool,
) -> Option<ImageFilter> {
    if let Some(exact) = onto.and_then(|onto| onto.exact_color(color, space)) {
        let tint = exact_blend(exact, BlendMode::SrcIn)?;
        let blurred = image_filters::blur(sigma, None, input.clone(), None)?;
        let tinted = image_filters::color_filter(tint, blurred, None)?;
        let shadow = image_filters::matrix_transform(
            &Matrix::translate(offset),
            SamplingOptions::from(FilterMode::Linear),
            tinted,
        )?;
        let shadowed = match only {
            true => shadow,
            false => image_filters::merge([Some(shadow), input], None)?,
        };
        return match crop.rect() {
            Some(crop) => image_filters::crop(crop, None, shadowed),
            None => Some(shadowed),
        };
    }
    let (color, space) = match onto {
        Some(onto) => onto.color(color, space),
        None => skia_paint_color(color, space),
    };
    match only {
        true => image_filters::drop_shadow_only(
            offset, sigma, color, space, input, crop,
        ),
        false => {
            image_filters::drop_shadow(offset, sigma, color, space, input, crop)
        }
    }
}

/// A Skia filter, `T`, and how to build it again for a page, where what it
/// holds depends on the page.
#[derive(Clone)]
pub(crate) struct PageFilter<T> {
    plain: T,
    rebuild: Option<Rc<Rebuild<T>>>,
}

/// What builds a filter for a page, or with none named -- see
/// [`PageFilter::built`].
type Build<T> = Box<dyn Fn(Option<&Onto>) -> Option<T>>;

/// How a [`PageFilter`] builds itself for a page, and the one it built
/// last for each kind of recording, indexed by [`Onto::exact`]: a filter
/// drawn many times onto one page is asked for again, by each of a page's
/// two recordings in turn.
struct Rebuild<T> {
    build: Build<T>,
    held: RefCell<[Option<(Onto, T)>; 2]>,
}

impl<T: Clone> PageFilter<T> {
    /// A filter that draws the same onto every page.
    pub fn plain(filter: T) -> Self {
        Self {
            plain: filter,
            rebuild: None,
        }
    }

    /// The filter `build` makes, given no page, and -- where `depends` --
    /// the filter it makes for each page it is drawn onto. `None` where
    /// Skia declines to build the plain one.
    ///
    /// `build` reaches the filters it is built over through
    /// [`Self::on`], so a filter depends on the page wherever one of them
    /// does.
    pub fn built(
        depends: bool,
        build: impl Fn(Option<&Onto>) -> Option<T> + 'static,
    ) -> Option<Self> {
        let plain = build(None)?;
        let rebuild = depends.then(|| {
            Rc::new(Rebuild {
                build: Box::new(build),
                held: RefCell::new([None, None]),
            })
        });
        Some(Self { plain, rebuild })
    }

    /// Whether what this filter holds depends on the page it is drawn
    /// onto.
    pub fn depends(&self) -> bool {
        self.rebuild.is_some()
    }

    /// The filter as `onto` draws it, or the plain one where no page is
    /// named, nothing it holds depends on the page, or Skia declines to
    /// build it for this one.
    pub fn on(&self, onto: Option<&Onto>) -> T {
        let (Some(rebuild), Some(onto)) = (&self.rebuild, onto) else {
            return self.plain.clone();
        };
        let slot = usize::from(onto.exact);
        if let Some((page, filter)) = &rebuild.held.borrow()[slot]
            && page == onto
        {
            return filter.clone();
        }
        let filter =
            (rebuild.build)(Some(onto)).unwrap_or_else(|| self.plain.clone());
        rebuild.held.borrow_mut()[slot] = Some((onto.clone(), filter.clone()));
        filter
    }
}

/// SkSL for [`exact_blend`]: `colour`, premultiplied and in the working
/// space, blended with what is filtered through `mode`, a child blender --
/// what `SkColorFilters::Blend` does, with the colour as given. A uniform is
/// not converted, where Skia's own filter converts the colour it holds
/// through skcms's approximate transfer function.
const EXACT_BLEND_SKSL: &str = "
uniform float4 colour;
uniform blender mode;
half4 main(half4 dst) { return mode.eval(colour, dst); }
";

thread_local! {
    /// [`EXACT_BLEND_SKSL`], compiled once per thread: a `RuntimeEffect` is
    /// not `Send`.
    static EXACT_BLEND: Option<RuntimeEffect> =
        RuntimeEffect::make_for_color_filter(EXACT_BLEND_SKSL, None).ok();
}

/// A colour filter blending `colour`, unpremultiplied and already in the
/// space the filter runs in, with what it filters through `mode`, as
/// `SkColorFilters::Blend` does with nothing converting the colour: to the
/// bit on the CPU, and on Ganesh within a level of eight bits -- see
/// `context::floats_draw_on_the_cpu`. `None` where the effect does not
/// compile.
pub(crate) fn exact_blend(
    colour: Color4f,
    mode: BlendMode,
) -> Option<ColorFilter> {
    let a = colour.a;
    let uniforms: Vec<u8> = [colour.r * a, colour.g * a, colour.b * a, a]
        .into_iter()
        .flat_map(f32::to_ne_bytes)
        .collect();
    EXACT_BLEND.with(|effect| {
        effect.as_ref()?.make_color_filter(
            Data::new_copy(&uniforms),
            &[ChildPtr::from(Blender::mode(mode))][..],
        )
    })
}

#[cfg(test)]
mod tests {
    use skia_safe::{
        AlphaType, ColorType, ImageInfo, Paint, SamplingOptions, Shader,
        Surface, TileMode, color_filters, shaders::color_in_space,
    };

    use super::*;
    use crate::{context::page::ExportOptions, gpu::RenderingEngine, started};

    /// Every blend mode Skia names.
    const MODES: [BlendMode; 29] = [
        BlendMode::Clear,
        BlendMode::Src,
        BlendMode::Dst,
        BlendMode::SrcOver,
        BlendMode::DstOver,
        BlendMode::SrcIn,
        BlendMode::DstIn,
        BlendMode::SrcOut,
        BlendMode::DstOut,
        BlendMode::SrcATop,
        BlendMode::DstATop,
        BlendMode::Xor,
        BlendMode::Plus,
        BlendMode::Modulate,
        BlendMode::Screen,
        BlendMode::Overlay,
        BlendMode::Darken,
        BlendMode::Lighten,
        BlendMode::ColorDodge,
        BlendMode::ColorBurn,
        BlendMode::HardLight,
        BlendMode::SoftLight,
        BlendMode::Difference,
        BlendMode::Exclusion,
        BlendMode::Multiply,
        BlendMode::Hue,
        BlendMode::Saturation,
        BlendMode::Color,
        BlendMode::Luminosity,
    ];

    /// Colours to blend and to blend with, unpremultiplied sRGB: opaque,
    /// translucent and clear, dark and light.
    const COLOURS: [[f32; 4]; 6] = [
        [0.2, 0.5, 0.8, 1.0],
        [0.9, 0.1, 0.4, 1.0],
        [0.3, 0.7, 0.2, 0.6],
        [0.95, 0.85, 0.05, 0.25],
        [0.05, 0.05, 0.05, 1.0],
        [0.5, 0.5, 0.5, 0.0],
    ];

    /// `dst` as a colour shader, which Skia may fold a colour filter into,
    /// or as an image of one pixel, which it cannot.
    fn source(dst: Color4f, image: bool) -> Shader {
        if !image {
            return color_in_space(dst, ColorSpace::new_srgb());
        }
        let info = ImageInfo::new(
            (1, 1),
            ColorType::RGBAF32,
            AlphaType::Unpremul,
            ColorSpace::new_srgb(),
        );
        let pixel: Vec<u8> = [dst.r, dst.g, dst.b, dst.a]
            .into_iter()
            .flat_map(f32::to_ne_bytes)
            .collect();
        started::raster_from_data(&info, Data::new_copy(&pixel), 16)
            .and_then(|image| {
                image.to_shader(
                    (TileMode::Clamp, TileMode::Clamp),
                    SamplingOptions::default(),
                    None,
                )
            })
            .expect("an image shader")
    }

    /// The pixel `filter` makes of `dst`, drawn through it onto `surface`
    /// from a [`source`], read back as `read`.
    fn filtered(
        surface: &mut Surface,
        read: &ImageInfo,
        dst: Color4f,
        image: bool,
        filter: ColorFilter,
    ) -> Vec<u8> {
        let mut paint = Paint::default();
        paint
            .set_shader(source(dst, image))
            .set_color_filter(filter)
            .set_blend_mode(BlendMode::Src);
        surface.canvas().clear(Color4f::new(0.0, 0.0, 0.0, 0.0));
        surface.canvas().draw_paint(&paint);
        let mut pixel = vec![0u8; read.min_row_bytes()];
        assert!(surface.read_pixels(
            read,
            &mut pixel,
            read.min_row_bytes(),
            (0, 0)
        ));
        pixel
    }

    /// A case where [`exact_blend`] makes a pixel other than Skia's own
    /// blend filter does.
    struct Miss {
        mode: BlendMode,
        /// The largest difference in any channel, in the surface's units.
        by: f32,
        case: String,
    }

    /// The cases where [`exact_blend`] makes a pixel other than Skia's own
    /// blend filter does, on `surface`, in sRGB, where Skia converts an
    /// sRGB colour by the identity; and how many it compared. Skia builds no
    /// filter for a blend that changes nothing -- `dst`, or a clear colour
    /// over -- which the filter holding a colour refuses when it is built,
    /// so those are not compared.
    fn misses(
        surface: &mut Surface,
        read: &ImageInfo,
        image: bool,
    ) -> (Vec<Miss>, usize) {
        let mut compared = 0;
        let colours = || COLOURS.map(|[r, g, b, a]| Color4f::new(r, g, b, a));
        let missed = MODES
            .into_iter()
            .flat_map(|mode| {
                colours().into_iter().flat_map(move |colour| {
                    colours().into_iter().map(move |dst| (mode, colour, dst))
                })
            })
            .filter_map(|(mode, colour, dst)| {
                let skias = color_filters::blend_with_color_space(
                    colour,
                    ColorSpace::new_srgb(),
                    mode,
                )?;
                compared += 1;
                let ours =
                    exact_blend(colour, mode).expect("the effect compiles");
                let want = filtered(surface, read, dst, image, skias);
                let got = filtered(surface, read, dst, image, ours);
                (got != want).then(|| Miss {
                    mode,
                    by: got
                        .iter()
                        .zip(&want)
                        .map(|(got, want)| got.abs_diff(*want) as f32)
                        .fold(0.0, f32::max),
                    case: format!(
                        "{mode:?} {colour:?} over {dst:?}: {got:?} for {want:?}"
                    ),
                })
            })
            .collect::<Vec<_>>();
        (missed, compared)
    }

    /// On the CPU, in floats, the runtime filter blends every mode to the
    /// bit as Skia's own filter does, over a flat colour Skia could fold the
    /// filter into and over an image it cannot.
    #[test]
    fn the_exact_blend_is_skia_s_blend_on_the_cpu() {
        let info = ImageInfo::new(
            (1, 1),
            ColorType::RGBAF32,
            AlphaType::Premul,
            ColorSpace::new_srgb(),
        );
        let mut surface =
            started::raster(&info, None, None).expect("a surface");
        for image in [true, false] {
            let (missed, compared) = misses(&mut surface, &info, image);
            assert!(compared > MODES.len() * COLOURS.len(), "{compared}");
            assert!(
                missed.is_empty(),
                "image {image}: {} missed, first {}",
                missed.len(),
                missed[0].case
            );
        }
    }

    /// The modes Skia blends by hue, saturation and luminosity rather than
    /// channel by channel.
    const NON_SEPARABLE: [BlendMode; 4] = [
        BlendMode::Hue,
        BlendMode::Saturation,
        BlendMode::Color,
        BlendMode::Luminosity,
    ];

    /// On the GPU, where there is one, what the runtime filter is: it
    /// compiles, and on eight bits a channel blends an image as Skia's own
    /// filter does byte for byte in every channel-by-channel mode, and
    /// within a level otherwise. Ganesh carries a fragment's colour at half
    /// precision: Skia folds its own filter into a flat colour on the CPU
    /// and rounds the result once, and the runtime filter, which it does
    /// not fold, lands a level off there -- a copy of 0.9 at 229 where
    /// Skia's is 230 -- and the non-separable modes part by a level on an
    /// image too. Why no float page the GPU draws takes this filter -- see
    /// `context::floats_draw_on_the_cpu`.
    #[test]
    fn the_exact_blend_is_within_a_level_of_skia_s_on_the_gpu() {
        let info = ImageInfo::new(
            (1, 1),
            ColorType::RGBA8888,
            AlphaType::Premul,
            ColorSpace::new_srgb(),
        );
        let gpu = RenderingEngine::GPU;
        let Some(mut surface) = gpu
            .selectable()
            .then(|| gpu.make_surface(&info, &ExportOptions::default()).ok())
            .flatten()
        else {
            eprintln!("no GPU here: the GPU half is not run");
            return;
        };
        for image in [true, false] {
            let (missed, compared) = misses(&mut surface, &info, image);
            assert!(compared > MODES.len() * COLOURS.len(), "{compared}");
            let beyond: Vec<_> = missed
                .iter()
                .filter(|miss| {
                    miss.by > 1.0
                        || (image && !NON_SEPARABLE.contains(&miss.mode))
                })
                .map(|miss| &miss.case)
                .collect();
            assert!(
                beyond.is_empty(),
                "image {image}: {} beyond, first {}",
                beyond.len(),
                beyond[0]
            );
        }
    }
}
