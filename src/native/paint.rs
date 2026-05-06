use skia_safe::{
    BlendMode as SkBlendMode, ColorSpace as SkColorSpace, Paint as SkPaint, PaintCap,
    PaintStyle as SkPaintStyle, dash_path_effect,
};

use crate::native::color::{RgbaLinear, rgba_linear_to_unpremul_color4f};
use crate::native::filter::{NativeColorFilter, NativeImageFilter};
use crate::native::shader::NativeShader;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PaintStyle {
    #[default]
    Fill,
    Stroke,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum StrokeCap {
    #[default]
    Butt,
    Round,
    Square,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DashPattern {
    pub intervals: Vec<f32>,
    pub phase: f32,
}

/// Canvas-compatible blend modes plus `PlusLighter`. Mirrors the
/// `globalCompositeOperation` set used by `@phyron/studio-renderer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BlendMode {
    #[default]
    SourceOver,
    SourceIn,
    SourceOut,
    SourceAtop,
    DestinationOver,
    DestinationIn,
    DestinationOut,
    DestinationAtop,
    Copy,
    Xor,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    Hue,
    Saturation,
    Color,
    Luminosity,
    PlusLighter,
}

impl BlendMode {
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
        }
    }
}

/// Mutable paint state used by all `NativeCanvas` drawing methods. Mirrors
/// the renderer-side paint accumulator from `@phyron/studio-renderer`. A
/// single paint instance carries either fill or stroke style; to render
/// both, issue two draws with two paints (matches Skia's `SkPaint`).
#[derive(Debug, Clone)]
pub struct NativePaint {
    pub color: RgbaLinear,
    pub style: PaintStyle,
    pub stroke_width: f32,
    pub stroke_cap: StrokeCap,
    pub dash: Option<DashPattern>,
    pub anti_alias: bool,
    pub alpha: f32,
    pub blend_mode: BlendMode,
    pub shader: Option<NativeShader>,
    pub image_filter: Option<NativeImageFilter>,
    pub color_filter: Option<NativeColorFilter>,
}

impl Default for NativePaint {
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
        }
    }
}

impl NativePaint {
    pub fn fill(color: RgbaLinear) -> Self {
        Self {
            color,
            style: PaintStyle::Fill,
            ..Self::default()
        }
    }

    pub fn stroke(color: RgbaLinear, width: f32) -> Self {
        Self {
            color,
            style: PaintStyle::Stroke,
            stroke_width: width,
            ..Self::default()
        }
    }

    pub fn set_color(&mut self, color: RgbaLinear) -> &mut Self {
        self.color = color;
        self
    }

    pub fn set_alpha(&mut self, alpha: f32) -> &mut Self {
        self.alpha = alpha.clamp(0.0, 1.0);
        self
    }

    pub fn set_blend_mode(&mut self, mode: BlendMode) -> &mut Self {
        self.blend_mode = mode;
        self
    }

    pub fn set_style(&mut self, style: PaintStyle) -> &mut Self {
        self.style = style;
        self
    }

    pub fn set_stroke_width(&mut self, width: f32) -> &mut Self {
        self.stroke_width = width;
        self
    }

    pub fn set_stroke_cap(&mut self, cap: StrokeCap) -> &mut Self {
        self.stroke_cap = cap;
        self
    }

    pub fn set_dash(&mut self, intervals: Vec<f32>, phase: f32) -> &mut Self {
        self.dash = Some(DashPattern { intervals, phase });
        self
    }

    pub fn clear_dash(&mut self) -> &mut Self {
        self.dash = None;
        self
    }

    pub fn set_anti_alias(&mut self, enabled: bool) -> &mut Self {
        self.anti_alias = enabled;
        self
    }

    pub fn set_shader(&mut self, shader: Option<NativeShader>) -> &mut Self {
        self.shader = shader;
        self
    }

    pub fn set_image_filter(&mut self, filter: Option<NativeImageFilter>) -> &mut Self {
        self.image_filter = filter;
        self
    }

    pub fn set_color_filter(&mut self, filter: Option<NativeColorFilter>) -> &mut Self {
        self.color_filter = filter;
        self
    }

    pub(crate) fn to_skia_paint(&self, working_color_space: &SkColorSpace) -> SkPaint {
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
        paint.set_stroke_cap(match self.stroke_cap {
            StrokeCap::Butt => PaintCap::Butt,
            StrokeCap::Round => PaintCap::Round,
            StrokeCap::Square => PaintCap::Square,
        });
        paint.set_anti_alias(self.anti_alias);
        paint.set_blend_mode(self.blend_mode.to_skia());
        if let Some(dash) = &self.dash
            && let Some(effect) = dash_path_effect::new(&dash.intervals, dash.phase)
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
        paint
    }
}
