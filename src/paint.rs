use skia_safe::{
    BlendMode as SkBlendMode, ColorSpace as SkColorSpace, Paint as SkPaint,
    PaintCap, PaintStyle as SkPaintStyle, dash_path_effect,
};

use crate::{
    color::{RgbaLinear, rgba_linear_to_unpremul_color4f},
    filter::{ColorFilter, ImageFilter, MaskFilter},
    shader::Shader,
};

/// Whether a draw fills its geometry or strokes its outline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PaintStyle {
    /// Fills the interior. The default.
    #[default]
    Fill,
    /// Strokes the outline at `stroke_width`.
    Stroke,
}

/// How the ends of an open stroked path are drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum StrokeCap {
    /// Ends exactly at the endpoint. The default.
    #[default]
    Butt,
    /// Extends by a half-width semicircle.
    Round,
    /// Extends by a half-width square.
    Square,
}

impl StrokeCap {
    pub(crate) fn from_skia(cap: PaintCap) -> Self {
        match cap {
            PaintCap::Round => Self::Round,
            PaintCap::Square => Self::Square,
            _ => Self::Butt,
        }
    }

    pub(crate) fn to_skia(self) -> PaintCap {
        match self {
            Self::Butt => PaintCap::Butt,
            Self::Round => PaintCap::Round,
            Self::Square => PaintCap::Square,
        }
    }
}

/// How two stroked segments are joined where they meet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum StrokeJoin {
    /// Extends the outer edges until they meet in a point. The default.
    ///
    /// A join sharper than the miter limit falls back to [`Bevel`], so a
    /// near-parallel pair cannot produce an unbounded spike.
    ///
    /// [`Bevel`]: StrokeJoin::Bevel
    #[default]
    Miter,
    /// Fills the gap with a half-width arc.
    Round,
    /// Cuts the corner off with a straight edge.
    Bevel,
}

impl StrokeJoin {
    pub(crate) fn from_skia(join: skia_safe::paint::Join) -> Self {
        match join {
            skia_safe::paint::Join::Round => Self::Round,
            skia_safe::paint::Join::Bevel => Self::Bevel,
            _ => Self::Miter,
        }
    }

    pub(crate) fn to_skia(self) -> skia_safe::paint::Join {
        match self {
            Self::Miter => skia_safe::paint::Join::Miter,
            Self::Round => skia_safe::paint::Join::Round,
            Self::Bevel => skia_safe::paint::Join::Bevel,
        }
    }
}

/// An on/off dash pattern for stroked paths.
#[derive(Debug, Clone, PartialEq)]
pub struct DashPattern {
    /// Alternating on and off lengths in pixels, starting with on.
    ///
    /// Skia requires an even count of at least two. An odd or empty count is
    /// rejected, and the stroke is then drawn solid with no error.
    pub intervals: Vec<f32>,
    /// How far into the pattern the first dash starts, in pixels.
    pub phase: f32,
}

/// Canvas-compatible blend modes, plus `PlusLighter`.
///
/// These are the values `globalCompositeOperation` accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BlendMode {
    /// Draws the source over the destination. The default.
    #[default]
    SourceOver,
    /// Keeps the source only where the destination is opaque.
    SourceIn,
    /// Keeps the source only where the destination is transparent.
    SourceOut,
    /// Draws the source over the destination, clipped to it.
    SourceAtop,
    /// Draws the source behind the destination.
    DestinationOver,
    /// Keeps the destination only where the source is opaque.
    DestinationIn,
    /// Keeps the destination only where the source is transparent.
    DestinationOut,
    /// Keeps the destination where the source is, then draws the source behind
    /// it.
    DestinationAtop,
    /// Replaces the destination with the source, alpha included.
    Copy,
    /// Keeps whichever of source and destination the other does not cover.
    Xor,
    /// Multiplies source and destination, always darkening.
    Multiply,
    /// Inverse of `Multiply`, always lightening.
    Screen,
    /// `Multiply` on dark backdrops, `Screen` on light ones.
    Overlay,
    /// Keeps the darker of the two per channel.
    Darken,
    /// Keeps the lighter of the two per channel.
    Lighten,
    /// Brightens the destination to reflect the source.
    ColorDodge,
    /// Darkens the destination to reflect the source.
    ColorBurn,
    /// `Overlay` with source and destination swapped.
    HardLight,
    /// A gentler `HardLight`.
    SoftLight,
    /// Absolute per-channel difference.
    Difference,
    /// Like `Difference` with lower contrast.
    Exclusion,
    /// Source hue, destination saturation and luminosity.
    Hue,
    /// Source saturation, destination hue and luminosity.
    Saturation,
    /// Source hue and saturation, destination luminosity.
    Color,
    /// Source luminosity, destination hue and saturation.
    Luminosity,
    /// Adds source and destination, clamped. CSS `plus-lighter`.
    PlusLighter,
    /// `R = 0` -- clears the destination within the draw.
    ///
    /// CanvasKit `BlendMode.Clear`; absent from the CSS
    /// `globalCompositeOperation` set.
    Clear,
    /// `R = S * D` per channel.
    ///
    /// CanvasKit `BlendMode.Modulate` (not the same as `Multiply`, which
    /// composites over the backdrop).
    Modulate,
    /// `R = D` -- keeps the destination, ignores the source. CanvasKit
    /// `BlendMode.Dst`.
    Destination,
}

impl BlendMode {
    pub(crate) fn from_skia(mode: SkBlendMode) -> Self {
        match mode {
            SkBlendMode::SrcOver => Self::SourceOver,
            SkBlendMode::SrcIn => Self::SourceIn,
            SkBlendMode::SrcOut => Self::SourceOut,
            SkBlendMode::SrcATop => Self::SourceAtop,
            SkBlendMode::DstOver => Self::DestinationOver,
            SkBlendMode::DstIn => Self::DestinationIn,
            SkBlendMode::DstOut => Self::DestinationOut,
            SkBlendMode::DstATop => Self::DestinationAtop,
            SkBlendMode::Src => Self::Copy,
            SkBlendMode::Xor => Self::Xor,
            SkBlendMode::Multiply => Self::Multiply,
            SkBlendMode::Screen => Self::Screen,
            SkBlendMode::Overlay => Self::Overlay,
            SkBlendMode::Darken => Self::Darken,
            SkBlendMode::Lighten => Self::Lighten,
            SkBlendMode::ColorDodge => Self::ColorDodge,
            SkBlendMode::ColorBurn => Self::ColorBurn,
            SkBlendMode::HardLight => Self::HardLight,
            SkBlendMode::SoftLight => Self::SoftLight,
            SkBlendMode::Difference => Self::Difference,
            SkBlendMode::Exclusion => Self::Exclusion,
            SkBlendMode::Hue => Self::Hue,
            SkBlendMode::Saturation => Self::Saturation,
            SkBlendMode::Color => Self::Color,
            SkBlendMode::Luminosity => Self::Luminosity,
            SkBlendMode::Plus => Self::PlusLighter,
            SkBlendMode::Clear => Self::Clear,
            SkBlendMode::Modulate => Self::Modulate,
            SkBlendMode::Dst => Self::Destination,
        }
    }

    pub(crate) fn to_skia(self) -> SkBlendMode {
        match self {
            Self::SourceOver => SkBlendMode::SrcOver,
            Self::SourceIn => SkBlendMode::SrcIn,
            Self::SourceOut => SkBlendMode::SrcOut,
            Self::SourceAtop => SkBlendMode::SrcATop,
            Self::DestinationOver => SkBlendMode::DstOver,
            Self::DestinationIn => SkBlendMode::DstIn,
            Self::DestinationOut => SkBlendMode::DstOut,
            Self::DestinationAtop => SkBlendMode::DstATop,
            Self::Copy => SkBlendMode::Src,
            Self::Xor => SkBlendMode::Xor,
            Self::Multiply => SkBlendMode::Multiply,
            Self::Screen => SkBlendMode::Screen,
            Self::Overlay => SkBlendMode::Overlay,
            Self::Darken => SkBlendMode::Darken,
            Self::Lighten => SkBlendMode::Lighten,
            Self::ColorDodge => SkBlendMode::ColorDodge,
            Self::ColorBurn => SkBlendMode::ColorBurn,
            Self::HardLight => SkBlendMode::HardLight,
            Self::SoftLight => SkBlendMode::SoftLight,
            Self::Difference => SkBlendMode::Difference,
            Self::Exclusion => SkBlendMode::Exclusion,
            Self::Hue => SkBlendMode::Hue,
            Self::Saturation => SkBlendMode::Saturation,
            Self::Color => SkBlendMode::Color,
            Self::Luminosity => SkBlendMode::Luminosity,
            // Skia exposes the additive mode as `SkBlendMode::Plus`; this is
            // the same R = a + b operation Canvas calls `plus-lighter`.
            Self::PlusLighter => SkBlendMode::Plus,
            Self::Clear => SkBlendMode::Clear,
            Self::Modulate => SkBlendMode::Modulate,
            Self::Destination => SkBlendMode::Dst,
        }
    }
}

/// Mutable paint state used by every
/// [`DrawTarget`](crate::recorder::DrawTarget) drawing method.
///
/// A single paint carries either fill or stroke style, never both. To render
/// both, issue two draws with two paints, as Skia's `SkPaint` requires.
#[derive(Debug, Clone)]
pub struct Paint {
    /// Color to fill or stroke with.
    ///
    /// Where a [`Shader`] is set its RGB is ignored, but the alpha still
    /// modulates the shader output.
    pub color: RgbaLinear,
    /// Fill or stroke.
    pub style: PaintStyle,
    /// Stroke width in pixels. Only used when `style` is
    /// [`PaintStyle::Stroke`].
    ///
    /// `0.0` is Skia's hairline sentinel: it draws a one-device-pixel line
    /// regardless of the current transform, not an invisible stroke.
    pub stroke_width: f32,
    /// How open stroke ends are capped.
    pub stroke_cap: StrokeCap,
    /// Dash pattern, or `None` for a solid stroke.
    pub dash: Option<DashPattern>,
    /// Whether edges are antialiased. On by default.
    pub anti_alias: bool,
    /// Opacity multiplier applied on top of `color`, clamped to
    /// `0.0..=1.0`.
    pub alpha: f32,
    /// How the result composites against what is already there.
    pub blend_mode: BlendMode,
    /// [`Shader`] supplying color per pixel, overriding `color`.
    pub shader: Option<Shader>,
    /// Filter applied to the rasterized result, such as a blur.
    pub image_filter: Option<ImageFilter>,
    /// Filter applied to each color before compositing.
    pub color_filter: Option<ColorFilter>,
    /// Coverage-mask filter (styled blur).
    ///
    /// Applied before rasterization for glows, feathered edges, and outline
    /// blurs.
    pub mask_filter: Option<MaskFilter>,
    /// Dither the paint to break up banding in gradients and dark frames on
    /// 8-bit surfaces.
    ///
    /// Mirrors CanvasKit's `Paint.setDither`.
    pub dither: bool,
}

impl Default for Paint {
    fn default() -> Self {
        Self {
            color: RgbaLinear::opaque(0.0, 0.0, 0.0),
            style: PaintStyle::Fill,
            stroke_width: 1.0,
            stroke_cap: StrokeCap::Butt,
            dash: None,
            anti_alias: true,
            alpha: 1.0,
            blend_mode: BlendMode::SourceOver,
            shader: None,
            image_filter: None,
            color_filter: None,
            mask_filter: None,
            dither: false,
        }
    }
}

impl Paint {
    /// Creates a fill paint in `color`, with every other field at its
    /// default.
    ///
    /// # Examples
    ///
    /// ```
    /// use meo_skia_canvas::prelude::*;
    ///
    /// let paint = Paint::fill(RgbaLinear::opaque(1.0, 0.0, 0.0));
    /// assert_eq!(paint.style, PaintStyle::Fill);
    /// ```
    pub fn fill(color: RgbaLinear) -> Self {
        Self {
            color,
            style: PaintStyle::Fill,
            ..Self::default()
        }
    }

    /// Creates a stroke paint in `color`, `width` pixels wide.
    pub fn stroke(color: RgbaLinear, width: f32) -> Self {
        Self {
            color,
            style: PaintStyle::Stroke,
            stroke_width: width,
            ..Self::default()
        }
    }

    /// Sets the fill or stroke color.
    ///
    /// Returns `&mut Self` so calls can be chained.
    pub fn set_color(&mut self, color: RgbaLinear) -> &mut Self {
        self.color = color;
        self
    }

    /// Sets the opacity multiplier, clamped to `0.0..=1.0`.
    ///
    /// Returns `&mut Self` so calls can be chained.
    pub fn set_alpha(&mut self, alpha: f32) -> &mut Self {
        self.alpha = alpha.clamp(0.0, 1.0);
        self
    }

    /// Sets how the result composites against the destination.
    ///
    /// Returns `&mut Self` so calls can be chained.
    pub fn set_blend_mode(&mut self, mode: BlendMode) -> &mut Self {
        self.blend_mode = mode;
        self
    }

    /// Sets whether the paint fills or strokes.
    ///
    /// Returns `&mut Self` so calls can be chained.
    pub fn set_style(&mut self, style: PaintStyle) -> &mut Self {
        self.style = style;
        self
    }

    /// Sets the stroke width in pixels.
    ///
    /// Returns `&mut Self` so calls can be chained.
    pub fn set_stroke_width(&mut self, width: f32) -> &mut Self {
        self.stroke_width = width;
        self
    }

    /// Sets how open stroke ends are capped.
    ///
    /// Returns `&mut Self` so calls can be chained.
    pub fn set_stroke_cap(&mut self, cap: StrokeCap) -> &mut Self {
        self.stroke_cap = cap;
        self
    }

    /// Sets a dash pattern on stroked paths.
    ///
    /// Silently ignored if Skia rejects the pattern -- an empty
    /// `intervals`, or one summing to zero, leaves the stroke solid.
    ///
    /// Returns `&mut Self` so calls can be chained.
    pub fn set_dash(&mut self, intervals: Vec<f32>, phase: f32) -> &mut Self {
        self.dash = Some(DashPattern { intervals, phase });
        self
    }

    /// Removes any dash pattern, making strokes solid again.
    ///
    /// Returns `&mut Self` so calls can be chained.
    pub fn clear_dash(&mut self) -> &mut Self {
        self.dash = None;
        self
    }

    /// Enables or disables edge antialiasing.
    ///
    /// Returns `&mut Self` so calls can be chained.
    pub fn set_anti_alias(&mut self, enabled: bool) -> &mut Self {
        self.anti_alias = enabled;
        self
    }

    /// Sets a shader to supply color per pixel, or clears it.
    ///
    /// Returns `&mut Self` so calls can be chained.
    pub fn set_shader(&mut self, shader: Option<Shader>) -> &mut Self {
        self.shader = shader;
        self
    }

    /// Sets a filter applied to the rasterized result, or clears it.
    ///
    /// Returns `&mut Self` so calls can be chained.
    pub fn set_image_filter(
        &mut self,
        filter: Option<ImageFilter>,
    ) -> &mut Self {
        self.image_filter = filter;
        self
    }

    /// Sets a filter applied to each color before compositing, or clears it.
    ///
    /// Returns `&mut Self` so calls can be chained.
    pub fn set_color_filter(
        &mut self,
        filter: Option<ColorFilter>,
    ) -> &mut Self {
        self.color_filter = filter;
        self
    }

    /// Sets a coverage-mask filter such as a styled blur, or clears it.
    ///
    /// Returns `&mut Self` so calls can be chained.
    pub fn set_mask_filter(&mut self, filter: Option<MaskFilter>) -> &mut Self {
        self.mask_filter = filter;
        self
    }

    /// Enables or disables dithering.
    ///
    /// Returns `&mut Self` so calls can be chained.
    pub fn set_dither(&mut self, enabled: bool) -> &mut Self {
        self.dither = enabled;
        self
    }

    pub(crate) fn to_skia_paint(
        &self,
        working_color_space: &SkColorSpace,
    ) -> SkPaint {
        let mut paint = SkPaint::default();
        let modulated = self.color.with_opacity(self.alpha);
        let unpremul = rgba_linear_to_unpremul_color4f(modulated);
        // Tag the `Color4f` with the destination's working color space.
        // Without this, Skia applies its default "decode as sRGB" pass
        // and gamma-decodes our already-linear values a second time.
        // Tagging with the surface's working space matches the
        // `RgbaLinear`-as-working-space convention.
        paint.set_color4f(unpremul, Some(working_color_space));
        paint.set_style(match self.style {
            PaintStyle::Fill => SkPaintStyle::Fill,
            PaintStyle::Stroke => SkPaintStyle::Stroke,
        });
        paint.set_stroke_width(self.stroke_width);
        paint.set_stroke_cap(self.stroke_cap.to_skia());
        paint.set_anti_alias(self.anti_alias);
        paint.set_blend_mode(self.blend_mode.to_skia());
        if let Some(dash) = &self.dash
            && let Some(effect) =
                dash_path_effect::new(&dash.intervals, dash.phase)
        {
            paint.set_path_effect(effect);
        }
        if let Some(shader) = &self.shader {
            paint.set_shader(shader.inner.clone());
        }
        if let Some(filter) = &self.image_filter {
            paint.set_image_filter(filter.inner.clone());
        }
        if let Some(filter) = &self.color_filter {
            paint.set_color_filter(filter.inner.clone());
        }
        if let Some(filter) = &self.mask_filter {
            paint.set_mask_filter(filter.inner.clone());
        }
        paint.set_dither(self.dither);
        paint
    }
}
