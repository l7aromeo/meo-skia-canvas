//! An `@2x` at the end of a file name's stem is a density, as JavaScript's
//! `toFile` reads one. The expected sizes are what the Node binding writes
//! for the same canvas -- a 10x4 page, then a 6x4 one -- and names.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use meo_skia_canvas::prelude::*;

fn two_pages() -> Canvas {
    let mut canvas = Canvas::new(10.0, 4.0);
    canvas.context().fill_rect(0.0, 0.0, 1.0, 1.0);
    canvas.new_page_with(6.0, 4.0);
    canvas
}

/// A fresh directory for one test.
fn scratch(name: &str) -> PathBuf {
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "meo-density-{name}-{}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("a clock after 1970")
            .as_nanos(),
        COUNT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&dir).expect("a temp dir");
    dir
}

/// Every file in `dir` with the size of the image it holds, sorted by name.
fn written(dir: &Path) -> Vec<(String, u32, u32)> {
    let mut files: Vec<(String, u32, u32)> = fs::read_dir(dir)
        .expect("a readable dir")
        .map(|entry| {
            let path = entry.expect("an entry").path();
            let image =
                Image::from_encoded(&fs::read(&path).expect("the file"))
                    .expect("an image");
            (
                path.file_name()
                    .expect("a name")
                    .to_string_lossy()
                    .into_owned(),
                image.width(),
                image.height(),
            )
        })
        .collect();
    files.sort();
    files
}

fn save(
    name: &str,
    options: &EncodeOptions,
) -> Result<Vec<(String, u32, u32)>, Error> {
    let dir = scratch("case");
    two_pages().to_file(dir.join(name), options)?;
    Ok(written(&dir))
}

fn one(name: &str, width: u32, height: u32) -> Vec<(String, u32, u32)> {
    vec![(name.to_string(), width, height)]
}

#[test]
fn a_whole_number_before_the_extension_is_the_density() {
    let default = EncodeOptions::default();
    assert_eq!(
        save("a@2x.png", &default).ok(),
        Some(one("a@2x.png", 12, 8))
    );
    assert_eq!(
        save("a@3X.png", &default).ok(),
        Some(one("a@3X.png", 18, 12))
    );
    assert_eq!(
        save("a@02x.png", &default).ok(),
        Some(one("a@02x.png", 12, 8))
    );
    assert_eq!(
        save("a@20x.png", &default).ok(),
        Some(one("a@20x.png", 120, 80))
    );
}

#[test]
fn anything_else_is_part_of_the_name() {
    let default = EncodeOptions::default();
    // A fraction is not a suffix, and neither is one with more after it.
    assert_eq!(
        save("a@1.5x.png", &default).ok(),
        Some(one("a@1.5x.png", 6, 4))
    );
    assert_eq!(
        save("a@2xb.png", &default).ok(),
        Some(one("a@2xb.png", 6, 4))
    );
    assert_eq!(
        save("a@2x-b.png", &default).ok(),
        Some(one("a@2x-b.png", 6, 4))
    );
}

#[test]
fn a_density_the_options_give_wins() {
    let at = |density| EncodeOptions {
        density,
        ..EncodeOptions::default()
    };
    assert_eq!(
        save("a@2x.png", &at(3.0)).ok(),
        Some(one("a@2x.png", 18, 12))
    );
    assert_eq!(save("a@2x.png", &at(1.5)).ok(), Some(one("a@2x.png", 9, 6)));
}

#[test]
fn every_page_of_a_sequence_takes_it() {
    assert_eq!(
        save("f-{}@2x.png", &EncodeOptions::default()).ok(),
        Some(vec![
            ("f-1@2x.png".to_string(), 20, 8),
            ("f-2@2x.png".to_string(), 12, 8),
        ])
    );
    // Before the braces it is not at the end of the stem.
    assert_eq!(
        save("f@2x-{}.png", &EncodeOptions::default()).ok(),
        Some(vec![
            ("f@2x-1.png".to_string(), 10, 4),
            ("f@2x-2.png".to_string(), 6, 4),
        ])
    );
}

#[test]
fn a_name_with_no_extension_ends_in_it() {
    let dir = scratch("as");
    two_pages()
        .to_file_as(
            dir.join("f@2x"),
            ImageFormat::Png,
            &EncodeOptions::default(),
        )
        .expect("written");
    assert_eq!(written(&dir), one("f@2x", 12, 8));
}

#[test]
fn a_zero_density_suffix_is_refused_by_name() {
    let dir = scratch("zero");
    let result =
        two_pages().to_file(dir.join("a@0x.png"), &EncodeOptions::default());
    let Err(Error::InvalidExportOption { option, reason }) = result else {
        panic!("refused: {result:?}");
    };
    assert_eq!(option, "density");
    assert!(reason.contains("@0x"), "{reason}");
    assert!(written(&dir).is_empty(), "nothing written");
}

#[test]
fn the_stem_is_the_name_less_its_extension_whatever_the_format() {
    let as_format = |name: &str, format| {
        let dir = scratch("format");
        two_pages()
            .to_file_as(dir.join(name), format, &EncodeOptions::default())
            .map(|()| written(&dir))
    };
    assert_eq!(
        as_format("f@2x.png", ImageFormat::Png).ok(),
        Some(one("f@2x.png", 12, 8))
    );
    assert_eq!(
        as_format("f@2x.dat", ImageFormat::Png).ok(),
        Some(one("f@2x.dat", 12, 8))
    );
    assert_eq!(
        as_format("f@2x.png", ImageFormat::Jpeg).ok(),
        Some(one("f@2x.png", 12, 8))
    );
    assert_eq!(
        as_format("f@2xjpg", ImageFormat::Jpeg).ok(),
        Some(one("f@2xjpg", 6, 4))
    );
}

#[test]
fn an_explicit_density_of_one_cannot_be_told_from_the_default() {
    // The one place this differs from JavaScript, where `{ density: 1 }`
    // wins over the suffix: `EncodeOptions::density` has no unset state.
    let options = EncodeOptions {
        density: 1.0,
        ..EncodeOptions::default()
    };
    assert_eq!(
        save("a@2x.png", &options).ok(),
        Some(one("a@2x.png", 12, 8))
    );
}
