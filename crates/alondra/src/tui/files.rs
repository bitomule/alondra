//! What a stored attachment holds once it is loaded: text to read, an image to draw, or bytes
//! the TUI cannot show and leaves to the Mac's default app.

use std::sync::Arc;

#[derive(Clone)]
pub enum FileBody {
    Text(String),
    Image(Arc<image::DynamicImage>),
    Binary { bytes: usize },
}

impl FileBody {
    pub fn classify(bytes: Vec<u8>) -> Self {
        if let Ok(format) = image::guess_format(&bytes)
            && let Ok(img) = image::load_from_memory_with_format(&bytes, format)
        {
            return FileBody::Image(Arc::new(img));
        }
        let size = bytes.len();
        match String::from_utf8(bytes) {
            Ok(text) if !text.contains('\0') => FileBody::Text(text),
            _ => FileBody::Binary { bytes: size },
        }
    }
}

/// What stands in for a file that is not text: its kind, size, and the key that opens it.
pub fn not_text(body: &FileBody) -> String {
    match body {
        FileBody::Image(img) => {
            let (w, h) = (img.width(), img.height());
            trf!("Image {w}×{h} · v opens it in Preview", "Imagen {w}×{h} · v la abre en Vista Previa")
        }
        FileBody::Binary { bytes } => {
            let s = size(*bytes);
            trf!("Binary file, {s} · v opens it with its app", "Fichero binario, {s} · v lo abre con su aplicación")
        }
        FileBody::Text(_) => String::new(),
    }
}

/// "12 KB", "3.4 MB".
pub fn size(bytes: usize) -> String {
    match bytes {
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{} KB", b / 1024),
        b => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
    }
}

#[cfg(test)]
pub(crate) fn png(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbImage::from_fn(w, h, |x, y| image::Rgb([(x * 40) as u8, (y * 40) as u8, 200]));
    let mut out = std::io::Cursor::new(vec![]);
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_attachment_is_read_as_text_drawn_as_an_image_or_left_as_bytes() {
        let cases: [(&str, Vec<u8>, &str); 5] = [
            ("markdown", "# Plan\n\nUna línea con tilde.".as_bytes().to_vec(), "text"),
            ("a PNG", png(4, 3), "image"),
            ("a PDF", b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n1 0 obj".to_vec(), "binary"),
            ("text with NUL", b"abc\0def".to_vec(), "binary"),
            ("a PNG header over garbage", b"\x89PNG\r\n\x1a\n\0\0\0\rIHDRbroken".to_vec(), "binary"),
        ];
        for (what, bytes, want) in cases {
            let got = match FileBody::classify(bytes) {
                FileBody::Text(_) => "text",
                FileBody::Image(_) => "image",
                FileBody::Binary { .. } => "binary",
            };
            assert_eq!(got, want, "{what}");
        }
    }
}
