use super::*;
use crate::test_support::Directory;

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nshot";

fn written<'a>(markdown: &'a str, range: &Range<usize>) -> &'a str {
    &markdown[range.clone()]
}

#[test]
fn an_inline_image_says_where_it_and_its_destination_are_written_and_its_alt() {
    let markdown = "Look: ![main **screen**](docs/shot.png \"Main\") and on.\n";
    let found = images(markdown);
    assert_eq!(found.len(), 1);
    let image = &found[0];
    assert_eq!(written(markdown, &image.span), "![main **screen**](docs/shot.png \"Main\")");
    assert_eq!(written(markdown, &image.dest), "docs/shot.png");
    assert_eq!((image.url.as_str(), image.alt.as_str(), image.html), ("docs/shot.png", "main screen", false));
}

#[test]
fn a_reference_image_points_its_destination_at_the_definition() {
    let markdown = "![one][shot] and ![two][shot]\n\n[shot]: https://github.com/o/r/blob/abc/a.png?raw=true\n";
    let found = images(markdown);
    assert_eq!(found.len(), 2);
    assert_eq!(written(markdown, &found[0].span), "![one][shot]");
    assert_eq!(written(markdown, &found[1].span), "![two][shot]");
    for image in &found {
        assert_eq!(written(markdown, &image.dest), "https://github.com/o/r/blob/abc/a.png?raw=true");
    }
}

#[test]
fn html_images_give_their_tag_their_src_and_their_alt() {
    let markdown = "<p align=\"center\">\n<img width=\"300\" alt=\"Paywall\" src=\"https://github.com/user-attachments/assets/3bae41ae-ce20-495d-bb1b-ad4229a6e245\">\n</p>\n\nInline <img src=\"docs/a.png\"> too.\n";
    let found = images(markdown);
    assert_eq!(found.len(), 2);
    assert!(found.iter().all(|image| image.html));
    assert_eq!(
        written(markdown, &found[0].span),
        "<img width=\"300\" alt=\"Paywall\" src=\"https://github.com/user-attachments/assets/3bae41ae-ce20-495d-bb1b-ad4229a6e245\">"
    );
    assert_eq!(written(markdown, &found[0].dest), found[0].url);
    assert_eq!(found[0].alt, "Paywall");
    assert_eq!((written(markdown, &found[1].dest), found[1].alt.as_str()), ("docs/a.png", ""));
}

#[test]
fn an_image_inside_a_link_is_found_and_links_and_code_are_not() {
    let markdown = "[![shot](a.png)](https://example.com) [a link](b.png) `![no](c.png)`\n\n```\n![no](d.png)\n```\n";
    let found = images(markdown);
    assert_eq!(found.iter().map(|image| image.url.as_str()).collect::<Vec<_>>(), ["a.png"]);
    assert_eq!(written(markdown, &found[0].span), "![shot](a.png)");
}

#[test]
fn a_kept_image_is_stored_by_its_sha256_once_and_its_destination_names_the_copy() {
    let dir = Directory::new();
    let home = dir.join("home");
    let route = keep(&home, PNG, "png").unwrap();
    assert_eq!(keep(&home, PNG, "png").unwrap(), route);
    let reference = stored_image(&route).unwrap();
    assert_eq!(std::fs::read(home.join(reference)).unwrap(), PNG);
    assert_eq!(std::fs::read_dir(home.join(FOLDER)).unwrap().count(), 1);
}

#[test]
fn the_kind_of_an_image_comes_from_its_first_bytes() {
    assert_eq!(image_extension(PNG), Some("png"));
    assert_eq!(image_extension(&[0xff, 0xd8, 0xff, 0xe0, 0, 0]), Some("jpg"));
    assert_eq!(image_extension(b"GIF89a...."), Some("gif"));
    assert_eq!(image_extension(b"RIFF\0\0\0\0WEBPVP8 "), Some("webp"));
    assert_eq!(image_extension(b"\0\0\0\x18ftypheic\0\0"), Some("heic"));
    assert_eq!(image_extension(b"<?xml version=\"1.0\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\"/>"), Some("svg"));
    assert_eq!(image_extension(b"<!DOCTYPE html><html>Sign in to GitHub</html>"), None);
    assert_eq!(image_extension(b""), None);
}
