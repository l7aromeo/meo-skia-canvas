use skia_safe::{
    BlurStyle as SkBlurStyle, ColorChannel as SkColorChannel,
    ColorFilter as SkColorFilter, ImageFilter as SkImageFilter,
    MaskFilter as SkMaskFilter, Point as SkPoint, Point3 as SkPoint3,
    Rect as SkRect, SamplingOptions, TileMode as SkTileMode, color_filters,
    image_filters, luma_color_filter,
};

use crate::{
    color::{
        RgbaLinear, linear_srgb_color_space, rgba_css,
        rgba_linear_to_skia_color, rgba_linear_to_unpremul_color4f,
    },
    context2d::affine_to_matrix,
    error::Error,
    geometry::{Affine, Rect},
    node::filter::FilterSpec,
    paint::BlendMode,
    pixels::SamplingMode,
};

/// Skia's sampling options for one of this crate's sampling modes.
fn sampling_options(mode: SamplingMode) -> SamplingOptions {
    use skia_safe::{CubicResampler, FilterMode, MipmapMode};
    match mode {
        SamplingMode::Nearest => {
            SamplingOptions::new(FilterMode::Nearest, MipmapMode::None)
        }
        SamplingMode::Linear => {
            SamplingOptions::new(FilterMode::Linear, MipmapMode::None)
        }
        SamplingMode::Mipmapped => {
            SamplingOptions::new(FilterMode::Linear, MipmapMode::Linear)
        }
        SamplingMode::Cubic => {
            SamplingOptions::from(CubicResampler::mitchell())
        }
    }
}

/// What a filter does with the area outside its input.
///
/// Skia's tile mode, which the SVG filter primitives call `edgeMode`. It
/// decides what a filter samples when it reaches past the edge of what it
/// was given -- a blur at the border of an image has to read *something*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TileMode {
    /// Repeats the edge pixel outward. The usual choice, and what keeps a
    /// blurred edge from fading into nothing.
    #[default]
    Clamp,
    /// Tiles the input, so the right edge continues into the left.
    Repeat,
    /// Tiles it flipped, so edges meet themselves and no seam shows.
    Mirror,
    /// Reads transparent black outside. Lets a blur fade out rather than
    /// smearing.
    Decal,
}

impl TileMode {
    fn to_skia(self) -> SkTileMode {
        match self {
            Self::Clamp => SkTileMode::Clamp,
            Self::Repeat => SkTileMode::Repeat,
            Self::Mirror => SkTileMode::Mirror,
            Self::Decal => SkTileMode::Decal,
        }
    }
}

/// One channel of a colour, for the filters that read a single one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorChannel {
    /// Red.
    Red,
    /// Green.
    Green,
    /// Blue.
    Blue,
    /// Alpha.
    Alpha,
}

impl ColorChannel {
    fn to_skia(self) -> SkColorChannel {
        match self {
            Self::Red => SkColorChannel::R,
            Self::Green => SkColorChannel::G,
            Self::Blue => SkColorChannel::B,
            Self::Alpha => SkColorChannel::A,
        }
    }
}

/// A point in the space a lighting filter lights, with `z` toward the
/// viewer.
///
/// The surface being lit sits at `z = 0`, so a light at a positive `z` is in
/// front of it. Distances are in the same units the canvas draws in.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point3 {
    /// Horizontal position.
    pub x: f32,
    /// Vertical position.
    pub y: f32,
    /// Distance toward the viewer.
    pub z: f32,
}

impl Point3 {
    /// A point at `(x, y, z)`.
    pub fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    fn to_skia(self) -> SkPoint3 {
        SkPoint3::new(self.x, self.y, self.z)
    }
}

/// Image-domain filter (blur, drop shadow, color matrix wrapped as image
/// filter, compose).
///
/// Composed by `Paint` and applied to draws.
#[derive(Clone)]
pub struct ImageFilter {
    pub(crate) inner: SkImageFilter,
}

impl std::fmt::Debug for ImageFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImageFilter").finish_non_exhaustive()
    }
}

/// Which color axis [`ColorMatrix::rotated`] turns around.
///
/// CanvasKit numbers these 0, 1 and 2, which is what the JavaScript side
/// passes; naming them means a caller cannot pass 3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorAxis {
    /// Red. Rotating around it mixes green into blue.
    Red,
    /// Green. Rotating around it mixes blue into red.
    Green,
    /// Blue. Rotating around it mixes red into green.
    Blue,
}

impl ColorAxis {
    /// Position of this axis's row and column in the matrix.
    fn offset(self) -> usize {
        match self {
            Self::Red => 0,
            Self::Green => 1,
            Self::Blue => 2,
        }
    }
}

/// A 4x5 color matrix, in the form [`ColorFilter::matrix`] takes.
///
/// Row-major and twenty long: four output rows of
/// `[from_red, from_green, from_blue, from_alpha, translate]`, so output
/// channel `c` is `c_r*R + c_g*G + c_b*B + c_a*A + c_t`.
///
/// ```text
/// | r_r r_g r_b r_a r_t |
/// | g_r g_g g_b g_a g_t |
/// | b_r b_g b_b b_a b_t |
/// | a_r a_g a_b a_a a_t |
/// ```
///
/// Mirrors CanvasKit's `ColorMatrix` helpers, which the JavaScript side
/// exports under the same name. Hand the result to
/// [`ColorFilter::matrix`] with `.into()`.
///
/// # Examples
///
/// ```
/// use meo_skia_canvas::prelude::*;
///
/// # fn main() -> Result<(), meo_skia_canvas::error::Error> {
/// // Darken the blue channel, then lift every channel by a tenth.
/// let grade = ColorMatrix::scaled(1.0, 1.0, 0.8, 1.0)
///     .post_translate(0.1, 0.1, 0.1, 0.0);
/// let filter = ColorFilter::matrix(grade.into())?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorMatrix([f32; Self::LEN]);

impl ColorMatrix {
    /// The three channels a hue rotation turns between. Alpha is a row of
    /// this matrix like any other, but it is not a color axis -- rotating
    /// into it would make a hue change the opacity.
    const COLOR_AXES: usize = 3;
    /// One column per input channel, plus the translation column.
    const COLUMNS: usize = 5;
    /// Entries in the flattened row-major form.
    const LEN: usize = Self::ROWS * Self::COLUMNS;
    /// Output channels: red, green, blue, alpha.
    const ROWS: usize = 4;
    /// Column carrying the constant added to each output channel. It is the
    /// last one, and unlike the others it has no input channel behind it --
    /// which is why the sums below run over `ROWS` and not `COLUMNS`.
    const TRANSLATION: usize = Self::COLUMNS - 1;

    /// Position of `(row, column)` in the flattened form.
    const fn at(row: usize, column: usize) -> usize {
        row * Self::COLUMNS + column
    }

    /// The matrix that changes nothing: 1 down the diagonal, 0 elsewhere.
    pub fn identity() -> Self {
        let mut entries = [0.0; Self::LEN];
        let mut row = 0;
        while row < Self::ROWS {
            entries[Self::at(row, row)] = 1.0;
            row += 1;
        }
        Self(entries)
    }

    /// Wraps twenty numbers already in row-major order.
    pub fn from_rows(entries: [f32; 20]) -> Self {
        Self(entries)
    }

    /// Scales each channel independently. `1.0` leaves a channel alone.
    pub fn scaled(red: f32, green: f32, blue: f32, alpha: f32) -> Self {
        let mut matrix = Self::identity();
        for (row, scale) in [red, green, blue, alpha].into_iter().enumerate() {
            matrix.0[Self::at(row, row)] = scale;
        }
        matrix
    }

    /// Rotates the other two channels around `axis` by a given sine and
    /// cosine.
    ///
    /// The angle is passed already resolved rather than in radians, because
    /// hue-rotation grades are usually built from a sine and cosine that were
    /// computed once and reused across several matrices. Pass
    /// `angle.sin(), angle.cos()` for the ordinary case.
    pub fn rotated(axis: ColorAxis, sine: f32, cosine: f32) -> Self {
        let mut matrix = Self::identity();
        // The two channels that are not the axis, in cyclic order, so that
        // a positive angle turns the same way for every axis.
        let a = (axis.offset() + 1) % Self::COLOR_AXES;
        let b = (axis.offset() + 2) % Self::COLOR_AXES;
        matrix.0[Self::at(a, a)] = cosine;
        matrix.0[Self::at(a, b)] = sine;
        matrix.0[Self::at(b, a)] = -sine;
        matrix.0[Self::at(b, b)] = cosine;
        matrix
    }

    /// Combines two matrices into one that applies `inner` and then `outer`.
    ///
    /// Argument order is `outer` first, matching the multiplication it
    /// performs and the JavaScript `ColorMatrix.concat(outer, inner)`.
    pub fn concat(outer: Self, inner: Self) -> Self {
        let mut entries = [0.0; Self::LEN];
        for row in 0..Self::ROWS {
            for column in 0..Self::COLUMNS {
                // Only the input channels take part in the product; the
                // translation column has no row to multiply against.
                let mut sum: f32 = (0..Self::ROWS)
                    .map(|k| {
                        outer.0[Self::at(row, k)] * inner.0[Self::at(k, column)]
                    })
                    .sum();
                // `outer`'s own translation rides through unscaled, as
                // though `inner` had an implicit fifth row of `[0 0 0 0 1]`.
                if column == Self::TRANSLATION {
                    sum += outer.0[Self::at(row, Self::TRANSLATION)];
                }
                entries[Self::at(row, column)] = sum;
            }
        }
        Self(entries)
    }

    /// Adds a constant to each output channel, after everything else.
    ///
    /// Offsets are in the same 0..1 range as the colors.
    pub fn post_translate(
        mut self,
        red: f32,
        green: f32,
        blue: f32,
        alpha: f32,
    ) -> Self {
        for (row, offset) in [red, green, blue, alpha].into_iter().enumerate() {
            self.0[Self::at(row, Self::TRANSLATION)] += offset;
        }
        self
    }

    /// The twenty numbers, row-major.
    pub fn into_rows(self) -> [f32; 20] {
        self.0
    }
}

impl Default for ColorMatrix {
    fn default() -> Self {
        Self::identity()
    }
}

impl From<ColorMatrix> for [f32; 20] {
    fn from(matrix: ColorMatrix) -> Self {
        matrix.into_rows()
    }
}

impl From<[f32; 20]> for ColorMatrix {
    fn from(entries: [f32; 20]) -> Self {
        Self::from_rows(entries)
    }
}

/// Color-domain filter (luma, gamma transfers, color matrix, compose).
///
/// Composed by `Paint` or wrapped as an image filter via
/// `ImageFilter::from_color_filter`.
#[derive(Clone)]
pub struct ColorFilter {
    pub(crate) inner: SkColorFilter,
}

impl std::fmt::Debug for ColorFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ColorFilter").finish_non_exhaustive()
    }
}

/// Coverage-mask blur style. Mirrors CanvasKit's `BlurStyle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BlurStyle {
    /// Blur both inside and outside the geometry (the usual soft blur).
    #[default]
    Normal,
    /// Solid interior with a blurred exterior (glow that keeps the shape).
    Solid,
    /// Blur only outside the geometry (outline / halo).
    Outer,
    /// Blur only inside the geometry (inner shadow / feathered fill).
    Inner,
}

impl BlurStyle {
    fn to_skia(self) -> SkBlurStyle {
        match self {
            Self::Normal => SkBlurStyle::Normal,
            Self::Solid => SkBlurStyle::Solid,
            Self::Outer => SkBlurStyle::Outer,
            Self::Inner => SkBlurStyle::Inner,
        }
    }
}

/// Coverage-mask filter applied before rasterization.
///
/// Unlike a plain image-filter blur, the [`BlurStyle`] variants give glows,
/// feathered edges, and outline blurs. Composed by
/// [`Paint`](crate::paint::Paint). Mirrors CanvasKit's `MaskFilter.MakeBlur`.
#[derive(Clone)]
pub struct MaskFilter {
    pub(crate) inner: SkMaskFilter,
}

impl std::fmt::Debug for MaskFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MaskFilter").finish_non_exhaustive()
    }
}

impl MaskFilter {
    /// Builds a Gaussian coverage blur.
    ///
    /// `sigma` is the blur standard deviation in pixels. `respect_ctm` scales
    /// the blur with the canvas transform (zoom / keyframed scale); pass
    /// `false` to keep it screen-fixed.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when Skia declines to build the
    /// filter.
    pub fn blur(
        style: BlurStyle,
        sigma: f32,
        respect_ctm: bool,
    ) -> Result<Self, Error> {
        SkMaskFilter::blur(style.to_skia(), sigma, respect_ctm)
            .map(|inner| Self { inner })
            .ok_or_else(|| Error::FilterCreate {
                reason: format!("mask blur (style={style:?}, sigma={sigma})"),
            })
    }
}

impl ImageFilter {
    /// A rectangle in Skia's terms.
    fn skia_rect(rect: Rect) -> SkRect {
        SkRect::new(rect.left, rect.top, rect.right, rect.bottom)
    }

    /// A crop rectangle, or none.
    fn crop_of(crop: Option<Rect>) -> image_filters::CropRect {
        crop.map(Self::skia_rect)
            .map_or(image_filters::CropRect::NO_CROP_RECT, Into::into)
    }

    /// Wraps a Skia filter, naming `what` if Skia declined to build it.
    fn built(filter: Option<SkImageFilter>, what: &str) -> Result<Self, Error> {
        filter
            .map(|inner| Self { inner })
            .ok_or_else(|| Error::FilterCreate {
                reason: format!("{what} failed"),
            })
    }

    /// Combines two filters arithmetically:
    /// `k1 * bg * fg + k2 * bg + k3 * fg + k4`.
    ///
    /// SVG's `feComposite` with `operator="arithmetic"`. The four constants
    /// reach every ordinary compositing operation and many with no name --
    /// `(0, 1, 0, 0)` is the background alone, `(0, 0, 1, 0)` the
    /// foreground, `(1, 0, 0, 0)` their product.
    ///
    /// `enforce_premultiplied` clamps each result back into the range a
    /// premultiplied colour may occupy, which the arithmetic can leave.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when Skia declines to build it.
    #[allow(clippy::too_many_arguments)]
    pub fn arithmetic(
        k1: f32,
        k2: f32,
        k3: f32,
        k4: f32,
        enforce_premultiplied: bool,
        background: Option<ImageFilter>,
        foreground: Option<ImageFilter>,
        crop: Option<Rect>,
    ) -> Result<Self, Error> {
        Self::built(
            image_filters::arithmetic(
                k1,
                k2,
                k3,
                k4,
                enforce_premultiplied,
                background.map(|f| f.inner),
                foreground.map(|f| f.inner),
                Self::crop_of(crop),
            ),
            "arithmetic",
        )
    }

    /// Blends two filters through `mode`.
    ///
    /// The same operators `globalCompositeOperation` names, between two
    /// filtered results rather than between a draw and the canvas.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when Skia declines to build it.
    pub fn blend(
        mode: BlendMode,
        background: Option<ImageFilter>,
        foreground: Option<ImageFilter>,
        crop: Option<Rect>,
    ) -> Result<Self, Error> {
        Self::built(
            image_filters::blend(
                mode.to_skia(),
                background.map(|f| f.inner),
                foreground.map(|f| f.inner),
                Self::crop_of(crop),
            ),
            "blend",
        )
    }

    /// Restricts a filter's output to `rect`, sampling `tile_mode` outside.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when Skia declines to build it.
    pub fn crop(
        rect: Rect,
        tile_mode: TileMode,
        input: Option<ImageFilter>,
    ) -> Result<Self, Error> {
        Self::built(
            image_filters::crop(
                Self::skia_rect(rect),
                tile_mode.to_skia(),
                input.map(|f| f.inner),
            ),
            "crop",
        )
    }

    /// Grows the brightest parts of the input by a radius.
    ///
    /// SVG's `feMorphology` with `operator="dilate"`. Thickens strokes and
    /// closes small gaps; the radii are independent, so a horizontal-only
    /// dilate is `(n, 0)`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when Skia declines to build it.
    pub fn dilate(
        radius_x: f32,
        radius_y: f32,
        input: Option<ImageFilter>,
        crop: Option<Rect>,
    ) -> Result<Self, Error> {
        Self::built(
            image_filters::dilate(
                (radius_x, radius_y),
                input.map(|f| f.inner),
                Self::crop_of(crop),
            ),
            "dilate",
        )
    }

    /// Shrinks the brightest parts of the input by a radius.
    ///
    /// The inverse of [`dilate`](Self::dilate), and the other half of SVG's
    /// `feMorphology`. Thins strokes and opens small gaps.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when Skia declines to build it.
    pub fn erode(
        radius_x: f32,
        radius_y: f32,
        input: Option<ImageFilter>,
        crop: Option<Rect>,
    ) -> Result<Self, Error> {
        Self::built(
            image_filters::erode(
                (radius_x, radius_y),
                input.map(|f| f.inner),
                Self::crop_of(crop),
            ),
            "erode",
        )
    }

    /// Displaces `color`'s pixels by values read out of `displacement`.
    ///
    /// SVG's `feDisplacementMap`. Each output pixel is taken from elsewhere
    /// in `color`, offset by two channels of `displacement` scaled by
    /// `scale`. A noise texture in the displacement input is what makes
    /// water, glass and heat-haze effects.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when Skia declines to build it.
    pub fn displacement_map(
        x_channel: ColorChannel,
        y_channel: ColorChannel,
        scale: f32,
        displacement: Option<ImageFilter>,
        color: Option<ImageFilter>,
        crop: Option<Rect>,
    ) -> Result<Self, Error> {
        Self::built(
            image_filters::displacement_map(
                (x_channel.to_skia(), y_channel.to_skia()),
                scale,
                displacement.map(|f| f.inner),
                color.map(|f| f.inner),
                Self::crop_of(crop),
            ),
            "displacement_map",
        )
    }

    /// Lights the input as a surface, from a light infinitely far away.
    ///
    /// SVG's `feDiffuseLighting` with an `feDistantLight`. The alpha channel
    /// is read as a height map, so an opaque shape lit this way looks
    /// embossed. Only `direction`'s direction matters, not its length.
    ///
    /// `surface_scale` is how tall the height map stands, and `kd` how
    /// strongly the surface scatters light.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when Skia declines to build it.
    pub fn distant_lit_diffuse(
        direction: Point3,
        light_color: RgbaLinear,
        surface_scale: f32,
        kd: f32,
        input: Option<ImageFilter>,
        crop: Option<Rect>,
    ) -> Result<Self, Error> {
        Self::built(
            image_filters::distant_lit_diffuse(
                direction.to_skia(),
                rgba_linear_to_skia_color(light_color),
                surface_scale,
                kd,
                input.map(|f| f.inner),
                Self::crop_of(crop),
            ),
            "distant_lit_diffuse",
        )
    }

    /// As [`distant_lit_diffuse`](Self::distant_lit_diffuse), with a
    /// specular highlight.
    ///
    /// `ks` is how strongly the surface reflects and `shininess` how tight
    /// the highlight is -- higher is a smaller, harder glint.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when Skia declines to build it.
    #[allow(clippy::too_many_arguments)]
    pub fn distant_lit_specular(
        direction: Point3,
        light_color: RgbaLinear,
        surface_scale: f32,
        ks: f32,
        shininess: f32,
        input: Option<ImageFilter>,
        crop: Option<Rect>,
    ) -> Result<Self, Error> {
        Self::built(
            image_filters::distant_lit_specular(
                direction.to_skia(),
                rgba_linear_to_skia_color(light_color),
                surface_scale,
                ks,
                shininess,
                input.map(|f| f.inner),
                Self::crop_of(crop),
            ),
            "distant_lit_specular",
        )
    }

    /// Lights the input from a point at `location`.
    ///
    /// As [`distant_lit_diffuse`](Self::distant_lit_diffuse), but the light
    /// has a position rather than only a direction, so it falls off across
    /// the surface.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when Skia declines to build it.
    pub fn point_lit_diffuse(
        location: Point3,
        light_color: RgbaLinear,
        surface_scale: f32,
        kd: f32,
        input: Option<ImageFilter>,
        crop: Option<Rect>,
    ) -> Result<Self, Error> {
        Self::built(
            image_filters::point_lit_diffuse(
                location.to_skia(),
                rgba_linear_to_skia_color(light_color),
                surface_scale,
                kd,
                input.map(|f| f.inner),
                Self::crop_of(crop),
            ),
            "point_lit_diffuse",
        )
    }

    /// As [`point_lit_diffuse`](Self::point_lit_diffuse), with a specular
    /// highlight.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when Skia declines to build it.
    #[allow(clippy::too_many_arguments)]
    pub fn point_lit_specular(
        location: Point3,
        light_color: RgbaLinear,
        surface_scale: f32,
        ks: f32,
        shininess: f32,
        input: Option<ImageFilter>,
        crop: Option<Rect>,
    ) -> Result<Self, Error> {
        Self::built(
            image_filters::point_lit_specular(
                location.to_skia(),
                rgba_linear_to_skia_color(light_color),
                surface_scale,
                ks,
                shininess,
                input.map(|f| f.inner),
                Self::crop_of(crop),
            ),
            "point_lit_specular",
        )
    }

    /// Lights the input from a cone at `location` pointing at `target`.
    ///
    /// `cutoff_angle` is the cone's half-angle in degrees, and
    /// `falloff_exponent` how sharply the light fades toward its edge.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when Skia declines to build it.
    #[allow(clippy::too_many_arguments)]
    pub fn spot_lit_diffuse(
        location: Point3,
        target: Point3,
        falloff_exponent: f32,
        cutoff_angle: f32,
        light_color: RgbaLinear,
        surface_scale: f32,
        kd: f32,
        input: Option<ImageFilter>,
        crop: Option<Rect>,
    ) -> Result<Self, Error> {
        Self::built(
            image_filters::spot_lit_diffuse(
                location.to_skia(),
                target.to_skia(),
                falloff_exponent,
                cutoff_angle,
                rgba_linear_to_skia_color(light_color),
                surface_scale,
                kd,
                input.map(|f| f.inner),
                Self::crop_of(crop),
            ),
            "spot_lit_diffuse",
        )
    }

    /// As [`spot_lit_diffuse`](Self::spot_lit_diffuse), with a specular
    /// highlight.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when Skia declines to build it.
    #[allow(clippy::too_many_arguments)]
    pub fn spot_lit_specular(
        location: Point3,
        target: Point3,
        falloff_exponent: f32,
        cutoff_angle: f32,
        light_color: RgbaLinear,
        surface_scale: f32,
        ks: f32,
        shininess: f32,
        input: Option<ImageFilter>,
        crop: Option<Rect>,
    ) -> Result<Self, Error> {
        Self::built(
            image_filters::spot_lit_specular(
                location.to_skia(),
                target.to_skia(),
                falloff_exponent,
                cutoff_angle,
                rgba_linear_to_skia_color(light_color),
                surface_scale,
                ks,
                shininess,
                input.map(|f| f.inner),
                Self::crop_of(crop),
            ),
            "spot_lit_specular",
        )
    }

    /// The shadow [`drop_shadow`](Self::drop_shadow) casts, without the
    /// thing casting it.
    ///
    /// For drawing the shadow separately -- under other content, or with its
    /// own blend mode.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when Skia declines to build it.
    #[allow(clippy::too_many_arguments)]
    pub fn drop_shadow_only(
        dx: f32,
        dy: f32,
        sigma_x: f32,
        sigma_y: f32,
        color: RgbaLinear,
        input: Option<ImageFilter>,
        crop: Option<Rect>,
    ) -> Result<Self, Error> {
        Self::built(
            image_filters::drop_shadow_only(
                (dx, dy),
                (sigma_x, sigma_y),
                rgba_linear_to_unpremul_color4f(color),
                linear_srgb_color_space(),
                input.map(|f| f.inner),
                Self::crop_of(crop),
            ),
            "drop_shadow_only",
        )
    }

    /// A filter that produces nothing.
    ///
    /// Transparent black everywhere. The identity element when building a
    /// chain conditionally, and how a
    /// [`drop_shadow_only`](Self::drop_shadow_only) is drawn on its own.
    pub fn empty() -> Self {
        Self {
            inner: image_filters::empty(),
        }
    }

    /// Magnifies the region `lens_bounds` by `zoom`.
    ///
    /// `inset` is how far inside the bounds the magnification blends back to
    /// none, which gives the lens a soft rim rather than a hard edge.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when Skia declines to build it.
    pub fn magnifier(
        lens_bounds: Rect,
        zoom: f32,
        inset: f32,
        sampling: SamplingMode,
        input: Option<ImageFilter>,
        crop: Option<Rect>,
    ) -> Result<Self, Error> {
        Self::built(
            image_filters::magnifier(
                Self::skia_rect(lens_bounds),
                zoom,
                inset,
                sampling_options(sampling),
                input.map(|f| f.inner),
                Self::crop_of(crop),
            ),
            "magnifier",
        )
    }

    /// Convolves the input with an arbitrary kernel.
    ///
    /// SVG's `feConvolveMatrix`, and the general form every sharpen, emboss
    /// and edge-detect is a case of. `kernel` is `width * height` values in
    /// row-major order; `gain` scales the result and `bias` shifts it.
    ///
    /// `kernel_offset` names which kernel cell sits over the pixel being
    /// computed -- the centre for a symmetric kernel, off-centre to shift
    /// the result.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when the kernel's length is not
    /// `width * height`, or when Skia declines to build it.
    #[allow(clippy::too_many_arguments)]
    pub fn matrix_convolution(
        kernel_width: i32,
        kernel_height: i32,
        kernel: &[f32],
        gain: f32,
        bias: f32,
        kernel_offset_x: i32,
        kernel_offset_y: i32,
        tile_mode: TileMode,
        convolve_alpha: bool,
        input: Option<ImageFilter>,
        crop: Option<Rect>,
    ) -> Result<Self, Error> {
        // A negative dimension is refused on its own, before the length is
        // compared against anything. Clamping it to zero instead made an
        // empty kernel the *expected* length for `-1x-1`, so the empty case
        // sailed past this check and Skia asserted on the raw signed
        // product it computes for itself. It also produced the wrong
        // complaint for the non-empty case: nine values for `-3x-3` were
        // rejected as "needs 0 kernel values", naming the clamp rather than
        // the negative that caused it.
        if kernel_width <= 0 || kernel_height <= 0 {
            return Err(Error::FilterCreate {
                reason: format!(
                    "a convolution kernel must be at least 1x1, \
                     got {kernel_width}x{kernel_height}"
                ),
            });
        }
        // Checked here rather than left to Skia, which reads past the slice
        // rather than refusing: the length and the declared size are two
        // arguments that have to agree and nothing else makes them.
        let expected = kernel_width as usize * kernel_height as usize;
        if kernel.len() != expected {
            return Err(Error::FilterCreate {
                reason: format!(
                    "a {kernel_width}x{kernel_height} convolution needs \
                     {expected} kernel values, got {}",
                    kernel.len()
                ),
            });
        }
        Self::built(
            image_filters::matrix_convolution(
                (kernel_width, kernel_height),
                kernel,
                gain,
                bias,
                (kernel_offset_x, kernel_offset_y),
                tile_mode.to_skia(),
                convolve_alpha,
                input.map(|f| f.inner),
                Self::crop_of(crop),
            ),
            "matrix_convolution",
        )
    }

    /// Transforms the input by an affine matrix before drawing it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when Skia declines to build it.
    pub fn matrix_transform(
        transform: Affine,
        sampling: SamplingMode,
        input: Option<ImageFilter>,
    ) -> Result<Self, Error> {
        Self::built(
            image_filters::matrix_transform(
                &affine_to_matrix(transform),
                sampling_options(sampling),
                input.map(|f| f.inner),
            ),
            "matrix_transform",
        )
    }

    /// Draws several filters over one another, first to last.
    ///
    /// A `None` in the list is the source draw unfiltered, which is how a
    /// filtered copy is composited over the original.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when Skia declines to build it.
    pub fn merge(
        filters: Vec<Option<ImageFilter>>,
        crop: Option<Rect>,
    ) -> Result<Self, Error> {
        Self::built(
            image_filters::merge(
                filters.into_iter().map(|f| f.map(|f| f.inner)),
                Self::crop_of(crop),
            ),
            "merge",
        )
    }

    /// Shifts the input by `(dx, dy)`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when Skia declines to build it.
    pub fn offset(
        dx: f32,
        dy: f32,
        input: Option<ImageFilter>,
        crop: Option<Rect>,
    ) -> Result<Self, Error> {
        Self::built(
            image_filters::offset(
                (dx, dy),
                input.map(|f| f.inner),
                Self::crop_of(crop),
            ),
            "offset",
        )
    }

    /// Repeats the `src` region of the input across `dst`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when Skia declines to build it.
    pub fn tile(
        src: Rect,
        dst: Rect,
        input: Option<ImageFilter>,
    ) -> Result<Self, Error> {
        Self::built(
            image_filters::tile(
                Self::skia_rect(src),
                Self::skia_rect(dst),
                input.map(|f| f.inner),
            ),
            "tile",
        )
    }

    /// Builds a Gaussian blur with separable sigmas.
    ///
    /// `input` is the upstream filter to blur, or `None` to blur the source
    /// draw. `tile_mode` says what the kernel reads past the edge of that
    /// input, and `None` means [`TileMode::Decal`] -- transparent outside,
    /// so the blur fades out rather than smearing the edge pixel. That is
    /// Skia's default here and the JavaScript `MakeBlur`'s.
    ///
    /// Both were hardcoded once: this passed `None` for the tile mode and
    /// `None` for the crop, so it was the one constructor of seventeen that
    /// took no crop rect, while the binding's `MakeBlur` reached a tile mode
    /// no Rust caller could.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when Skia declines to build the
    /// filter.
    pub fn blur(
        sigma_x: f32,
        sigma_y: f32,
        tile_mode: Option<TileMode>,
        input: Option<ImageFilter>,
        crop: Option<Rect>,
    ) -> Result<Self, Error> {
        Self::built(
            image_filters::blur(
                (sigma_x, sigma_y),
                tile_mode.map(TileMode::to_skia),
                input.map(|f| f.inner),
                Self::crop_of(crop),
            ),
            &format!("blur({sigma_x}, {sigma_y})"),
        )
    }

    /// Builds a drop shadow at `(dx, dy)` with separable blur sigmas.
    ///
    /// `color` is premultiplied linear and is tagged as linear-light sRGB,
    /// not as the destination's working color space. On a wider-gamut
    /// surface the shadow will therefore not match an equivalent
    /// [`Paint`](crate::paint::Paint) fill of the same `RgbaLinear`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when Skia declines to build the
    /// filter.
    pub fn drop_shadow(
        dx: f32,
        dy: f32,
        sigma_x: f32,
        sigma_y: f32,
        color: RgbaLinear,
        input: Option<ImageFilter>,
        crop: Option<Rect>,
    ) -> Result<Self, Error> {
        // Tag the shadow color as linear-light sRGB. Without an
        // explicit color space, Skia treats the value as
        // sRGB-encoded and gamma-decodes it -- darkening the shadow.
        Self::built(
            image_filters::drop_shadow(
                skia_safe::Vector::new(dx, dy),
                (sigma_x, sigma_y),
                rgba_linear_to_unpremul_color4f(color),
                Some(linear_srgb_color_space()),
                input.map(|f| f.inner),
                Self::crop_of(crop),
            ),
            &format!("drop_shadow({dx}, {dy})"),
        )
    }

    /// 4x5 color matrix in row-major order:
    ///
    /// ```text
    /// | r_r  r_g  r_b  r_a  r_offset |
    /// | g_r  g_g  g_b  g_a  g_offset |
    /// | b_r  b_g  b_b  b_a  b_offset |
    /// | a_r  a_g  a_b  a_a  a_offset |
    /// ```
    ///
    /// Output channel `c` = `c_r * r_in + c_g * g_in + c_b * b_in + c_a *
    /// a_in + c_offset`. Offsets are in the `0..1` range for `u8` channels.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when Skia declines to build the
    /// filter.
    pub fn color_matrix(
        matrix: [f32; 20],
        input: Option<ImageFilter>,
        crop: Option<Rect>,
    ) -> Result<Self, Error> {
        // Before the colour filter is built, not after: it is an argument
        // here, so an unwrapped null took the process down while this
        // function's own `Result` was still unreachable.
        ColorFilter::finite(&matrix, "color_matrix")?;
        Self::built(
            image_filters::color_filter(
                color_filters::matrix_row_major(&matrix, None),
                input.map(|f| f.inner),
                Self::crop_of(crop),
            ),
            "color_matrix",
        )
    }

    /// Wraps a `ColorFilter` as an image filter, optionally chained
    /// onto `input`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when Skia declines to build the
    /// filter.
    pub fn from_color_filter(
        color_filter: ColorFilter,
        input: Option<ImageFilter>,
        crop: Option<Rect>,
    ) -> Result<Self, Error> {
        Self::built(
            image_filters::color_filter(
                color_filter.inner,
                input.map(|f| f.inner),
                Self::crop_of(crop),
            ),
            "from_color_filter",
        )
    }

    /// Composes two image filters: `outer(inner(source))`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when Skia declines to build the
    /// filter.
    pub fn compose(
        outer: ImageFilter,
        inner: ImageFilter,
    ) -> Result<Self, Error> {
        image_filters::compose(outer.inner, inner.inner)
            .map(|f| ImageFilter { inner: f })
            .ok_or_else(|| Error::FilterCreate {
                reason: "image filter compose failed".to_string(),
            })
    }
}

impl ColorFilter {
    /// Builds Skia's luma color filter.
    ///
    /// Output alpha is the perceived luminance of the input RGB, and output
    /// RGB is zero. Useful as the `inner` filter in a `destination-in` mask
    /// path, where luminance becomes the alpha mask.
    pub fn luma() -> Self {
        Self {
            inner: luma_color_filter::new(),
        }
    }

    /// Applies the linear-to-sRGB gamma transfer to the input color before
    /// downstream draws see it.
    ///
    /// Used to bridge linear-light pipelines to gamma-coded readers.
    pub fn linear_to_srgb_gamma() -> Self {
        Self {
            inner: color_filters::linear_to_srgb_gamma(),
        }
    }

    /// Inverse of `linear_to_srgb_gamma`.
    pub fn srgb_to_linear_gamma() -> Self {
        Self {
            inner: color_filters::srgb_to_linear_gamma(),
        }
    }

    /// A filter that blends every pixel with `color` through `mode`.
    ///
    /// The colour-domain half of what a `globalCompositeOperation` does:
    /// the same blend, applied to a draw's own colours rather than between
    /// a draw and the canvas under it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when Skia declines the mode, which it
    /// does for the ones that need a destination it does not have here.
    pub fn blend(color: RgbaLinear, mode: BlendMode) -> Result<Self, Error> {
        color_filters::blend(rgba_linear_to_skia_color(color), mode.to_skia())
            .map(|inner| Self { inner })
            .ok_or_else(|| Error::FilterCreate {
                reason: format!("no color filter for blend mode {mode:?}"),
            })
    }

    /// A filter that multiplies each colour by a 5x4 matrix.
    ///
    /// Row-major, twenty entries: four rows of `[r, g, b, a, offset]`
    /// producing red, green, blue and alpha in turn. The offset column is
    /// in the same 0..1 range as the rest, so a row of
    /// `[0, 0, 0, 0, 0.5]` sets that channel to a half regardless of input.
    ///
    /// The identity is 1 on the diagonal and 0 everywhere else. This is what
    /// CSS's `grayscale`, `sepia`, `saturate` and `hue-rotate` all compile
    /// down to.
    ///
    /// [`ColorMatrix`] builds the twenty numbers without writing them out:
    /// `ColorFilter::matrix(ColorMatrix::scaled(1.0, 0.9, 0.8, 1.0).into())?`.
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when an entry is not finite. Skia
    /// hands back a null for such a matrix and `skia_safe` unwraps it, so
    /// this had no way to report the failure and took the process down
    /// instead -- from safe code, on input a caller need never have typed:
    /// [`ColorMatrix::concat`] of two large scalings overflows to infinity
    /// from entirely finite arguments.
    pub fn matrix(matrix: [f32; 20]) -> Result<Self, Error> {
        Self::finite(&matrix, "matrix")?;
        Ok(Self {
            inner: color_filters::matrix_row_major(&matrix, None),
        })
    }

    /// Refuses a matrix Skia would answer with a null.
    ///
    /// Checked here rather than after the call, because `skia_safe`'s
    /// wrapper unwraps that null rather than returning it -- there is no
    /// failure left to observe by the time it comes back.
    fn finite(matrix: &[f32; 20], what: &str) -> Result<(), Error> {
        match matrix.iter().position(|value| !value.is_finite()) {
            None => Ok(()),
            Some(at) => Err(Error::FilterCreate {
                reason: format!(
                    "{what} needs twenty finite numbers; entry {at} is {}",
                    matrix[at]
                ),
            }),
        }
    }

    /// As [`matrix`](Self::matrix), applied in hue-saturation-lightness
    /// rather than in RGB.
    ///
    /// The same twenty numbers, against different axes: the first three
    /// inputs are hue, saturation and lightness. Rotating hue is a shift in
    /// the first row here where in RGB it takes the whole matrix.
    /// # Errors
    ///
    /// As [`matrix`](Self::matrix): a non-finite entry is refused rather
    /// than passed to a Skia call that would panic on the null it returns.
    pub fn hsla_matrix(matrix: [f32; 20]) -> Result<Self, Error> {
        Self::finite(&matrix, "hsla_matrix")?;
        Ok(Self {
            inner: color_filters::hsla_matrix(&matrix),
        })
    }

    /// A filter `weight` of the way from `from` to `to`.
    ///
    /// 0 is `from` and 1 is `to`. For crossfading between two colour
    /// treatments without drawing twice.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when Skia cannot interpolate the two.
    pub fn lerp(
        weight: f32,
        from: ColorFilter,
        to: ColorFilter,
    ) -> Result<Self, Error> {
        color_filters::lerp(weight, from.inner, to.inner)
            .map(|inner| Self { inner })
            .ok_or_else(|| Error::FilterCreate {
                reason: "color filter lerp failed".to_string(),
            })
    }

    /// A filter that multiplies by `multiply` and then adds `add`.
    ///
    /// Per channel, which is what makes it a tint plus a lift rather than a
    /// blend: `multiply` scales what is there and `add` raises the floor.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when Skia declines to build it.
    pub fn lighting(
        multiply: RgbaLinear,
        add: RgbaLinear,
    ) -> Result<Self, Error> {
        color_filters::lighting(
            rgba_linear_to_skia_color(multiply),
            rgba_linear_to_skia_color(add),
        )
        .map(|inner| Self { inner })
        .ok_or_else(|| Error::FilterCreate {
            reason: "lighting color filter failed".to_string(),
        })
    }

    /// A filter that maps every channel through one 256-entry lookup table.
    ///
    /// Input byte in, output byte out, alpha included. An arbitrary transfer
    /// curve, where [`matrix`](Self::matrix) can only do linear ones -- a
    /// posterize or a levels curve is a table and is not a matrix.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when Skia declines to build it.
    pub fn table(table: [u8; 256]) -> Result<Self, Error> {
        color_filters::table(&table)
            .map(|inner| Self { inner })
            .ok_or_else(|| Error::FilterCreate {
                reason: "table color filter failed".to_string(),
            })
    }

    /// As [`table`](Self::table), with a separate table per channel.
    ///
    /// `None` leaves a channel alone, which is the difference from passing
    /// an identity table: no lookup happens at all.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when Skia declines to build it.
    pub fn table_argb(
        alpha: Option<[u8; 256]>,
        red: Option<[u8; 256]>,
        green: Option<[u8; 256]>,
        blue: Option<[u8; 256]>,
    ) -> Result<Self, Error> {
        color_filters::table_argb(
            alpha.as_ref(),
            red.as_ref(),
            green.as_ref(),
            blue.as_ref(),
        )
        .map(|inner| Self { inner })
        .ok_or_else(|| Error::FilterCreate {
            reason: "table color filter failed".to_string(),
        })
    }

    /// Composes two color filters: `outer(inner(input))`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FilterCreate`] when Skia declines to build the
    /// filter.
    pub fn compose(
        outer: ColorFilter,
        inner: ColorFilter,
    ) -> Result<Self, Error> {
        color_filters::compose(outer.inner, inner.inner)
            .map(|f| ColorFilter { inner: f })
            .ok_or_else(|| Error::FilterCreate {
                reason: "color filter compose failed".to_string(),
            })
    }
}

/// One step of a CSS-style filter chain.
///
/// The Canvas API's `filter` property takes a string such as
/// `"blur(4px) saturate(150%)"`. Here it is a slice of these, passed to
/// [`Context2D::set_filter`](crate::context2d::Context2D::set_filter). No
/// parser: a value that a stylesheet would have to spell correctly is a
/// typed argument instead. One failure mode survives that -- a non-finite
/// amount, which
/// [`set_filter`](crate::context2d::Context2D::set_filter) rejects rather
/// than passing on to fail at the next draw.
///
/// Amounts are fractions, not percentages, so CSS's `150%` is `1.5`.
///
/// What a value means differs by group, which is easy to get backwards:
///
/// - **Scaling filters** -- [`Brightness`](FilterOp::Brightness),
///   [`Contrast`](FilterOp::Contrast), [`Opacity`](FilterOp::Opacity),
///   [`Saturate`](FilterOp::Saturate) -- multiply what is already there, so
///   `1.0` leaves the drawing unchanged and `0.0` erases the property.
/// - **Fraction filters** -- [`Grayscale`](FilterOp::Grayscale),
///   [`Invert`](FilterOp::Invert), [`Sepia`](FilterOp::Sepia) -- apply an
///   effect by amount, so `0.0` leaves the drawing unchanged and `1.0` applies
///   it fully.
/// - **Measured filters** -- [`Blur`](FilterOp::Blur) and
///   [`DropShadow`](FilterOp::DropShadow) in pixels,
///   [`HueRotate`](FilterOp::HueRotate) in degrees -- carry a quantity rather
///   than a fraction. `0.0` still leaves the drawing unchanged, but there is no
///   full: `HueRotate(1.0)` turns the hue by one degree and is all but
///   invisible, where `HueRotate(180.0)` is the opposite colour, and
///   `Blur(1.0)` is a one-pixel radius.
///
/// # Examples
///
/// ```
/// use meo_skia_canvas::prelude::*;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let mut canvas = Canvas::new(64.0, 64.0);
/// let ctx = canvas.context();
///
/// // "blur(4px) saturate(150%)"
/// ctx.set_filter(&[FilterOp::Blur(4.0), FilterOp::Saturate(1.5)])?;
///
/// // "none"
/// ctx.set_filter(&[])?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FilterOp {
    /// Gaussian blur of the given radius in pixels. `0.0` is no blur.
    Blur(f32),
    /// Linear brightness scale. `1.0` unchanged, `0.0` black.
    Brightness(f32),
    /// Contrast scale about mid-gray. `1.0` unchanged, `0.0` flat gray.
    Contrast(f32),
    /// Desaturation fraction. `0.0` unchanged, `1.0` fully gray.
    Grayscale(f32),
    /// Hue rotation in degrees. `0.0` unchanged.
    HueRotate(f32),
    /// Inversion fraction. `0.0` unchanged, `1.0` fully inverted.
    Invert(f32),
    /// Opacity scale. `1.0` unchanged, `0.0` fully transparent.
    Opacity(f32),
    /// Saturation scale. `1.0` unchanged, `0.0` gray, above `1.0` boosted.
    Saturate(f32),
    /// Sepia fraction. `0.0` unchanged, `1.0` fully sepia.
    Sepia(f32),
    /// A shadow cast by the drawing's alpha, behind it.
    ///
    /// Unlike the [`shadow`](crate::context2d::Context2D::set_shadow_blur)
    /// state, a filter shadow applies to the drawing as a whole after it is
    /// composed, and several can be chained.
    DropShadow {
        /// Horizontal offset in pixels. Positive moves right.
        offset_x: f32,
        /// Vertical offset in pixels. Positive moves down.
        offset_y: f32,
        /// Blur radius in pixels. `0.0` gives a hard-edged shadow.
        blur: f32,
        /// Shadow color.
        color: RgbaLinear,
    },
}

impl FilterOp {
    /// Rejects a value Skia cannot build a filter from.
    ///
    /// The JavaScript side never needs this: its CSS parser discards a
    /// non-finite amount before a `FilterSpec` is ever built. A typed API
    /// hands the float straight through, so the check has to live here --
    /// without it Skia returns a null color filter and skia-safe unwraps
    /// it, aborting on the *next draw* rather than at the offending call.
    pub(crate) fn validate(self) -> Result<(), Error> {
        let reject = |what: &str, value: f32| {
            Err(Error::FilterCreate {
                reason: format!("{what} must be finite, got {value}"),
            })
        };
        let finite = |what: &str, value: f32| match value.is_finite() {
            true => Ok(()),
            false => reject(what, value),
        };

        match self {
            Self::Blur(radius) => finite("blur radius", radius),
            Self::Brightness(amount) => finite("brightness", amount),
            Self::Contrast(amount) => finite("contrast", amount),
            Self::Grayscale(amount) => finite("grayscale", amount),
            Self::HueRotate(degrees) => finite("hue-rotate", degrees),
            Self::Invert(amount) => finite("invert", amount),
            Self::Opacity(amount) => finite("opacity", amount),
            Self::Saturate(amount) => finite("saturate", amount),
            Self::Sepia(amount) => finite("sepia", amount),
            Self::DropShadow {
                offset_x,
                offset_y,
                blur,
                color,
            } => {
                finite("drop-shadow offset x", offset_x)?;
                finite("drop-shadow offset y", offset_y)?;
                finite("drop-shadow blur", blur)?;
                for (channel, value) in [
                    ("r", color.r),
                    ("g", color.g),
                    ("b", color.b),
                    ("a", color.a),
                ] {
                    finite(&format!("drop-shadow color {channel}"), value)?;
                }
                Ok(())
            }
        }
    }

    /// The CSS the operation corresponds to.
    ///
    /// Joined with spaces this is what
    /// [`Context2D::filter`](crate::context2d::Context2D::filter) reports,
    /// so the Rust side round-trips through the same string the JavaScript
    /// side would have set.
    pub(crate) fn to_css(self) -> String {
        match self {
            Self::Blur(radius) => format!("blur({radius}px)"),
            Self::Brightness(amount) => format!("brightness({amount})"),
            Self::Contrast(amount) => format!("contrast({amount})"),
            Self::Grayscale(amount) => format!("grayscale({amount})"),
            Self::HueRotate(degrees) => format!("hue-rotate({degrees}deg)"),
            Self::Invert(amount) => format!("invert({amount})"),
            Self::Opacity(amount) => format!("opacity({amount})"),
            Self::Saturate(amount) => format!("saturate({amount})"),
            Self::Sepia(amount) => format!("sepia({amount})"),
            Self::DropShadow {
                offset_x,
                offset_y,
                blur,
                color,
            } => {
                format!(
                    "drop-shadow({offset_x}px {offset_y}px {blur}px {})",
                    rgba_css(color)
                )
            }
        }
    }

    /// Lowers the operation onto the internal filter spec.
    pub(crate) fn to_spec(self) -> FilterSpec {
        let plain = |name: &str, value: f32| FilterSpec::Plain {
            name: name.to_string(),
            value,
        };
        match self {
            Self::Blur(radius) => plain("blur", radius),
            Self::Brightness(amount) => plain("brightness", amount),
            Self::Contrast(amount) => plain("contrast", amount),
            Self::Grayscale(amount) => plain("grayscale", amount),
            Self::HueRotate(degrees) => plain("hue-rotate", degrees),
            Self::Invert(amount) => plain("invert", amount),
            Self::Opacity(amount) => plain("opacity", amount),
            Self::Saturate(amount) => plain("saturate", amount),
            Self::Sepia(amount) => plain("sepia", amount),
            Self::DropShadow {
                offset_x,
                offset_y,
                blur,
                color,
            } => FilterSpec::Shadow {
                offset: SkPoint::new(offset_x, offset_y),
                blur,
                color: rgba_linear_to_skia_color(color),
            },
        }
    }
}

/// Every expectation here was read out of `lib/classes/color_matrix.js` by
/// running it, not derived from the same algebra a second time -- the point
/// is that the two surfaces agree, and a shared derivation would agree with
/// itself while both were wrong.
#[cfg(test)]
mod sampling_tests {
    use super::sampling_options;
    use crate::pixels::SamplingMode;

    /// Mitchell-Netravali's own recommended parameters, from the 1988 paper
    /// that names the family: B = C = 1/3, the pair they single out as
    /// subjectively best of the whole `(B, C)` plane. `lib/index.d.ts`
    /// promises callers "Mitchell-Netravali bicubic" by name for this mode,
    /// and nothing checked that promise.
    #[test]
    fn the_cubic_sampling_mode_is_mitchell() {
        let options = sampling_options(SamplingMode::Cubic);
        assert!(options.use_cubic, "`Cubic` takes a cubic");
        assert!(
            (options.cubic.b - 1.0 / 3.0).abs() < 1e-6
                && (options.cubic.c - 1.0 / 3.0).abs() < 1e-6,
            "B and C are Mitchell's, got ({}, {}) -- CatmullRom is (0, 0.5)",
            options.cubic.b,
            options.cubic.c
        );
    }

    /// The three modes that are not cubic do not silently become one.
    /// `use_cubic` decides whether Skia consults the mipmap chain at all, so
    /// a mode that gained a cubic would lose mipmapping with it.
    #[test]
    fn the_other_sampling_modes_stay_out_of_the_cubic_path() {
        for mode in [
            SamplingMode::Nearest,
            SamplingMode::Linear,
            SamplingMode::Mipmapped,
        ] {
            assert!(
                !sampling_options(mode).use_cubic,
                "{mode:?} must not take a cubic"
            );
        }
    }
}

#[cfg(test)]
mod color_matrix_tests {
    use super::*;

    #[test]
    fn identity_changes_nothing() {
        assert_eq!(
            ColorMatrix::identity().into_rows(),
            [
                1.0, 0.0, 0.0, 0.0, 0.0, //
                0.0, 1.0, 0.0, 0.0, 0.0, //
                0.0, 0.0, 1.0, 0.0, 0.0, //
                0.0, 0.0, 0.0, 1.0, 0.0,
            ]
        );
        assert_eq!(ColorMatrix::default(), ColorMatrix::identity());
    }

    #[test]
    fn scaled_puts_each_factor_on_its_own_diagonal() {
        assert_eq!(
            ColorMatrix::scaled(0.5, 1.0, 2.0, 0.25).into_rows(),
            [
                0.5, 0.0, 0.0, 0.0, 0.0, //
                0.0, 1.0, 0.0, 0.0, 0.0, //
                0.0, 0.0, 2.0, 0.0, 0.0, //
                0.0, 0.0, 0.0, 0.25, 0.0,
            ]
        );
    }

    #[test]
    fn post_translate_only_touches_the_last_column() {
        assert_eq!(
            ColorMatrix::identity()
                .post_translate(0.1, 0.2, 0.3, 0.4)
                .into_rows(),
            [
                1.0, 0.0, 0.0, 0.0, 0.1, //
                0.0, 1.0, 0.0, 0.0, 0.2, //
                0.0, 0.0, 1.0, 0.0, 0.3, //
                0.0, 0.0, 0.0, 1.0, 0.4,
            ]
        );
    }

    /// A quarter turn -- sine 1, cosine 0 -- around each axis in turn.
    ///
    /// Alpha stays 1 on its own diagonal in all three. That is the assertion
    /// worth having: the axis index is taken modulo three, not modulo the
    /// four rows, and a rotation that wrapped into alpha would silently make
    /// a hue change the opacity.
    #[test]
    fn rotated_turns_the_two_channels_that_are_not_the_axis() {
        assert_eq!(
            ColorMatrix::rotated(ColorAxis::Red, 1.0, 0.0).into_rows(),
            [
                1.0, 0.0, 0.0, 0.0, 0.0, //
                0.0, 0.0, 1.0, 0.0, 0.0, //
                0.0, -1.0, 0.0, 0.0, 0.0, //
                0.0, 0.0, 0.0, 1.0, 0.0,
            ]
        );
        assert_eq!(
            ColorMatrix::rotated(ColorAxis::Green, 1.0, 0.0).into_rows(),
            [
                0.0, 0.0, -1.0, 0.0, 0.0, //
                0.0, 1.0, 0.0, 0.0, 0.0, //
                1.0, 0.0, 0.0, 0.0, 0.0, //
                0.0, 0.0, 0.0, 1.0, 0.0,
            ]
        );
        assert_eq!(
            ColorMatrix::rotated(ColorAxis::Blue, 1.0, 0.0).into_rows(),
            [
                0.0, 1.0, 0.0, 0.0, 0.0, //
                -1.0, 0.0, 0.0, 0.0, 0.0, //
                0.0, 0.0, 1.0, 0.0, 0.0, //
                0.0, 0.0, 0.0, 1.0, 0.0,
            ]
        );
    }

    /// The translation column is the part a naive matrix multiply gets
    /// wrong: `outer`'s own offsets ride through unscaled, as though `inner`
    /// carried an implicit fifth row of `[0 0 0 0 1]`.
    #[test]
    fn concat_applies_inner_then_outer() {
        let outer = ColorMatrix::scaled(2.0, 2.0, 2.0, 1.0)
            .post_translate(0.5, 0.0, 0.0, 0.0);
        let inner = ColorMatrix::rotated(ColorAxis::Blue, 1.0, 0.0);
        assert_eq!(
            ColorMatrix::concat(outer, inner).into_rows(),
            [
                0.0, 2.0, 0.0, 0.0, 0.5, //
                -2.0, 0.0, 0.0, 0.0, 0.0, //
                0.0, 0.0, 2.0, 0.0, 0.0, //
                0.0, 0.0, 0.0, 1.0, 0.0,
            ]
        );
    }

    #[test]
    fn concat_with_the_identity_on_either_side_is_a_no_op() {
        let matrix = ColorMatrix::scaled(0.3, 0.6, 0.9, 1.0)
            .post_translate(0.05, 0.0, -0.05, 0.0);
        assert_eq!(
            ColorMatrix::concat(ColorMatrix::identity(), matrix),
            matrix
        );
        assert_eq!(
            ColorMatrix::concat(matrix, ColorMatrix::identity()),
            matrix
        );
    }

    #[test]
    fn rows_round_trip_through_the_conversions() {
        let matrix = ColorMatrix::rotated(ColorAxis::Green, 0.6, 0.8);
        let rows: [f32; 20] = matrix.into();
        assert_eq!(ColorMatrix::from(rows), matrix);
        assert_eq!(ColorMatrix::from_rows(rows), matrix);
    }
}

#[cfg(test)]
mod color_filter_tests {
    use super::*;
    use crate::prelude::*;

    /// The 5x4 identity: 1 on the diagonal, nothing added.
    fn identity_matrix() -> [f32; 20] {
        ColorMatrix::identity().into_rows()
    }

    #[test]
    fn every_constructor_produces_a_filter() {
        // Skia hands back `None` rather than an error for a filter it will
        // not build, and each of these turns that into a named `Err`. What
        // this checks is that the ordinary arguments do not take that path.
        let white = RgbaLinear::opaque(1.0, 1.0, 1.0);
        let half = RgbaLinear::opaque(0.5, 0.5, 0.5);

        ColorFilter::blend(white, BlendMode::Multiply).expect("blend");
        ColorFilter::matrix(identity_matrix()).expect("matrix");
        ColorFilter::hsla_matrix(identity_matrix()).expect("hsla_matrix");
        ColorFilter::lighting(white, half).expect("lighting");
        ColorFilter::table([0u8; 256]).expect("table");
        ColorFilter::table_argb(None, Some([0u8; 256]), None, None)
            .expect("table_argb");
        ColorFilter::lerp(0.5, ColorFilter::luma(), ColorFilter::luma())
            .expect("lerp");
    }

    #[test]
    fn a_color_filter_reaches_a_draw_through_paint() {
        // The point of the type. An identity matrix has to leave the colour
        // alone, and a matrix that zeroes the red row has to remove it --
        // which is what proves the twenty numbers are being read row-major
        // rather than transposed.
        let mut drop_red = identity_matrix();
        drop_red[0] = 0.0;

        let sample = |filter: Option<ColorFilter>| {
            let mut canvas = Canvas::new(4.0, 4.0);
            {
                let ctx = canvas.context();
                ctx.set_fill_style(RgbaLinear::opaque(1.0, 0.5, 0.25));
                ctx.set_color_filter(filter.as_ref());
                ctx.fill_rect(0.0, 0.0, 4.0, 4.0);
            }
            let raw = canvas
                .to_buffer(ImageFormat::Raw, &EncodeOptions::default())
                .expect("raw export");
            [raw[0], raw[1], raw[2]]
        };

        let plain = sample(None);
        assert_eq!(
            sample(Some(
                ColorFilter::matrix(identity_matrix()).expect("identity")
            )),
            plain,
            "the identity changes nothing"
        );

        let without_red =
            sample(Some(ColorFilter::matrix(drop_red).expect("drop red")));
        assert_eq!(without_red[0], 0, "the red row was zeroed");
        assert_eq!(
            &without_red[1..],
            &plain[1..],
            "and the other two rows were not"
        );
    }

    #[test]
    fn a_table_maps_every_channel_through_itself() {
        // The case a matrix cannot express: an arbitrary curve. Inverting
        // through a table is `255 - i`, which no linear matrix row does.
        let mut inverted = [0u8; 256];
        for (i, entry) in inverted.iter_mut().enumerate() {
            *entry = 255 - i as u8;
        }

        let mut canvas = Canvas::new(4.0, 4.0);
        {
            let ctx = canvas.context();
            ctx.set_fill_style(RgbaLinear::opaque(1.0, 0.0, 0.0));
            ctx.set_color_filter(Some(
                &ColorFilter::table(inverted).expect("table"),
            ));
            ctx.fill_rect(0.0, 0.0, 4.0, 4.0);
        }
        let raw = canvas
            .to_buffer(ImageFormat::Raw, &EncodeOptions::default())
            .expect("raw export");
        // Opaque red inverts to transparent cyan, alpha included -- a table
        // is applied to all four channels.
        assert_eq!(raw[0], 0, "red inverted");
        assert_eq!(raw[3], 0, "and so did alpha");
    }
}

#[cfg(test)]
mod image_filter_tests {
    use super::*;
    use crate::prelude::*;

    fn area() -> Rect {
        Rect::from_xywh(0.0, 0.0, 16.0, 16.0)
    }

    /// No safe call in this module takes the process down.
    ///
    /// Skia answers a non-finite colour matrix with a null and `skia_safe`
    /// unwraps it, so `matrix` and `hsla_matrix` -- which returned `Self`
    /// and had nowhere to report a failure -- panicked instead. The caller
    /// never had to write a `NaN` to get there: `ColorMatrix::concat` of two
    /// large scalings overflows to infinity from finite arguments, which is
    /// the case that makes this a bug rather than a nicety.
    #[test]
    fn a_matrix_skia_will_not_take_is_an_error_not_a_panic() {
        let mut one_bad = ColorMatrix::identity().into_rows();
        one_bad[3] = f32::NAN;

        // Reached without anyone typing a non-finite number.
        let huge = ColorMatrix::scaled(1e20, 1e20, 1e20, 1.0);
        let overflowed = ColorMatrix::concat(huge, huge).into_rows();
        assert!(
            overflowed.iter().any(|v| !v.is_finite()),
            "the overflow case stopped overflowing; pick larger scalings"
        );

        for matrix in [[f32::NAN; 20], one_bad, overflowed, [f32::INFINITY; 20]]
        {
            assert!(ColorFilter::matrix(matrix).is_err());
            assert!(ColorFilter::hsla_matrix(matrix).is_err());
            assert!(ImageFilter::color_matrix(matrix, None, None).is_err());
        }

        // And a finite one still builds, so the guard did not refuse
        // everything.
        let identity = ColorMatrix::identity().into_rows();
        assert!(ColorFilter::matrix(identity).is_ok());
        assert!(ColorFilter::hsla_matrix(identity).is_ok());
        assert!(ImageFilter::color_matrix(identity, None, None).is_ok());
    }

    /// A convolution refuses a negative dimension rather than clamping it.
    ///
    /// `kernel_width.max(0) as usize` made an empty kernel the expected
    /// length for `-1x-1`, so the empty case passed the length check and
    /// Skia asserted on the signed product it computes itself. The
    /// non-empty case was refused, but for the wrong reason: nine values
    /// for `-3x-3` were told the filter "needs 0 kernel values".
    #[test]
    fn a_convolution_refuses_a_negative_dimension() {
        let build = |w: i32, h: i32, kernel: &[f32]| {
            ImageFilter::matrix_convolution(
                w,
                h,
                kernel,
                1.0,
                0.0,
                0,
                0,
                TileMode::Clamp,
                true,
                None,
                None,
            )
        };

        for (w, h) in [(-1, -1), (-3, -3), (-1, 3), (3, -1), (0, 3), (3, 0)] {
            let refused = build(w, h, &[]).expect_err("should be refused");
            let Error::FilterCreate { reason } = &refused else {
                panic!("expected FilterCreate, got {refused:?}");
            };
            assert!(
                reason.contains("at least 1x1"),
                "{w}x{h} was refused as {reason:?}, which names the clamp                  rather than the negative"
            );
        }
        // The same dimensions with a plausible kernel are refused too, and
        // for the same stated reason rather than a length complaint.
        assert!(build(-3, -3, &[0.0; 9]).is_err());

        // A real kernel still builds.
        assert!(build(3, 3, &[0.0; 9]).is_ok());
        // And a mismatched length is still a length complaint.
        let short = build(3, 3, &[0.0; 8]).expect_err("length mismatch");
        assert!(format!("{short}").contains("9 kernel values"));
    }

    /// The three constructors that were still discarding a crop rect.
    ///
    /// Found by the same sweep that caught `blur`, and `drop_shadow` is the
    /// clearest of them: `drop_shadow_only` -- its own twin, eight lines
    /// away in the source -- has always taken one. Skia's `color_filter`
    /// takes one too, which `color_matrix` and `from_color_filter` both
    /// build on.
    ///
    /// Drawn pixels rather than a `Some` check, for the reason the blur
    /// test gives: a signature that took the argument and dropped it would
    /// still build.
    #[test]
    fn the_last_three_constructors_read_their_crop() {
        // Left half only, so a filter spreading rightward is cut.
        let half = Some(Rect::from_xywh(0.0, 0.0, 16.0, 32.0));

        let drawn = |filter: ImageFilter| {
            let mut canvas = Canvas::new(32.0, 32.0);
            {
                let ctx = canvas.context();
                ctx.set_image_filter(Some(&filter));
                ctx.set_fill_style(RgbaLinear::opaque(1.0, 1.0, 1.0));
                ctx.fill_rect(8.0, 8.0, 16.0, 16.0);
            }
            canvas
                .to_buffer(ImageFormat::Raw, &EncodeOptions::default())
                .expect("raw")
        };

        let white = RgbaLinear::opaque(1.0, 1.0, 1.0);
        let shadow = |crop| {
            ImageFilter::drop_shadow(6.0, 6.0, 3.0, 3.0, white, None, crop)
                .expect("drop_shadow")
        };
        assert_ne!(
            drawn(shadow(None)),
            drawn(shadow(half)),
            "drop_shadow ignored its crop, where drop_shadow_only never did"
        );

        // A colour matrix that tints, so the cropped and uncropped halves
        // cannot coincide.
        let tint = ColorMatrix::scaled(1.0, 0.2, 0.2, 1.0).into_rows();
        let matrix = |crop| {
            ImageFilter::color_matrix(tint, None, crop).expect("color_matrix")
        };
        assert_ne!(drawn(matrix(None)), drawn(matrix(half)));

        let from_filter = |crop| {
            ImageFilter::from_color_filter(ColorFilter::luma(), None, crop)
                .expect("from_color_filter")
        };
        assert_ne!(drawn(from_filter(None)), drawn(from_filter(half)));
    }

    /// A blur's tile mode and crop rect must actually reach Skia.
    ///
    /// Both were hardcoded `None`, and passing them changes the picture
    /// rather than just the call: a crop bounds the output, and a tile mode
    /// says what the kernel reads past the input's edge. Compared as drawn
    /// pixels, because a constructor that accepted the arguments and
    /// discarded them would still build.
    #[test]
    fn a_blur_reads_its_tile_mode_and_its_crop() {
        let drawn = |tile: Option<TileMode>, crop: Option<Rect>| {
            let filter = ImageFilter::blur(4.0, 4.0, tile, None, crop)
                .expect("a blur builds");
            let mut canvas = Canvas::new(32.0, 32.0);
            {
                let ctx = canvas.context();
                ctx.set_image_filter(Some(&filter));
                ctx.set_fill_style(RgbaLinear::opaque(1.0, 1.0, 1.0));
                // Inset, so the blur has somewhere to spread and the crop
                // has something to cut off.
                ctx.fill_rect(8.0, 8.0, 16.0, 16.0);
            }
            canvas
                .to_buffer(ImageFormat::Raw, &EncodeOptions::default())
                .expect("raw")
        };

        let plain = drawn(None, None);

        // A crop rect bounds the filter's output, so half the blur is gone.
        let cropped = drawn(None, Some(Rect::from_xywh(0.0, 0.0, 16.0, 32.0)));
        assert_ne!(plain, cropped, "the crop rect changed nothing");

        // And the tile mode decides what lies past the input's edge.
        // `Decal` is the default, so it should match; `Clamp` should not.
        assert_eq!(
            plain,
            drawn(Some(TileMode::Decal), None),
            "None should mean Decal, which is Skia's default and MakeBlur's"
        );
        assert_ne!(
            plain,
            drawn(Some(TileMode::Clamp), None),
            "the tile mode changed nothing"
        );
    }

    #[test]
    fn every_constructor_builds() {
        // Each of these turns Skia's `None` into a named error, so what is
        // checked is that ordinary arguments do not take that path. The
        // names are in the error, which is why they are worth having.
        let white = RgbaLinear::opaque(1.0, 1.0, 1.0);
        let light = Point3::new(0.0, 0.0, 10.0);
        let blur = ImageFilter::blur(2.0, 2.0, None, None, None).expect("blur");

        ImageFilter::arithmetic(1.0, 0.0, 0.0, 0.0, true, None, None, None)
            .expect("arithmetic");
        ImageFilter::blend(BlendMode::Multiply, None, None, None)
            .expect("blend");
        ImageFilter::crop(area(), TileMode::Clamp, None).expect("crop");
        ImageFilter::dilate(2.0, 2.0, None, None).expect("dilate");
        ImageFilter::erode(2.0, 2.0, None, None).expect("erode");
        ImageFilter::displacement_map(
            ColorChannel::Red,
            ColorChannel::Green,
            4.0,
            Some(blur.clone()),
            None,
            None,
        )
        .expect("displacement_map");
        ImageFilter::distant_lit_diffuse(light, white, 1.0, 1.0, None, None)
            .expect("distant_lit_diffuse");
        ImageFilter::distant_lit_specular(
            light, white, 1.0, 1.0, 8.0, None, None,
        )
        .expect("distant_lit_specular");
        ImageFilter::point_lit_diffuse(light, white, 1.0, 1.0, None, None)
            .expect("point_lit_diffuse");
        ImageFilter::point_lit_specular(
            light, white, 1.0, 1.0, 8.0, None, None,
        )
        .expect("point_lit_specular");
        ImageFilter::spot_lit_diffuse(
            light,
            Point3::default(),
            1.0,
            45.0,
            white,
            1.0,
            1.0,
            None,
            None,
        )
        .expect("spot_lit_diffuse");
        ImageFilter::spot_lit_specular(
            light,
            Point3::default(),
            1.0,
            45.0,
            white,
            1.0,
            1.0,
            8.0,
            None,
            None,
        )
        .expect("spot_lit_specular");
        ImageFilter::drop_shadow_only(2.0, 2.0, 1.0, 1.0, white, None, None)
            .expect("drop_shadow_only");
        ImageFilter::empty();
        ImageFilter::magnifier(
            area(),
            2.0,
            1.0,
            SamplingMode::Linear,
            None,
            None,
        )
        .expect("magnifier");
        ImageFilter::matrix_convolution(
            1,
            1,
            &[1.0],
            1.0,
            0.0,
            0,
            0,
            TileMode::Clamp,
            true,
            None,
            None,
        )
        .expect("matrix_convolution");
        ImageFilter::matrix_transform(
            Affine::default(),
            SamplingMode::Linear,
            None,
        )
        .expect("matrix_transform");
        ImageFilter::merge(vec![None, Some(blur.clone())], None)
            .expect("merge");
        ImageFilter::offset(4.0, 4.0, None, None).expect("offset");
        ImageFilter::tile(area(), area(), None).expect("tile");
    }

    #[test]
    fn a_convolution_kernel_must_match_the_size_it_declares() {
        // Skia reads `width * height` values off the slice rather than
        // checking it, so a short kernel is a read past the end. Refused
        // here, where both numbers are in view.
        let refused = ImageFilter::matrix_convolution(
            3,
            3,
            &[1.0, 1.0],
            1.0,
            0.0,
            1,
            1,
            TileMode::Clamp,
            true,
            None,
            None,
        )
        .expect_err("two values cannot fill a 3x3 kernel");
        assert!(
            format!("{refused}").contains("needs 9 kernel values, got 2"),
            "{refused}"
        );

        // And the right number is accepted.
        assert!(
            ImageFilter::matrix_convolution(
                3,
                3,
                &[0.0; 9],
                1.0,
                0.0,
                1,
                1,
                TileMode::Clamp,
                true,
                None,
                None,
            )
            .is_ok()
        );
    }

    #[test]
    fn a_filter_reaches_a_draw_and_changes_it() {
        // Constructing proves nothing on its own. `offset` is the one whose
        // effect can be checked with a single pixel: a square shifted right
        // by half its width leaves its old left edge empty.
        let sample = |filter: Option<ImageFilter>| {
            let mut canvas = Canvas::new(16.0, 16.0);
            {
                let ctx = canvas.context();
                ctx.set_image_filter(filter.as_ref());
                ctx.set_fill_style(RgbaLinear::opaque(1.0, 0.0, 0.0));
                ctx.fill_rect(0.0, 0.0, 8.0, 16.0);
            }
            canvas
                .to_buffer(ImageFormat::Raw, &EncodeOptions::default())
                .expect("raw export")
        };

        let plain = sample(None);
        let shifted = sample(Some(
            ImageFilter::offset(8.0, 0.0, None, None).expect("offset"),
        ));

        // Pixel (1, 0) is inside the square before the shift and outside
        // after it; (9, 0) is the reverse.
        let alpha_at = |raw: &[u8], x: usize| raw[x * 4 + 3];
        assert_eq!(alpha_at(&plain, 1), 255);
        assert_eq!(alpha_at(&shifted, 1), 0, "the square moved off this pixel");
        assert_eq!(alpha_at(&plain, 9), 0);
        assert_eq!(alpha_at(&shifted, 9), 255, "and onto this one");
    }
}
