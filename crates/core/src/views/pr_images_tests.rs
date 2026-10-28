use super::*;
use crate::markdown::images::images;

const ROUTE: &str = "/files?path=files/images/0000000000000000000000000000000000000000000000000000000000000000.png";

fn found(markdown: &str, shown: impl Fn(&str) -> Shown) -> Vec<PrImage> {
    images(markdown)
        .into_iter()
        .map(|image| PrImage { shown: shown(&image.url), span: image.span, dest: image.dest, url: image.url, alt: image.alt, html: image.html })
        .collect()
}

#[test]
fn a_brought_image_points_at_its_copy_and_keeps_the_rest_as_written() {
    let body = "Before:\n\n![storage](https://github.com/o/r/blob/8d0e5fb/docs/a.jpg?raw=true \"Areas\")\n\n<img width=\"300\" src=\"docs/b.png\">\n";
    let images = found(body, |_| Shown::Stored { route: ROUTE.into() });
    assert_eq!(
        show(body, &images, Lang::Es),
        format!("Before:\n\n![storage]({ROUTE} \"Areas\")\n\n<img alt=\"b.png\" width=\"300\" src=\"{ROUTE}\">\n")
    );
}

#[test]
fn an_image_that_could_not_be_brought_says_why_in_the_users_language_and_links_to_github() {
    let body = "![](https://github.com/o/r/blob/8d0e5fb7edcc190633b9fc0b868f7051e48bc0f9/docs/gone.jpg?raw=true) and ![Image](https://github.com/user-attachments/assets/3bae41ae-ce20-495d-bb1b-ad4229a6e245)\n";
    let images = found(body, |url| Shown::Unavailable {
        link: url.into(),
        problem: if url.contains("blob") { ImageProblem::NotInBranch { reference: "8d0e5fb7edcc190633b9fc0b868f7051e48bc0f9".into() } } else { ImageProblem::Gone },
    });
    assert_eq!(
        show(body, &images, Lang::Es),
        "![gone.jpg — no está en la rama (8d0e5fb)](https://github.com/o/r/blob/8d0e5fb7edcc190633b9fc0b868f7051e48bc0f9/docs/gone.jpg?raw=true) and \
         ![Imagen — GitHub ya no la tiene](https://github.com/user-attachments/assets/3bae41ae-ce20-495d-bb1b-ad4229a6e245)\n"
    );
    assert!(show(body, &images, Lang::En).starts_with("![gone.jpg — not in the branch (8d0e5fb)]("));
}

#[test]
fn an_html_image_that_could_not_be_brought_stays_html_with_the_reason_as_its_alt() {
    let body = "<p><img alt=\"Paywall <dark>\" src=\"docs/p.png\"></p>\n";
    let images = found(body, |_| Shown::Unavailable {
        link: "https://github.com/o/r/blob/abc/docs/p.png".into(),
        problem: ImageProblem::TooLarge { bytes: 23_000_000 },
    });
    assert_eq!(
        show(body, &images, Lang::Es),
        "<p><img alt=\"Paywall &lt;dark> — pesa 23 MB, más del máximo de 10 MB\" src=\"https://github.com/o/r/blob/abc/docs/p.png\"></p>\n"
    );
}

#[test]
fn a_brought_image_without_alt_text_is_named_by_its_file() {
    let body = "![](https://github.com/o/r/blob/8d0e5fb/docs/pr/duo/storage-areas-es.jpg?raw=true)\n";
    let images = found(body, |_| Shown::Stored { route: ROUTE.into() });
    assert_eq!(show(body, &images, Lang::Es), format!("![storage-areas-es.jpg]({ROUTE})\n"));
}

#[test]
fn a_reference_image_used_twice_points_its_definition_at_the_copy_once() {
    let body = "![one][s] ![two][s]\n\n[s]: docs/a.png\n";
    let images = found(body, |_| Shown::Stored { route: ROUTE.into() });
    assert_eq!(show(body, &images, Lang::En), format!("![one][s] ![two][s]\n\n[s]: {ROUTE}\n"));
}

#[test]
fn images_found_in_another_text_change_nothing() {
    let images = found("![a](docs/a.png)\n", |_| Shown::Stored { route: ROUTE.into() });
    let edited = "Now the text moved: ![a](docs/a.png)\n";
    assert_eq!(show(edited, &images, Lang::En), edited);
    assert_eq!(show("", &images, Lang::En), "");
}

#[test]
fn every_reason_reads_in_both_languages() {
    let reasons = [
        ImageProblem::NotInBranch { reference: "8d0e5fb7edcc190633b9fc0b868f7051e48bc0f9".into() },
        ImageProblem::Gone,
        ImageProblem::NoAccess { repository: "o/r".into() },
        ImageProblem::SignedOut,
        ImageProblem::TooLarge { bytes: 23_000_000 },
        ImageProblem::NotAnImage,
        ImageProblem::TooMany,
        ImageProblem::Retrying,
    ];
    for reason in reasons {
        assert_ne!(reason.text(Lang::En), reason.text(Lang::Es), "{reason:?}");
        assert_eq!(reason.missing(), matches!(reason, ImageProblem::NotInBranch { .. } | ImageProblem::Gone));
    }
}
