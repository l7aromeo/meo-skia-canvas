//
// Font collection management
//
#![allow(non_snake_case)]
use neon::{prelude::*, types::buffer::TypedArray};
use std::{
    cell::RefCell, collections::HashMap, fs, path::Path, sync::OnceLock,
};

use skia_safe::{
    FontArguments, FontMgr, FourByteTag, Typeface,
    font_arguments::{VariationPosition, variation_position::Coordinate},
    font_style::{FontStyle, Slant},
    textlayout::{FontCollection, TextStyle, TypefaceFontProvider},
    utils::OrderedFontMgr,
};

use crate::{
    typography::{
        FontSpec, from_slant, from_width, typeface_details, typeface_wght_range,
    },
    utils::*,
};

#[cfg(target_os = "windows")]
use allsorts::{
    binary::read::ReadScope, subset::whole_font, tables::FontTableProvider,
    woff::WoffFont, woff2::Woff2Font,
};

/// The CSS `font-stretch` percentage an OpenType `usWidthClass` names.
///
/// The nine classes of the OS/2 table, which CSS Fonts 4 defines the
/// keywords against: `ultra-condensed` through `ultra-expanded`, with
/// `normal` in the middle. The `wdth` variation axis is measured in these
/// percentages, which is why the lookup is here rather than a ratio applied
/// somewhere else.
///
/// The table is spelled out rather than computed. The steps are not uniform
/// -- 12.5 apart at the condensed end, then 12.5, then 25, then 50 -- so any
/// formula would be a worse way of writing nine numbers from a standard.
const WIDTH_CLASS_PERCENT: [f32; 9] =
    [50.0, 62.5, 75.0, 87.5, 100.0, 112.5, 125.0, 150.0, 200.0];

/// [`WIDTH_CLASS_PERCENT`] for `class`, or the normal width for a class the
/// standard does not define.
///
/// `usWidthClass` runs 1 to 9. Class 5 is normal, and used to share a
/// catch-all arm with every out-of-range value -- so a font declaring class
/// 0 or 12 was treated as normal without the reader of that arm being able
/// to tell the legitimate case from the malformed one. Same answer, because
/// normal is the only sensible fallback, and now it is one the code says on
/// purpose.
fn stretch_percent(class: i32) -> f32 {
    const NORMAL: f32 = WIDTH_CLASS_PERCENT[4];
    usize::try_from(class)
        .ok()
        .and_then(|class| class.checked_sub(1))
        .and_then(|index| WIDTH_CLASS_PERCENT.get(index))
        .copied()
        .unwrap_or(NORMAL)
}

thread_local!(
    static LIBRARY: OnceLock<RefCell<FontLibrary>> = const { OnceLock::new() };
);

#[derive(PartialEq, Eq, Hash)]

pub struct CollectionKey {
    families: String,
    weight: i32,
    /// Part of the key because a variable font is now instanced at the `wdth`
    /// axis the width asks for. Without it the first width to be asked for
    /// cached a collection the rest reused, so `fontStretch` moved nothing.
    width: i32,
    slant: Slant,
    /// Axis values, fixed-point at [`AXIS_KEY_STEPS`] per unit.
    ///
    /// An integer because the key is hashed and `f32` is not [`Hash`], and
    /// quantized rather than transmuted because two axis values a
    /// ten-thousandth apart instance the same typeface and should not cache
    /// two of it.
    variations: Vec<(u32, i32)>,
}

/// Steps per axis unit in a [`CollectionKey`].
///
/// A thousandth of a unit is finer than any axis this can be asked for:
/// `wght` runs 1 to 1000 and `wdth` 50 to 200, so this distinguishes far more
/// positions than a typeface has distinguishable instances. Coarser would
/// merge two the caller can tell apart; finer would only cache duplicates.
const AXIS_KEY_STEPS: f32 = 1000.0;

impl CollectionKey {
    pub fn new(style: &TextStyle, variations: &[(FourByteTag, f32)]) -> Self {
        let families = style.font_families();
        let families = families.iter().collect::<Vec<&str>>().join(", ");
        let weight = *style.font_style().weight();
        let width = *style.font_style().width();
        let slant = style.font_style().slant();
        // Rounded, not truncated. Truncation is one-sided, so the step either
        // side of a whole number is not the same size -- and the `as` cast
        // rounds toward zero, which makes it asymmetric about zero as well.
        let variations = variations
            .iter()
            .map(|(tag, val)| (**tag, (*val * AXIS_KEY_STEPS).round() as i32))
            .collect();
        CollectionKey {
            families,
            weight,
            width,
            slant,
            variations,
        }
    }
}

pub struct FontLibrary {
    mgr: FontMgr,
    collection: Option<FontCollection>,
    fonts: Vec<(Typeface, Option<String>)>,
    generics_cache: Vec<(Typeface, Option<String>)>,
    collection_cache: HashMap<CollectionKey, FontCollection>,
    collection_hinted: bool,
}

impl FontLibrary {
    pub fn with_shared<T, F>(f: F) -> T
    where
        F: FnOnce(&mut FontLibrary) -> T,
    {
        LIBRARY.with(|lib_lock| {
            let shared_lib = lib_lock.get_or_init(|| {
                // detect linux systems without a working fontconfig setup and
                // use the fallback config in `lib/fonts` instead
                #[cfg(target_os = "linux")]
                {
                    let has_config =
                        fs::exists(Path::new("/etc/fonts/fonts.conf"))
                            .unwrap_or(false);
                    let has_override = std::env::var_os("FONTCONFIG_PATH")
                        .is_some()
                        || std::env::var_os("FONTCONFIG_FILE").is_some();
                    if !(has_config || has_override)
                        && let Some(mut fallback_config_path) =
                            process_path::get_dylib_path()
                    {
                        fallback_config_path.set_file_name("fonts");
                        // SAFETY: This is called during single-threaded library
                        // initialization
                        unsafe {
                            std::env::set_var(
                                "FONTCONFIG_PATH",
                                fallback_config_path,
                            )
                        };
                    }
                }

                RefCell::new(FontLibrary {
                    mgr: FontMgr::default(),
                    fonts: vec![],
                    collection: None,
                    collection_cache: HashMap::new(),
                    collection_hinted: false,
                    generics_cache: vec![],
                })
            });

            f(&mut shared_lib.borrow_mut())
        })
    }

    pub fn font_collection(&mut self) -> FontCollection {
        // lazily initialize font collection on first actual use
        if self.collection.is_none() {
            self.collection = Some(self.new_font_collection());
        };

        // SAFETY: `collection` was set to `Some` on line 109 above.
        self.collection.as_ref().unwrap().clone()
    }

    fn new_font_collection(&mut self) -> FontCollection {
        let mut assets = TypefaceFontProvider::new();
        for (font, alias) in self.generics() {
            assets.register_typeface(font.clone(), alias.as_deref());
        }
        for (font, alias) in &self.fonts {
            assets.register_typeface(font.clone(), alias.as_deref());
        }

        let mut style_set = assets.match_family("system-ui");
        let default_fam = match style_set.count() > 1 {
            true => style_set.match_style(FontStyle::default()),
            false => self.mgr.legacy_make_typeface(None, FontStyle::default()),
        }
        .map(|f| f.family_name());

        let mut collection = FontCollection::new();
        collection
            .set_default_font_manager(self.mgr.clone(), default_fam.as_deref());
        collection.set_asset_font_manager(Some(assets.into()));
        collection
    }

    fn generics(&mut self) -> &Vec<(Typeface, Option<String>)> {
        // set up generic font family mappings
        if self.generics_cache.is_empty() {
            let mut generics = vec![];
            let mut font_stacks = HashMap::new();
            font_stacks.insert(
                "serif",
                vec![
                    "Times",
                    "Nimbus Roman",
                    "Times New Roman",
                    "Tinos",
                    "Noto Serif",
                    "Liberation Serif",
                    "DejaVu Serif",
                    "Source Serif Pro",
                ],
            );
            font_stacks.insert(
                "sans-serif",
                vec![
                    "Avenir Next",
                    "Avenir",
                    "Helvetica Neue",
                    "Helvetica",
                    "Arial Nova",
                    "Arial",
                    "Inter",
                    "Arimo",
                    "Roboto",
                    "Noto Sans",
                    "Liberation Sans",
                    "DejaVu Sans",
                    "Nimbus Sans",
                    "Clear Sans",
                    "Lato",
                    "Cantarell",
                    "Arimo",
                    "Ubuntu",
                ],
            );
            font_stacks.insert(
                "monospace",
                vec![
                    "Cascadia Code",
                    "Source Code Pro",
                    "Menlo",
                    "Consolas",
                    "Monaco",
                    "Liberation Mono",
                    "Ubuntu Mono",
                    "Roboto Mono",
                    "Lucida Console",
                    "Monaco",
                    "Courier New",
                    "Courier",
                ],
            );
            font_stacks.insert(
                "system-ui",
                vec![
                    "Helvetica Neue",
                    "Ubuntu",
                    "Segoe UI",
                    "Fira Sans",
                    "Roboto",
                    "DroidSans",
                    "Tahoma",
                ],
            );
            // see also: https://modernfontstacks.com | https://systemfontstack.com | https://www.ctrl.blog/entry/font-stack-text.html

            // Set up mappings for generic font names based on the first match
            // found on the system
            for (generic_name, families) in font_stacks.into_iter() {
                let best_match = families.iter().find_map(|fam| {
                    let mut style_set = self.mgr.match_family(fam);
                    match style_set.count() > 0 {
                        true => Some(style_set),
                        false => None,
                    }
                });

                let alias = Some(generic_name.to_string());
                if let Some(mut style_set) = best_match {
                    for style_index in 0..style_set.count() {
                        if let Some(font) = style_set.new_typeface(style_index)
                        {
                            generics.push((font, alias.clone()));
                        }
                    }
                }
            }
            self.generics_cache = generics;
        }

        &self.generics_cache
    }

    pub fn font_mgr(&mut self) -> FontMgr {
        // collect non-system fonts in a provider
        let mut dyn_mgr = TypefaceFontProvider::new();

        // add a sensible fallback as the first font so the default isn't just
        // whatever is alphabetically first
        if let Some(fallback) = self
            .font_collection()
            .find_typefaces(
                &["system-ui", "sans-serif", "serif"],
                FontStyle::normal(),
            )
            .into_iter()
            .nth(0)
        {
            dyn_mgr.register_typeface(fallback, None);
        }

        // add generic mappings & user-loaded fonts
        for (font, alias) in self.generics() {
            dyn_mgr.register_typeface(font.clone(), alias.as_deref());
        }
        for (font, alias) in &self.fonts {
            dyn_mgr.register_typeface(font.clone(), alias.as_deref());
        }

        // merge system & non-system fonts into single FontMgr
        let mut union_mgr = OrderedFontMgr::new();
        union_mgr.append(dyn_mgr); // generics & user-loaded fonts
        union_mgr.append(self.mgr.clone()); // system fonts
        union_mgr.into()
    }

    pub(crate) fn families(&self) -> Vec<String> {
        let mut names: Vec<String> = self.mgr.family_names().collect();
        for (font, alias) in &self.fonts {
            names.push(match alias {
                Some(name) => name.clone(),
                None => font.family_name(),
            })
        }
        names.sort();
        names.dedup();
        names
    }

    pub(crate) fn family_details(
        &self,
        family: &str,
    ) -> (Vec<f32>, Vec<String>, Vec<String>) {
        // merge the system fonts and our dynamically added fonts into one list
        // of FontStyles
        let mut dynamic = TypefaceFontProvider::new();
        for (font, alias) in &self.fonts {
            dynamic.register_typeface(font.clone(), alias.as_deref());
        }
        let std_mgr = self.mgr.clone();
        let dyn_mgr: FontMgr = dynamic.into();
        let mut std_set = std_mgr.match_family(family);
        let mut dyn_set = dyn_mgr.match_family(family);
        let std_styles = (0..std_set.count()).map(|i| std_set.style(i));
        let dyn_styles = (0..dyn_set.count()).map(|i| dyn_set.style(i));
        let all_styles = std_styles.chain(dyn_styles);

        // set up a collection to query for variable fonts who specify their
        // weights via the 'wght' axis rather than through distinct
        // files with different FontStyles
        let mut var_fc = FontCollection::new();
        var_fc.set_default_font_manager(self.mgr.clone(), None);
        var_fc.set_asset_font_manager(Some(dyn_mgr));

        // pull style values out of each matching font
        let mut weights: Vec<i32> = vec![];
        let mut widths: Vec<String> = vec![];
        let mut styles: Vec<String> = vec![];
        all_styles.for_each(|(style, _name)| {
            widths.push(from_width(style.width()));
            styles.push(from_slant(style.slant()));
            weights.push(*style.weight());
            if let Some(font) = var_fc.find_typefaces(&[&family], style).first()
            {
                // for variable fonts, report all the 100× sizes they support
                // within their wght range
                weights.append(&mut typeface_wght_range(font));
            }
        });

        // repackage collected values
        widths.sort_by(|a, b| {
            a.replace("normal", "_")
                .partial_cmp(&b.replace("normal", "_"))
                // SAFETY: `partial_cmp` on `String` always returns `Some`.
                .unwrap()
        });
        widths.dedup();
        styles.sort_by(|a, b| {
            a.replace("normal", "_")
                .partial_cmp(&b.replace("normal", "_"))
                // SAFETY: `partial_cmp` on `String` always returns `Some`.
                .unwrap()
        });
        styles.dedup();
        weights.sort_unstable();
        weights.dedup();
        let weights = weights.iter().map(|w| *w as f32).collect();
        (weights, widths, styles)
    }

    /// Adds `font` to the shared registry `ctx.font` resolves against.
    ///
    /// The Rust facade's [`FontLibrary`](crate::font::FontLibrary) registers
    /// here as well as with its own provider, so a family registered through
    /// it is visible to `Context2D::set_font` and not only to paragraphs.
    pub fn register_typeface(&mut self, font: Typeface, alias: Option<String>) {
        self.add_typeface(font, alias);
    }

    fn add_typeface(&mut self, font: Typeface, alias: Option<String>) {
        // replace any previously added font with the same metadata/alias
        if let Some(idx) = self.fonts.iter().position(|(old_font, old_alias)|
      match alias.is_some(){
        true => old_alias == &alias,
        false => old_font.family_name() == font.family_name()
      } && old_font.font_style() == font.font_style()
    ){
      self.fonts.remove(idx);
    }

        // add the new typeface/alias and recreate the FontCollection to include
        // it
        self.fonts.push((font, alias));
        self.collection = None;
        self.collection_cache.drain();
    }

    pub fn update_style(
        &mut self,
        orig_style: &TextStyle,
        spec: &FontSpec,
    ) -> Option<TextStyle> {
        let mut style = orig_style.clone();

        // only update the style if a usable family name was specified
        self.font_collection()
            .find_typefaces(&spec.families, spec.style())
            .into_iter()
            .nth(0)
            .map(|typeface| {
                style.set_typeface(typeface);
                style.set_font_families(&spec.families);
                style.set_font_style(spec.style());
                style.set_font_size(spec.size);
                style.reset_font_features();
                for (feat, val) in &spec.features {
                    style.add_font_feature(feat, *val);
                }
                style
            })
    }

    pub fn set_hinting(&mut self, hinting: bool) -> &mut Self {
        // skia's rasterizer cache doesn't take hinting into account, so
        // manually invalidate if changed
        if hinting != self.collection_hinted {
            self.collection_hinted = hinting;
            self.collection_cache
                .iter_mut()
                .for_each(|(_, fc)| fc.clear_caches());
            self.font_collection().clear_caches();
        }
        self
    }

    pub fn fonts_for_style(
        &mut self,
        style: &TextStyle,
        variations: &[(FourByteTag, f32)],
    ) -> FontCollection {
        let families = style.font_families();
        let families: Vec<&str> = families.iter().collect();
        let matches = self
            .font_collection()
            .find_typefaces(&families, style.font_style());

        // if any of the matched typefaces is a variable font, create an
        // instance that matches the current weight settings and add it
        // to a dynamic font mgr
        if matches
            .iter()
            .any(|font| font.variation_design_parameters().is_some())
        {
            // memoize the generation of FontCollections for instanced variable
            // fonts
            let key = CollectionKey::new(style, variations);
            if let Some(collection) = self.collection_cache.get(&key) {
                return collection.clone();
            }

            // build a set of explicitly-set axis tags for quick lookup
            let explicit_tags: Vec<u32> =
                variations.iter().map(|(tag, _)| **tag).collect();

            // collect any instantiated variable fonts in a TFP to be used as
            // the 'dynamic' font mgr (which is searched before the
            // 'asset' or the 'default' mgr)
            let mut dynamic = TypefaceFontProvider::new();

            for font in matches.into_iter() {
                if let Some(params) = font.variation_design_parameters() {
                    // build coordinates from explicit variations + auto wght
                    let mut coords: Vec<Coordinate> = vec![];

                    // add explicit variations first
                    for (tag, value) in variations {
                        // find the matching axis parameter to clamp values
                        if let Some(param) =
                            params.iter().find(|p| *p.tag == **tag)
                        {
                            coords.push(Coordinate {
                                axis: param.tag,
                                value: value.max(param.min).min(param.max),
                            });
                        }
                    }

                    // auto-add wdth if not explicitly set, the way wght is
                    // just below. Without it `fontStretch` reached only
                    // families with a separate condensed *face*: a variable
                    // font carrying a `wdth` axis measured the same at every
                    // setting, because the same typeface came back unpinned.
                    // A browser applies `font-stretch` to the axis, and
                    // fontconfig already resolves the named instance --
                    // `fc-match "Ubuntu:width=condensed"` picks
                    // `Ubuntu[wdth,wght].ttf` -- so we were the ones ignoring
                    // it.
                    let wdth_tag = FourByteTag::from_chars('w', 'd', 't', 'h');
                    if !explicit_tags.contains(&*wdth_tag)
                        && let Some(param) =
                            params.iter().find(|p| *p.tag == *wdth_tag)
                    {
                        let percent =
                            stretch_percent(*style.font_style().width());
                        coords.push(Coordinate {
                            axis: param.tag,
                            value: percent.max(param.min).min(param.max),
                        });
                    }

                    // auto-add wght if not explicitly set
                    let wght_tag = FourByteTag::from_chars('w', 'g', 'h', 't');
                    if !explicit_tags.contains(&*wght_tag)
                        && let Some(param) =
                            params.iter().find(|p| *p.tag == *wght_tag)
                    {
                        let weight = *style.font_style().weight() - 1;
                        let value =
                            (weight as f32).max(param.min).min(param.max);
                        coords.push(Coordinate {
                            axis: param.tag,
                            value,
                        });
                    }

                    if !coords.is_empty() {
                        let v_pos = VariationPosition {
                            coordinates: &coords,
                        };
                        let args = FontArguments::new()
                            .set_variation_design_position(v_pos);
                        if let Some(face) = font.clone_with_arguments(&args) {
                            let alias =
                                self.fonts.iter().find_map(|(orig, alias)| {
                                    if Typeface::equal(&font, orig) {
                                        alias.clone()
                                    } else {
                                        None
                                    }
                                });
                            dynamic.register_typeface(face, alias.as_deref());
                        }
                    }
                }
            }

            let mut collection = self.new_font_collection();
            collection.set_dynamic_font_manager(Some(dynamic.into()));
            self.collection_cache.insert(key, collection.clone());
            collection
        } else {
            // if the matched font wasn't variable, then just return the
            // standard collection
            self.font_collection()
        }
    }

    /// Instantiate a typeface at explicit variable-axis positions for use
    /// with `TextStyle::set_typeface`. Picks the first matched typeface
    /// that exposes variation parameters, intersects the caller's
    /// requested axes with the typeface's design space (clamping out-of-
    /// range values), and clones via `FontArguments`. Returns `None` if
    /// no matched typeface is variable or all requested axes miss the
    /// typeface's design parameters.
    ///
    /// Used by the paragraph builder's `pushStyle` path (paragraph.rs) so
    /// callers passing explicit `fontVariations` get a typeface bound to
    /// the text style directly -- matching CanvasKit's behaviour, where
    /// the paragraph engine respects the requested axis values instead
    /// of relying on the font collection's nominal weight match.
    pub fn instantiate_variable_typeface(
        &mut self,
        style: &TextStyle,
        variations: &[(FourByteTag, f32)],
    ) -> Option<Typeface> {
        if variations.is_empty() {
            return None;
        }
        let families = style.font_families();
        let families: Vec<&str> = families.iter().collect();
        let matches = self
            .font_collection()
            .find_typefaces(&families, style.font_style());
        for font in matches.into_iter() {
            let Some(params) = font.variation_design_parameters() else {
                continue;
            };
            let mut coords: Vec<Coordinate> = Vec::new();
            for (tag, value) in variations {
                if let Some(param) = params.iter().find(|p| *p.tag == **tag) {
                    coords.push(Coordinate {
                        axis: param.tag,
                        value: value.max(param.min).min(param.max),
                    });
                }
            }
            if coords.is_empty() {
                continue;
            }
            let v_pos = VariationPosition {
                coordinates: &coords,
            };
            let args =
                FontArguments::new().set_variation_design_position(v_pos);
            if let Some(face) = font.clone_with_arguments(&args) {
                return Some(face);
            }
        }
        None
    }
}

//
// Javascript Methods
//

pub fn get_families(mut cx: FunctionContext) -> JsResult<JsArray> {
    strings_to_array(&mut cx, &FontLibrary::with_shared(|lib| lib.families()))
}

pub fn has(mut cx: FunctionContext) -> JsResult<JsBoolean> {
    let family = string_arg(&mut cx, 1, "familyName")?;
    let found =
        FontLibrary::with_shared(|lib| lib.families().contains(&family));
    Ok(cx.boolean(found))
}

pub fn family(mut cx: FunctionContext) -> JsResult<JsValue> {
    let family = string_arg(&mut cx, 1, "familyName")?;
    let (weights, widths, styles) =
        FontLibrary::with_shared(|lib| lib.family_details(&family));

    if weights.is_empty() {
        return Ok(cx.undefined().upcast());
    }

    let name = cx.string(family);
    let weights = floats_to_array(&mut cx, &weights)?;
    let widths = strings_to_array(&mut cx, &widths)?;
    let styles = strings_to_array(&mut cx, &styles)?;

    let details = JsObject::new(&mut cx);
    let attr = cx.string("family");
    details.set(&mut cx, attr, name)?;
    let attr = cx.string("weights");
    details.set(&mut cx, attr, weights)?;
    let attr = cx.string("widths");
    details.set(&mut cx, attr, widths)?;
    let attr = cx.string("styles");
    details.set(&mut cx, attr, styles)?;

    Ok(details.upcast())
}

pub fn addFamily(mut cx: FunctionContext) -> JsResult<JsValue> {
    let alias = opt_string_arg(&mut cx, 1);
    let filenames = cx.argument::<JsArray>(2)?.to_vec(&mut cx)?;
    let results = JsArray::new(&mut cx, filenames.len());

    for (i, filename) in strings_in(&mut cx, &filenames).iter().enumerate() {
        let path = Path::new(&filename);
        let typeface = match fs::read(path) {
            Err(why) => {
                return cx.throw_error(format!(
                    "{}: \"{}\"",
                    why,
                    path.display()
                ));
            }
            Ok(bytes) => {
                #[cfg(target_os = "windows")]
                let bytes = {
                    fn decode_woff(bytes: &Vec<u8>) -> Option<Vec<u8>> {
                        let woff =
                            ReadScope::new(&bytes).read::<WoffFont>().ok()?;
                        let tags = woff.table_tags()?;
                        whole_font(&woff, &tags).ok()
                    }

                    fn decode_woff2(bytes: &Vec<u8>) -> Option<Vec<u8>> {
                        let woff2 =
                            ReadScope::new(&bytes).read::<Woff2Font>().ok()?;
                        let tables = woff2.table_provider(0).ok()?;
                        let tags = tables.table_tags()?;
                        whole_font(&tables, &tags).ok()
                    }

                    match filename.to_ascii_lowercase() {
                        name if name.ends_with(".woff") => decode_woff(&bytes),
                        name if name.ends_with(".woff2") => {
                            decode_woff2(&bytes)
                        }
                        _ => None,
                    }
                }
                .unwrap_or(bytes);

                FontLibrary::with_shared(|lib| {
                    lib.mgr.new_from_data(&bytes, None)
                })
            }
        };

        match typeface {
            Some(font) => {
                // add family/weight/width/slant details to return value
                let details =
                    typeface_details(&mut cx, filename, &font, alias.clone())?;
                results.set(&mut cx, i as u32, details)?;

                // register the typeface
                FontLibrary::with_shared(|lib| {
                    lib.add_typeface(font, alias.clone())
                });
            }
            None => {
                return cx.throw_error(format!(
                    "Could not decode font data in {}",
                    path.display()
                ));
            }
        }
    }

    Ok(results.upcast())
}

pub fn addFamilyFromData(mut cx: FunctionContext) -> JsResult<JsValue> {
    let alias = opt_string_arg(&mut cx, 1);
    let buffers = cx.argument::<JsArray>(2)?.to_vec(&mut cx)?;
    let results = JsArray::new(&mut cx, buffers.len());

    for (i, buf_val) in buffers.iter().enumerate() {
        let buf = buf_val.downcast_or_throw::<JsBuffer, _>(&mut cx)?;
        let bytes = buf.as_slice(&cx).to_vec();
        let typeface =
            FontLibrary::with_shared(|lib| lib.mgr.new_from_data(&bytes, None));

        match typeface {
            Some(font) => {
                let details = typeface_details(
                    &mut cx,
                    "<buffer>",
                    &font,
                    alias.clone(),
                )?;
                results.set(&mut cx, i as u32, details)?;
                FontLibrary::with_shared(|lib| {
                    lib.add_typeface(font, alias.clone())
                });
            }
            None => {
                return cx
                    .throw_error("Could not decode font data from buffer");
            }
        }
    }

    Ok(results.upcast())
}

pub fn reset(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    FontLibrary::with_shared(|lib| {
        lib.fonts.clear();
        lib.collection = None;
        lib.collection_cache.drain();
    });

    Ok(cx.undefined())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_width_class_maps_to_the_percentage_css_names() {
        // The nine OS/2 classes and the CSS Fonts 4 keywords they carry.
        // Restated here rather than read out of the table, so a reordered
        // row fails instead of agreeing with itself.
        for (class, percent, keyword) in [
            (1, 50.0, "ultra-condensed"),
            (2, 62.5, "extra-condensed"),
            (3, 75.0, "condensed"),
            (4, 87.5, "semi-condensed"),
            (5, 100.0, "normal"),
            (6, 112.5, "semi-expanded"),
            (7, 125.0, "expanded"),
            (8, 150.0, "extra-expanded"),
            (9, 200.0, "ultra-expanded"),
        ] {
            assert_eq!(
                stretch_percent(class),
                percent,
                "class {class} is {keyword}"
            );
        }
    }

    #[test]
    fn a_class_the_standard_does_not_define_falls_back_to_normal() {
        // The same answer class 5 gives, which is why the two used to share
        // an arm. They no longer do: a font declaring 0 or 12 is malformed
        // and a font declaring 5 is not, and the code should be able to say
        // which case it is in even when the number it returns is the same.
        for class in [i32::MIN, -1, 0, 10, 12, 255, i32::MAX] {
            assert_eq!(
                stretch_percent(class),
                stretch_percent(5),
                "class {class}"
            );
        }
    }

    #[test]
    fn the_table_runs_from_condensed_to_expanded_without_repeating() {
        // A transposed pair would still pass the lookup test above if it
        // were read out of the table; this asks the question the table
        // exists to answer.
        assert!(
            WIDTH_CLASS_PERCENT.windows(2).all(|pair| pair[0] < pair[1]),
            "{WIDTH_CLASS_PERCENT:?} is not strictly increasing"
        );
        assert_eq!(WIDTH_CLASS_PERCENT[4], 100.0, "the middle class is normal");
    }
}
