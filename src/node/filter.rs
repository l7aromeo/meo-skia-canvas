#![allow(unused_mut)]
#![allow(unused_imports)]
#![allow(unused_variables)]
#![allow(dead_code)]
use skia_safe::{
    BlurStyle, Color, Color4f, ColorSpace, CubicResampler, FilterMode,
    ImageFilter as SkImageFilter, MaskFilter, Matrix, MipmapMode, Paint, Point,
    SamplingOptions, TileMode, color_filters, image_filters,
    table_color_filter,
};
use std::fmt;

use crate::{export::VectorFeatures, utils::*};

/// The luminance coefficients CSS Filter Effects Level 1 uses for
/// `grayscale()` and `saturate()`.
///
/// Rec. 709's, which is what sRGB is defined against. The specification
/// writes them to four decimals in those two matrices and to three in the
/// `hue-rotate()` one -- see [`LUMA_ROUNDED`] -- so both spellings appear
/// here on purpose. Unifying them would make this crate disagree with the
/// specification, and with every browser, in whichever filter lost.
///
/// Visible to [`color_filter`](crate::node::color_filter) because Skia's
/// luma colour filter is the same three numbers doing the same job: the
/// coefficients are Rec. 709's, not CSS's, and CSS is only where this crate
/// happened to need them first.
pub(crate) const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

/// The same coefficients as the specification rounds them in its
/// `hue-rotate()` matrix.
///
/// Three decimals rather than four. Not a typo on anyone's part: the
/// hue-rotate matrix is tabulated that way in Filter Effects Level 1, and
/// browsers implement what is tabulated.
const LUMA_ROUNDED: [f32; 3] = [0.213, 0.715, 0.072];

/// The sine half of the `hue-rotate()` matrix from Filter Effects Level 1.
///
/// The cosine half is derivable from [`LUMA_ROUNDED`] -- it is the identity
/// minus the luminance matrix -- but this one is not: the middle row comes
/// out of the YIQ conversion hue-rotate is defined through, and 0.143, 0.140
/// and 0.283 are not a function of the luma coefficients or of each other.
/// So they are tabulated here as the specification tabulates them.
const HUE_ROTATE_SIN: [[f32; 3]; 3] = [
    [-0.213, -0.715, 0.928],
    [0.143, 0.140, -0.283],
    [-0.787, 0.715, 0.072],
];

/// The `sepia()` matrix from CSS Filter Effects Level 1, at full strength.
///
/// One row per output channel. Unlike [`LUMA`] the three rows differ, which
/// is what makes sepia a tint rather than a desaturation.
const SEPIA: [[f32; 3]; 3] = [
    [0.393, 0.769, 0.189],
    [0.349, 0.686, 0.168],
    [0.272, 0.534, 0.131],
];

/// The value `contrast()` pivots around, on the 0-255 ramp it is applied to.
///
/// Filter Effects defines contrast as a linear transfer with slope `amount`
/// and intercept `0.5 - 0.5 * amount`, on channels normalised to 0..1.
/// Scaled to a byte that intercept is `127.5 * (1 - amount)`, so the pivot
/// is 127.5 and not 127 -- the midpoint of 0..255 falls between two
/// integers, because there are 256 of them.
///
/// It was 127.0, which is the same shape of mistake as the BMP colour space
/// that spelled `sWin`: a specification value transcribed a little wrong and
/// invisible in any single picture. Measured over the 256-entry ramp it
/// builds, `contrast(2)` moved half the entries a level and `contrast(5)`
/// fifty-one of them by up to two. Some amounts were unaffected, because the
/// half level does not always survive the truncation -- which is also why
/// the truncation stays: Chrome returns 127 for `contrast(0)`, and rounding
/// 127.5 would give 128.
const CONTRAST_PIVOT: f32 = 127.5;

/// A colour matrix that fades from `rows` at `amount` 0 to the identity at
/// `amount` 1.
///
/// The shape `grayscale()`, `saturate()` and `sepia()` all have: each
/// diagonal entry moves toward 1 and each off-diagonal toward 0, in step.
/// It was written out three times as sixty literals, of which forty-two
/// were `1 - x` for an `x` on the same line -- so the matrices could drift
/// from the coefficients they were built from without a compiler or a
/// reader noticing.
fn hue_rotated(cos: f32, sin: f32) -> [f32; 20] {
    let mut matrix = [0.0f32; 20];
    for r in 0..3 {
        for c in 0..3 {
            let luma = LUMA_ROUNDED[c];
            // The cosine term is the identity minus the luminance matrix,
            // which is why only its sign changes off the diagonal.
            let cosine = match r == c {
                true => 1.0 - luma,
                false => -luma,
            };
            matrix[r * 5 + c] =
                luma + cos * cosine + sin * HUE_ROTATE_SIN[r][c];
        }
    }
    matrix[18] = 1.0;
    matrix
}

fn faded_toward_identity(rows: &[[f32; 3]; 3], amount: f32) -> [f32; 20] {
    let mut matrix = [0.0f32; 20];
    for (r, row) in rows.iter().enumerate() {
        for (c, coefficient) in row.iter().enumerate() {
            matrix[r * 5 + c] = match r == c {
                true => coefficient + (1.0 - coefficient) * amount,
                false => coefficient * (1.0 - amount),
            };
        }
    }
    // The alpha row is untouched by all three filters.
    matrix[18] = 1.0;
    matrix
}

/// The largest blur, as a standard deviation in device pixels, that a
/// rectangle's coverage blur agrees with Chrome as closely as the image
/// blur does.
///
/// Measured against Chrome 152 on a blurred square at 100, 200 and 400
/// pixels a side, along its middle row: up to 40 the coverage blur is
/// within 5 levels of Chrome's alpha and no further off than the image
/// blur, which strays as far at 20 and 40 itself; from 50 the image blur
/// is within a level and the coverage blur up to 5 off -- and past
/// Skia's own cap, [`MASK_BLUR_MAX_SIGMA`], it stops spreading at all.
const RECT_COVERAGE_BLUR_MAX_SIGMA: f32 = 40.0;

/// The largest blur a rounded rectangle's or an oval's coverage blur is
/// cheaper at than the image blur: 11 and 58 microseconds a draw at a
/// sigma of 2 against 156 and 166, about the same as the image blur from
/// 20, in a release build. A rectangle's stays nearly free at every sigma
/// measured, Skia caching its blurred mask.
const ROUNDED_COVERAGE_BLUR_MAX_SIGMA: f32 = 10.0;

/// The largest sigma Skia's coverage blur takes: `SkBlurMaskFilterImpl::
/// computeXformedSigma` holds it to `kMaxBlurSigma`, 128, so a larger one
/// draws exactly as 128 -- `blur(1000px)` on a 100 pixel square kept an
/// alpha of 9 where Chrome's is gone.
pub(crate) const MASK_BLUR_MAX_SIGMA: f32 = 128.0;

// Every coverage blur stays under Skia's cap, so none draws as a smaller
// blur than it names.
const _: () = assert!(
    RECT_COVERAGE_BLUR_MAX_SIGMA <= MASK_BLUR_MAX_SIGMA
        && ROUNDED_COVERAGE_BLUR_MAX_SIGMA <= MASK_BLUR_MAX_SIGMA
);

/// The largest side, in device pixels, of a glyph Skia keeps as a cached
/// mask: `SkGlyph::kSkSideTooBigForAtlas`. A glyph with a coverage blur is
/// as large as its outline plus the blur's window, and one past this is
/// drawn as a path and blurred again on every draw -- 48px text cost 22
/// microseconds a draw at a sigma of 38 and 2,537 at 42.
const GLYPH_ATLAS_SIDE: f32 = 256.0;

/// How many sigmas wide a coverage blur's window is: `SkBlurMask` sizes it
/// `ceil(6 * sigma)`, three to each side.
const BLUR_WINDOW_SIGMAS: f32 = 6.0;

/// What a draw covers, as far as choosing how to blur it goes.
#[derive(Clone, Copy, Debug)]
pub enum Coverage {
    /// A rectangle that stays one on the device.
    Rect,
    /// A rounded rectangle or an oval that stays one on the device.
    Rounded,
    /// Text set at this many device pixels to the em.
    Text {
        /// The em, in device pixels.
        em: f32,
    },
    /// Any other geometry.
    Path,
}

impl Coverage {
    /// Whether a blur of `sigma` device pixels is better made on this
    /// coverage than on the drawn result: where it matches Chrome as
    /// closely and costs less. Other paths are blurred as a result at
    /// every sigma: a circle took 145 microseconds a draw through its
    /// coverage at a sigma of 2 against 82, and a star no less.
    pub(crate) fn blurs_alike(self, sigma: f32) -> bool {
        match self {
            Coverage::Rect => sigma <= RECT_COVERAGE_BLUR_MAX_SIGMA,
            Coverage::Rounded => sigma <= ROUNDED_COVERAGE_BLUR_MAX_SIGMA,
            Coverage::Text { em } => {
                sigma <= RECT_COVERAGE_BLUR_MAX_SIGMA
                    && em + BLUR_WINDOW_SIGMAS * sigma <= GLYPH_ATLAS_SIDE
            }
            Coverage::Path => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum FilterSpec {
    Plain {
        name: String,
        value: f32,
    },
    Shadow {
        offset: Point,
        blur: f32,
        /// Unpremultiplied, in the space it is tagged with.
        color: (Color4f, ColorSpace),
    },
}

#[derive(Clone, Debug)]
pub struct Filter {
    pub css: String,
    specs: Vec<FilterSpec>,
    _raster: Option<LastFilter>,
    _vector: Option<LastFilter>,
}

#[derive(Clone, Debug)]
pub struct LastFilter {
    matrix: Matrix,
    mask: Option<MaskFilter>,
    image: Option<SkImageFilter>,
}

impl LastFilter {
    fn match_scale(&self, matrix: Matrix) -> Option<Self> {
        if self.matrix.scale_x() == matrix.scale_x()
            && self.matrix.scale_y() == matrix.scale_y()
        {
            Some(self.clone())
        } else {
            None
        }
    }
}

impl Default for Filter {
    fn default() -> Self {
        Filter {
            css: "none".to_string(),
            specs: vec![],
            _raster: None,
            _vector: None,
        }
    }
}

impl fmt::Display for Filter {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.css)
    }
}

impl Filter {
    /// Whether this filter is `css` resolved to `specs`: the same text, and
    /// the same lengths. Text alone does not say -- `blur(1em)` is a
    /// different blur under a different font -- so both are compared.
    pub fn is(&self, css: &str, specs: &[FilterSpec]) -> bool {
        self.css == css && self.specs == specs
    }

    /// Whether this filter would leave a draw untouched.
    pub fn is_none(&self) -> bool {
        self.specs.is_empty()
    }

    /// The standard deviation, in canvas pixels, of a filter that is one
    /// `blur()` and nothing else; `None` for any other filter.
    pub fn sole_blur(&self) -> Option<f32> {
        match self.specs.as_slice() {
            [FilterSpec::Plain { name, value }] if name == "blur" => {
                Some(*value)
            }
            _ => None,
        }
    }

    pub fn new(css: &str, specs: &[FilterSpec]) -> Self {
        let css = css.to_string();
        let specs = specs.to_vec();
        Filter {
            css,
            specs,
            _raster: None,
            _vector: None,
        }
    }

    /// Whether every blur in this filter is better made on the coverage of
    /// a draw like `coverage` than on its result. True where there is no
    /// blur, since nothing else here differs between the two.
    pub fn blurs_coverage(&self, coverage: Coverage) -> bool {
        self.specs.iter().all(|spec| match spec {
            FilterSpec::Plain { name, value } if name == "blur" => {
                coverage.blurs_alike(*value)
            }
            _ => true,
        })
    }

    /// What a flat-colour draw of `coverage` blurred by this filter asks of
    /// a vector backend: the mask filter where [`Self::mix_into`] blurs its
    /// coverage, the image filter where it blurs its result. As
    /// `Context2D::vector_features` reads off the paint `mix_into` makes,
    /// for a draw that never makes it.
    pub(crate) fn blur_features(&self, coverage: Coverage) -> VectorFeatures {
        match self.blurs_coverage(coverage) {
            true => VectorFeatures::MASK_FILTER,
            false => VectorFeatures::IMAGE_FILTER,
        }
    }

    /// Puts this filter onto `paint`.
    ///
    /// `rendered` asks for the draw's result to be blurred rather than its
    /// coverage. True for anything that paints a bitmap, and for geometry
    /// whose paint carries a shader -- see [`Self::filters_for`].
    pub fn mix_into<'a>(
        &mut self,
        paint: &'a mut Paint,
        matrix: Matrix,
        rendered: bool,
    ) -> &'a mut Paint {
        let filters = self.filters_for(matrix, rendered);
        paint
            .set_image_filter(filters.image)
            .set_mask_filter(filters.mask)
    }

    /// The filters for this draw, built once per transform scale and kept.
    ///
    /// A blur becomes one of two different Skia objects. Blurring the drawn
    /// result needs an image filter; blurring a shape's coverage is a mask
    /// filter. The two approximate a Gaussian differently, so they are
    /// close rather than the same picture even for one flat colour, and the
    /// mask filter is held to a sigma of 128; which of them a flat-colour
    /// draw gets is `Coverage`'s to say, from what each costs and how near
    /// each comes to Chrome. The image filter is the one that reaches
    /// Chrome's blur of a large radius.
    ///
    /// The coverage blur stops being even a near picture the moment the
    /// paint has structure.
    /// A coverage blur never touches the paint, so the stripes inside a
    /// blurred pattern came out byte-identical to no blur, and a shader that
    /// painted nothing outside its own rectangle had nothing to spread, so
    /// the fill kept a hard edge at any radius. Both are what `rendered`
    /// exists to route around.
    fn filters_for(&mut self, matrix: Matrix, rendered: bool) -> LastFilter {
        let cached = match (rendered, &self._raster, &self._vector) {
            (true, Some(cached), _) | (false, _, Some(cached)) => {
                cached.match_scale(matrix)
            }
            _ => None,
        };

        cached
            .or_else(|| {
                let mut mask_filter = None;
                let image_filter =
                    self.specs.iter().fold(None, |chain, next_filter| {
                        match next_filter {
                            FilterSpec::Shadow {
                                offset,
                                blur,
                                color,
                            } => {
                                let scale = Point {
                                    x: matrix.scale_x(),
                                    y: matrix.scale_y(),
                                };
                                let point =
                                    (offset.x / scale.x, offset.y / scale.y);
                                let sigma = (blur / scale.x, blur / scale.y);
                                let (color, space) =
                                    skia_paint_color(color.0, &color.1);
                                image_filters::drop_shadow(
                                    point, sigma, color, space, chain, None,
                                )
                            }
                            FilterSpec::Plain { name, value } => match name
                                .as_ref()
                            {
                                "blur" => {
                                    if rendered {
                                        // `blur(<length>)` gives the standard
                                        // deviation directly -- Filter Effects
                                        // says so, and the geometry branch
                                        // below hands the same length to a
                                        // mask filter as its sigma. Halving it
                                        // here is the `box-shadow` convention,
                                        // where the radius is 2 sigma; that
                                        // belongs to `shadowBlur` alone, and
                                        // `Context2D::paint_for_shadow` still
                                        // applies it. An image drew at half
                                        // the radius it was asked for.
                                        //
                                        // Divided by the scale so the sigma
                                        // is the length in device pixels
                                        // under the recording's transform,
                                        // as the mask filter's is below, and
                                        // scales with any transform the
                                        // draw is replayed under.
                                        let sigma_x = value / matrix.scale_x();
                                        let sigma_y = value / matrix.scale_y();
                                        image_filters::blur(
                                            (sigma_x, sigma_y),
                                            TileMode::Decal,
                                            chain,
                                            None,
                                        )
                                    } else {
                                        // The same length in local units,
                                        // with `respect_ctm` set, so that
                                        // Skia maps it back through the
                                        // transform the draw is replayed
                                        // under: the recording's own, or
                                        // that scaled by an export density
                                        // or by a canvas drawn into another
                                        // at a size. A sigma fixed in device
                                        // pixels stayed the width it was at
                                        // density 1 at every density. A
                                        // transform that maps no length to
                                        // a positive one draws nothing, and
                                        // keeps the device sigma.
                                        let scale =
                                            matrix.map_radius(1.0).filter(
                                                |s| s.is_normal() && *s > 0.0,
                                            );
                                        mask_filter = match scale {
                                            Some(scale) => MaskFilter::blur(
                                                BlurStyle::Normal,
                                                value / scale,
                                                true,
                                            ),
                                            None => MaskFilter::blur(
                                                BlurStyle::Normal,
                                                *value,
                                                false,
                                            ),
                                        };
                                        chain
                                    }
                                }

                                //
                                // matrices and formulæ taken from: https://www.w3.org/TR/filter-effects-1/
                                "brightness" => {
                                    let amt = value.max(0.0);
                                    let color_matrix =
                                        color_filters::matrix_row_major(
                                            &[
                                                amt, 0.0, 0.0, 0.0, 0.0, 0.0,
                                                amt, 0.0, 0.0, 0.0, 0.0, 0.0,
                                                amt, 0.0, 0.0, 0.0, 0.0, 0.0,
                                                1.0, 0.0,
                                            ],
                                            None,
                                        );
                                    image_filters::color_filter(
                                        color_matrix,
                                        chain,
                                        None,
                                    )
                                }
                                "contrast" => {
                                    let amt = value.max(0.0);
                                    let mut ramp = [0u8; 256];
                                    for (i, val) in
                                        ramp.iter_mut().take(256).enumerate()
                                    {
                                        let orig = i as f32;
                                        // Rounded, not truncated: this
                                        // builds a lookup table, and
                                        // truncating biases every entry
                                        // down by up to a level on top of
                                        // whatever the pivot does.
                                        // Truncated, not rounded, which is
                                        // what a browser does: Chrome
                                        // returns 127 for `contrast(0)`,
                                        // and a rounded 127.5 would be 128.
                                        // Rounding looked like the obvious
                                        // improvement to make alongside the
                                        // pivot; measured against Chrome it
                                        // moved every entry of the ramp a
                                        // level away from the answer.
                                        *val = (CONTRAST_PIVOT + amt * orig
                                            - CONTRAST_PIVOT * amt)
                                            .clamp(0.0, 255.0)
                                            as u8;
                                    }
                                    let table = Some(&ramp);
                                    if let Some(color_table) =
                                        color_filters::table_argb(
                                            None, table, table, table,
                                        )
                                    {
                                        image_filters::color_filter(
                                            color_table,
                                            chain,
                                            None,
                                        )
                                    } else {
                                        chain
                                    }
                                }
                                "grayscale" => {
                                    let amt = 1.0 - value.clamp(0.0, 1.0);
                                    let color_matrix =
                                        color_filters::matrix_row_major(
                                            &faded_toward_identity(
                                                &[LUMA, LUMA, LUMA],
                                                amt,
                                            ),
                                            None,
                                        );
                                    image_filters::color_filter(
                                        color_matrix,
                                        chain,
                                        None,
                                    )
                                }
                                "invert" => {
                                    let amt = value.clamp(0.0, 1.0);
                                    let mut ramp = [0u8; 256];
                                    for (i, val) in ramp
                                        .iter_mut()
                                        .take(256)
                                        .enumerate()
                                        .map(|(i, v)| (i as f32, v))
                                    {
                                        let (orig, inv) = (i, 255.0 - i);
                                        *val = (orig * (1.0 - amt) + inv * amt)
                                            as u8;
                                    }
                                    let table = Some(&ramp);
                                    if let Some(color_table) =
                                        color_filters::table_argb(
                                            None, table, table, table,
                                        )
                                    {
                                        image_filters::color_filter(
                                            color_table,
                                            chain,
                                            None,
                                        )
                                    } else {
                                        chain
                                    }
                                }
                                "opacity" => {
                                    let amt = value.clamp(0.0, 1.0);
                                    let color_matrix =
                                        color_filters::matrix_row_major(
                                            &[
                                                1.0, 0.0, 0.0, 0.0, 0.0, 0.0,
                                                1.0, 0.0, 0.0, 0.0, 0.0, 0.0,
                                                1.0, 0.0, 0.0, 0.0, 0.0, 0.0,
                                                amt, 0.0,
                                            ],
                                            None,
                                        );
                                    image_filters::color_filter(
                                        color_matrix,
                                        chain,
                                        None,
                                    )
                                }
                                "saturate" => {
                                    let amt = value.max(0.0);
                                    let color_matrix =
                                        color_filters::matrix_row_major(
                                            &faded_toward_identity(
                                                &[LUMA, LUMA, LUMA],
                                                amt,
                                            ),
                                            None,
                                        );
                                    image_filters::color_filter(
                                        color_matrix,
                                        chain,
                                        None,
                                    )
                                }
                                "sepia" => {
                                    let amt = 1.0 - value.clamp(0.0, 1.0);
                                    let color_matrix =
                                        color_filters::matrix_row_major(
                                            &faded_toward_identity(&SEPIA, amt),
                                            None,
                                        );
                                    image_filters::color_filter(
                                        color_matrix,
                                        chain,
                                        None,
                                    )
                                }
                                "hue-rotate" => {
                                    let cos = value.to_radians().cos();
                                    let sin = value.to_radians().sin();
                                    let color_matrix =
                                        color_filters::matrix_row_major(
                                            &hue_rotated(cos, sin),
                                            None,
                                        );
                                    image_filters::color_filter(
                                        color_matrix,
                                        chain,
                                        None,
                                    )
                                }
                                _ => chain,
                            },
                        }
                    });

                let filters = Some(LastFilter {
                    matrix,
                    mask: mask_filter,
                    image: image_filter,
                });
                if rendered {
                    self._raster = filters.clone();
                } else {
                    self._vector = filters.clone();
                }
                filters
            })
            // SAFETY: The `or_else` closure always returns `Some(LastFilter {
            // ... })`.
            .expect("Could not create filter")
    }
}

#[derive(Copy, Clone)]
pub enum SamplingQuality {
    None,
    Low,
    Medium,
    High,
}

#[derive(Copy, Clone)]
pub struct SamplingFilter {
    pub smoothing: bool,
    pub quality: SamplingQuality,
}

/// Which way a draw scales its source, mirroring Chrome's
/// `cc::PaintFlags::ScalingOperation`. Only `High` consults it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalingOperation {
    /// Chrome's "legacy behavior" arm, reached only through its one-argument
    /// overload. Its image path never produces this, and neither do we.
    Default,
    /// Anything that is not a strict upscale, including every minification.
    Unknown,
    /// Scaling up on both axes.
    Upscale,
}

impl ScalingOperation {
    /// Port of `MatrixToScalingOperation` in Chrome's `cc/paint/paint_op.cc`:
    ///
    /// ```text
    /// SkSize scale;
    /// if (m.decomposeScale(&scale)) {
    ///   return (scale.width() > 1 && scale.height() > 1) ? kUpscale : kUnknown;
    /// }
    /// return kUnknown;
    /// ```
    ///
    /// `matrix` is the whole local-to-device transform, so the canvas's own
    /// transform counts: a 2x drawImage under a 0.25x CTM is a minification.
    pub fn for_matrix(matrix: &Matrix) -> Self {
        match matrix.decompose_scale(None) {
            Some(scale) if scale.width > 1.0 && scale.height > 1.0 => {
                Self::Upscale
            }
            _ => Self::Unknown,
        }
    }
}

impl SamplingFilter {
    pub fn sampling(&self) -> SamplingOptions {
        self.sampling_for(ScalingOperation::Default)
    }

    /// Returns the sampling options for a quality level and scaling
    /// direction.
    ///
    /// `None`, `Low` and `Medium` map straight onto Skia's filter/mipmap
    /// pairs and are deliberately left alone. `High` follows Chrome, which
    /// with Safari is the only engine implementing `imageSmoothingQuality`
    /// at all -- Firefox has no such property, and the HTML spec declines
    /// to mandate an algorithm.
    ///
    /// From `cc/paint/paint_flags.cc`:
    ///
    /// ```text
    /// kHigh + kDefault  -> SkCubicResampler::CatmullRom()
    /// kHigh + kUnknown  -> (kLinear, kMipmapLinear)
    /// kHigh + kUpscale  -> SkCubicResampler::Mitchell()
    /// ```
    ///
    /// The split by scaling direction matters, and a cubic resampler for
    /// every case is wrong twice over. It matches no engine, so output
    /// diverges from every browser. And a cubic sets `use_cubic`, after
    /// which Skia ignores the mipmap chain, so heavy minification aliases
    /// badly -- which is why that case stays on trilinear.
    pub fn sampling_for(&self, op: ScalingOperation) -> SamplingOptions {
        let quality = if self.smoothing {
            self.quality
        } else {
            SamplingQuality::None
        };
        match quality {
            SamplingQuality::None => {
                SamplingOptions::new(FilterMode::Nearest, MipmapMode::None)
            }
            SamplingQuality::Low => {
                SamplingOptions::new(FilterMode::Linear, MipmapMode::Nearest)
            }
            SamplingQuality::Medium => {
                SamplingOptions::new(FilterMode::Linear, MipmapMode::Linear)
            }
            SamplingQuality::High => match op {
                ScalingOperation::Default => {
                    SamplingOptions::from(CubicResampler::catmull_rom())
                }
                ScalingOperation::Unknown => {
                    SamplingOptions::new(FilterMode::Linear, MipmapMode::Linear)
                }
                ScalingOperation::Upscale => {
                    SamplingOptions::from(CubicResampler::mitchell())
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compares against the literals these helpers replaced, which is the
    /// only check worth making: a derivation that is elegant and wrong is
    /// worse than the sixty numbers it removed.
    fn close(got: &[f32; 20], want: &[f32; 20], what: &str) {
        for (i, (g, w)) in got.iter().zip(want).enumerate() {
            assert!((g - w).abs() < 1e-6, "{what}: entry {i} is {g}, was {w}");
        }
    }

    /// Mitchell-Netravali's own recommended parameters, from the 1988 paper
    /// that names the family: B = C = 1/3, the pair they single out as
    /// subjectively best of the whole `(B, C)` plane. Written out rather than
    /// taken from `CubicResampler::mitchell()`, because comparing that helper
    /// against itself asserts nothing and what this pins is the *choice*.
    #[test]
    fn the_upscaling_cubic_is_mitchell() {
        let filter = SamplingFilter {
            quality: SamplingQuality::High,
            smoothing: true,
        };
        let options = filter.sampling_for(ScalingOperation::Upscale);

        assert!(options.use_cubic, "a strict upscale takes a cubic");
        assert!(
            (options.cubic.b - 1.0 / 3.0).abs() < 1e-6
                && (options.cubic.c - 1.0 / 3.0).abs() < 1e-6,
            "B and C are Mitchell's, got ({}, {}) -- CatmullRom is (0, 0.5)",
            options.cubic.b,
            options.cubic.c
        );
    }

    /// The minifying arm must not acquire one. A cubic sets `use_cubic` and
    /// Skia then ignores the mipmap chain, which is the whole reason this
    /// mapping splits by scaling direction.
    #[test]
    fn the_minifying_arm_takes_no_cubic() {
        let filter = SamplingFilter {
            quality: SamplingQuality::High,
            smoothing: true,
        };
        assert!(
            !filter.sampling_for(ScalingOperation::Unknown).use_cubic,
            "anything that is not a strict upscale stays mipmapped"
        );
    }

    #[test]
    fn the_luma_matrix_is_the_one_that_was_written_out() {
        for amt in [0.0f32, 0.25, 0.5, 1.0, 2.0] {
            let want = [
                0.2126 + 0.7874 * amt,
                0.7152 - 0.7152 * amt,
                0.0722 - 0.0722 * amt,
                0.0,
                0.0,
                0.2126 - 0.2126 * amt,
                0.7152 + 0.2848 * amt,
                0.0722 - 0.0722 * amt,
                0.0,
                0.0,
                0.2126 - 0.2126 * amt,
                0.7152 - 0.7152 * amt,
                0.0722 + 0.9278 * amt,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                1.0,
                0.0,
            ];
            close(
                &faded_toward_identity(&[LUMA, LUMA, LUMA], amt),
                &want,
                &format!("grayscale/saturate at {amt}"),
            );
        }
    }

    #[test]
    fn the_sepia_matrix_is_the_one_that_was_written_out() {
        for amt in [0.0f32, 0.25, 0.5, 1.0] {
            let want = [
                0.393 + 0.607 * amt,
                0.769 - 0.769 * amt,
                0.189 - 0.189 * amt,
                0.0,
                0.0,
                0.349 - 0.349 * amt,
                0.686 + 0.314 * amt,
                0.168 - 0.168 * amt,
                0.0,
                0.0,
                0.272 - 0.272 * amt,
                0.534 - 0.534 * amt,
                0.131 + 0.869 * amt,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                1.0,
                0.0,
            ];
            close(
                &faded_toward_identity(&SEPIA, amt),
                &want,
                &format!("sepia at {amt}"),
            );
        }
    }

    #[test]
    fn the_hue_rotate_matrix_is_the_one_that_was_written_out() {
        for degrees in [0.0f32, 45.0, 90.0, 180.0, 270.0] {
            let (cos, sin) =
                (degrees.to_radians().cos(), degrees.to_radians().sin());
            let want = [
                0.213 + cos * 0.787 - sin * 0.213,
                0.715 - cos * 0.715 - sin * 0.715,
                0.072 - cos * 0.072 + sin * 0.928,
                0.0,
                0.0,
                0.213 - cos * 0.213 + sin * 0.143,
                0.715 + cos * 0.285 + sin * 0.140,
                0.072 - cos * 0.072 - sin * 0.283,
                0.0,
                0.0,
                0.213 - cos * 0.213 - sin * 0.787,
                0.715 - cos * 0.715 + sin * 0.715,
                0.072 + cos * 0.928 + sin * 0.072,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                1.0,
                0.0,
            ];
            close(&hue_rotated(cos, sin), &want, &format!("{degrees} deg"));
        }
    }

    #[test]
    fn the_identity_end_of_each_filter_is_the_identity() {
        // What the shared shape is for: at full amount every one of these
        // leaves the pixels alone, and a matrix that did not would tint an
        // image nobody asked to filter.
        let identity = {
            let mut m = [0.0f32; 20];
            m[0] = 1.0;
            m[6] = 1.0;
            m[12] = 1.0;
            m[18] = 1.0;
            m
        };
        close(
            &faded_toward_identity(&[LUMA, LUMA, LUMA], 1.0),
            &identity,
            "saturate(1)",
        );
        close(&faded_toward_identity(&SEPIA, 1.0), &identity, "sepia(0)");
        close(&hue_rotated(1.0, 0.0), &identity, "hue-rotate(0deg)");
    }

    #[test]
    fn contrast_pivots_where_a_browser_pivots() {
        // Two questions, and browsers answer them differently from each
        // other's obvious reading.
        //
        // The pivot is 127.5, not 127. Filter Effects defines contrast as a
        // linear transfer with slope `amount` and intercept
        // `0.5 - 0.5 * amount` on channels normalised to 0..1, which on a
        // byte scales to `127.5 * (1 - amount)`. This crate had 127.0 --
        // the same shape of mistake as the BMP header that spelled `sWin`,
        // a specification value transcribed slightly wrong and invisible in
        // any one picture.
        //
        // The result is truncated, not rounded. That is the part worth
        // measuring rather than reasoning about: rounding is the obvious
        // thing to pair with a .5 pivot, and it is wrong. Chrome returns
        // 127 for `contrast(0)`, and a rounded 127.5 is 128.
        let ramp = |pivot: f32, amount: f32| {
            (0..256)
                .map(|i| {
                    (pivot + amount * i as f32 - pivot * amount)
                        .clamp(0.0, 255.0) as u8
                })
                .collect::<Vec<_>>()
        };

        // Taken from Chrome, filtering a flat rgb(64,128,192) fill. The
        // three channels are three samples of the same ramp.
        for (amount, red, green, blue) in [
            (0.0f32, 127u8, 127, 127),
            (0.5, 95, 127, 159),
            (1.0, 64, 128, 192),
            (1.01, 63, 128, 192),
            (2.0, 0, 128, 255),
        ] {
            let table = ramp(CONTRAST_PIVOT, amount);
            assert_eq!(
                (table[64], table[128], table[192]),
                (red, green, blue),
                "contrast({amount}) against Chrome"
            );
        }

        // Amount 1 is the identity, and is the one amount at which the old
        // pivot happened to agree -- which is why nothing caught it.
        assert!(
            ramp(CONTRAST_PIVOT, 1.0)
                .iter()
                .enumerate()
                .all(|(i, v)| *v as usize == i)
        );
        assert_eq!(ramp(127.0, 1.0), ramp(CONTRAST_PIVOT, 1.0));

        // Elsewhere it did not, though not uniformly: the half level only
        // survives truncation at some amounts, which is why this is
        // measured rather than asserted in the abstract. `contrast(0.5)`
        // happens to land on the same 256 bytes; `contrast(2)` moves half
        // of them and `contrast(5)` fifty-one by up to two.
        for (amount, moved) in
            [(0.0f32, 0), (0.25, 64), (0.5, 0), (2.0, 128), (5.0, 51)]
        {
            let differing = ramp(127.0, amount)
                .iter()
                .zip(ramp(CONTRAST_PIVOT, amount))
                .filter(|(was, now)| **was != *now)
                .count();
            assert_eq!(differing, moved, "contrast({amount})");
        }
    }

    #[test]
    fn a_rectangle_takes_the_coverage_blur_up_to_its_limit() {
        assert!(Coverage::Rect.blurs_alike(RECT_COVERAGE_BLUR_MAX_SIGMA));
        assert!(
            !Coverage::Rect.blurs_alike(RECT_COVERAGE_BLUR_MAX_SIGMA + 1.0)
        );
        assert!(Coverage::Rounded.blurs_alike(ROUNDED_COVERAGE_BLUR_MAX_SIGMA));
        assert!(
            !Coverage::Rounded
                .blurs_alike(ROUNDED_COVERAGE_BLUR_MAX_SIGMA + 1.0)
        );
        assert!(!Coverage::Path.blurs_alike(1.0));
    }

    /// The switch for text falls before the cliff measured at each size --
    /// a blurred glyph costs about 20 microseconds a draw below it and
    /// over 1,000 past it -- since the em stands in for the glyph's
    /// largest side, which is smaller: 48px text measured cheap at a sigma
    /// of 38 and dear at 40, and switches at 34.
    #[test]
    fn text_takes_the_coverage_blur_while_its_glyph_fits_the_cache() {
        for (em, cheap_at, dear_at) in
            [(24.0, 42.0, 45.0), (48.0, 38.0, 40.0), (96.0, 30.0, 35.0)]
        {
            let text = Coverage::Text { em };
            assert!(!text.blurs_alike(dear_at), "{em}px at {dear_at}");
            let switch = (GLYPH_ATLAS_SIDE - em) / BLUR_WINDOW_SIGMAS;
            assert!(switch <= cheap_at, "{em}px switches at {switch}");
            assert!(text.blurs_alike(switch.min(RECT_COVERAGE_BLUR_MAX_SIGMA)));
        }
    }
}
