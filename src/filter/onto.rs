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

use std::{cell::RefCell, rc::Rc};

use skia_safe::{Color4f, ColorSpace};

use crate::node::utils::skia_paint_color_onto;

/// The page a filter is built for.
#[derive(Clone, PartialEq)]
pub(crate) struct Onto {
    /// The space the page's pixels are in.
    pub destination: ColorSpace,
}

impl Onto {
    /// `color` in `space` as Skia is handed it in a filter drawn onto this
    /// page -- see [`skia_paint_color_onto`].
    pub fn color(
        &self,
        color: Color4f,
        space: &ColorSpace,
    ) -> (Color4f, ColorSpace) {
        skia_paint_color_onto(color, space, &self.destination)
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
/// last, which a filter drawn many times onto one page is asked for again.
struct Rebuild<T> {
    build: Build<T>,
    held: RefCell<Option<(Onto, T)>>,
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
                held: RefCell::new(None),
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
        if let Some((page, filter)) = &*rebuild.held.borrow()
            && page == onto
        {
            return filter.clone();
        }
        let filter =
            (rebuild.build)(Some(onto)).unwrap_or_else(|| self.plain.clone());
        *rebuild.held.borrow_mut() = Some((onto.clone(), filter.clone()));
        filter
    }
}
