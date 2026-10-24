//! `Canvas::to_file` writing one file per page, and `Canvas::to_file_as`
//! naming the format. The expected file names are what the Node binding's
//! `toFile` writes for the same canvas, names and options.

use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use meo_skia_canvas::prelude::*;

/// Twelve pages, the `n`th one `3 + n` pixels wide, so a file can be traced
/// back to the page it came from by its width.
fn twelve_pages() -> Canvas {
    let mut canvas = Canvas::new(4.0, 4.0);
    for n in 1..12 {
        canvas.new_page_with(4.0 + n as f32, 4.0);
    }
    for index in 0..12 {
        let page = canvas.page(index).expect("a page");
        page.fill_rect(0.0, 0.0, 1.0, 1.0);
    }
    canvas
}

/// A fresh directory for one test.
fn scratch(name: &str) -> PathBuf {
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "meo-to-file-{name}-{}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("a clock after 1970")
            .as_nanos(),
        COUNT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&dir).expect("a temp dir");
    dir
}

/// The file names in `dir`, sorted.
fn names(dir: &PathBuf) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .expect("a readable dir")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

/// The width of the image written to `path`.
fn width(path: PathBuf) -> u32 {
    Image::from_encoded(&fs::read(path).expect("the file"))
        .expect("an image")
        .width()
}

/// `options` with a page range of `start..end`.
fn range(start: usize, end: usize) -> EncodeOptions {
    EncodeOptions {
        page_range: Some(start..end),
        ..EncodeOptions::default()
    }
}

#[test]
fn braces_write_one_file_per_page_numbered_to_the_page_count() {
    let dir = scratch("all");
    twelve_pages()
        .to_file(dir.join("f-{}.png"), &EncodeOptions::default())
        .expect("written");
    let want: Vec<String> = (1..=12).map(|n| format!("f-{n:02}.png")).collect();
    assert_eq!(names(&dir), want);
    assert_eq!(width(dir.join("f-01.png")), 4);
    assert_eq!(width(dir.join("f-12.png")), 15);
}

#[test]
fn a_number_in_the_braces_is_the_padding() {
    let dir = scratch("padded");
    twelve_pages()
        .to_file(dir.join("f-{3}.png"), &EncodeOptions::default())
        .expect("written");
    assert_eq!(names(&dir).first().map(String::as_str), Some("f-001.png"));
    assert_eq!(names(&dir).last().map(String::as_str), Some("f-012.png"));

    let dir = scratch("unpadded");
    twelve_pages()
        .to_file(dir.join("f-{0}.png"), &EncodeOptions::default())
        .expect("written");
    assert!(names(&dir).contains(&"f-1.png".to_string()));
    assert!(names(&dir).contains(&"f-12.png".to_string()));
}

#[test]
fn a_range_numbers_its_own_pages_from_one() {
    // `pageRange: [3, 5]` in JavaScript, which counts from one and includes
    // its end: the third to fifth pages, written as 1 to 3.
    let dir = scratch("range");
    twelve_pages()
        .to_file(dir.join("f-{}.png"), &range(2, 5))
        .expect("written");
    assert_eq!(names(&dir), ["f-1.png", "f-2.png", "f-3.png"]);
    assert_eq!(width(dir.join("f-1.png")), 6, "the third page");
}

#[test]
fn a_named_page_is_written_alone_as_number_one() {
    let dir = scratch("page");
    let options = EncodeOptions {
        page: Some(1),
        ..EncodeOptions::default()
    };
    twelve_pages()
        .to_file(dir.join("f-{}.png"), &options)
        .expect("written");
    assert_eq!(names(&dir), ["f-1.png"]);
    assert_eq!(width(dir.join("f-1.png")), 5, "the second page");
}

#[test]
fn every_pair_of_braces_takes_the_number() {
    let dir = scratch("twice");
    twelve_pages()
        .to_file(dir.join("a{2}-b{}.png"), &range(0, 2))
        .expect("written");
    assert_eq!(names(&dir), ["a01-b01.png", "a02-b02.png"]);
}

#[test]
fn a_format_that_gathers_pages_writes_one_file_each_under_a_pattern() {
    let dir = scratch("pdf");
    twelve_pages()
        .to_file(dir.join("f-{}.pdf"), &range(0, 2))
        .expect("written");
    assert_eq!(names(&dir), ["f-1.pdf", "f-2.pdf"]);
}

#[test]
fn to_file_as_names_the_format_the_path_does_not() {
    let dir = scratch("as");
    twelve_pages()
        .to_file_as(dir.join("f-{}"), ImageFormat::Jpeg, &range(0, 2))
        .expect("written");
    assert_eq!(names(&dir), ["f-1", "f-2"]);
    let bytes = fs::read(dir.join("f-2")).expect("the file");
    assert_eq!(&bytes[..3], [0xFF, 0xD8, 0xFF], "a JPEG, whatever the name");
    assert_eq!(width(dir.join("f-2")), 5);

    let dir = scratch("as-single");
    let mut canvas = Canvas::new(3.0, 2.0);
    canvas
        .to_file_as(
            dir.join("picture.dat"),
            ImageFormat::Png,
            &EncodeOptions::default(),
        )
        .expect("written");
    let bytes = fs::read(dir.join("picture.dat")).expect("the file");
    assert_eq!(&bytes[1..4], b"PNG");
}

#[test]
fn a_range_without_a_pattern_is_still_refused_for_one_page_formats() {
    let dir = scratch("refused");
    let result = twelve_pages().to_file(dir.join("single.png"), &range(0, 2));
    assert!(
        matches!(result, Err(Error::InvalidExportOption { .. })),
        "{result:?}"
    );
    assert!(names(&dir).is_empty());
}

#[test]
fn a_name_without_braces_is_written_as_before() {
    let dir = scratch("plain");
    let mut canvas = twelve_pages();
    canvas
        .to_file(dir.join("last.png"), &EncodeOptions::default())
        .expect("written");
    assert_eq!(names(&dir), ["last.png"]);
    assert_eq!(width(dir.join("last.png")), 15, "the current page");
}
