//! What a stored attachment holds once it is loaded: text to read, an image to draw, or bytes
//! the TUI cannot show and leaves to the Mac's default app.

use std::sync::Arc;

use alondra_core::views::{Attachment, AttachmentKind};

#[derive(Clone)]
pub enum FileBody {
    Text(String),
    Image(Arc<image::DynamicImage>),
    Binary { bytes: usize },
    /// A PR's diff, read by file.
    Diff(Arc<super::unified_diff::Diff>),
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

/// A terminal graphics protocol that draws the image itself. Half-blocks only approximate it in
/// coloured cells, so a terminal that has nothing better shows the attachment's line instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Graphics {
    Kitty,
    Iterm2,
    Sixel,
}

pub fn graphics(protocol: ratatui_image::picker::ProtocolType) -> Option<Graphics> {
    use ratatui_image::picker::ProtocolType;
    match protocol {
        ProtocolType::Kitty => Some(Graphics::Kitty),
        ProtocolType::Iterm2 => Some(Graphics::Iterm2),
        ProtocolType::Sixel => Some(Graphics::Sixel),
        ProtocolType::Halfblocks => None,
    }
}

/// Whether the reader reads the attachment as it opens: an image to draw or a text to read.
/// A video or another file is only downloaded when the user opens it.
pub fn read_up_front(kind: Option<AttachmentKind>) -> bool {
    !matches!(kind, Some(AttachmentKind::Video | AttachmentKind::Binary))
}

pub fn glyph(kind: Option<AttachmentKind>) -> &'static str {
    match kind {
        Some(AttachmentKind::Image) => "📷",
        Some(AttachmentKind::Video) => "🎬",
        Some(AttachmentKind::Text) => "📄",
        Some(AttachmentKind::Binary) | None => "📎",
    }
}

pub fn kind_name(kind: AttachmentKind) -> &'static str {
    match kind {
        AttachmentKind::Image => tr!("Image", "Imagen"),
        AttachmentKind::Video => tr!("Video", "Vídeo"),
        AttachmentKind::Text => tr!("Text", "Texto"),
        AttachmentKind::Binary => tr!("File", "Fichero"),
    }
}

/// The attachment as one line of the card: glyph, name, kind and size.
pub fn attachment_line(attachment: &Attachment) -> String {
    let (glyph, name, kind) = (glyph(Some(attachment.kind)), &attachment.name, kind_name(attachment.kind));
    let size = size(attachment.size as usize);
    format!("{glyph} {name} · {kind} · {size}")
}

/// What stands in for an attachment the reader leaves to the Mac until the user opens it.
pub fn not_read(kind: Option<AttachmentKind>, bytes: Option<u64>) -> String {
    let kind = kind_name(kind.unwrap_or(AttachmentKind::Binary));
    let size = bytes.map(|b| format!(" · {}", size(b as usize))).unwrap_or_default();
    trf!(
        "{kind}{size} · v downloads it and opens it with its app",
        "{kind}{size} · v lo baja y lo abre con su aplicación"
    )
}

/// What stands in for a file that is not text: its kind, size, and the key that opens it.
pub fn not_text(body: &FileBody, bytes: Option<u64>) -> String {
    match body {
        FileBody::Image(img) => {
            let (w, h) = (img.width(), img.height());
            let size = bytes.map(|b| format!(" · {}", size(b as usize))).unwrap_or_default();
            trf!(
                "Image {w}×{h}{size} · v opens it in Preview",
                "Imagen {w}×{h}{size} · v la abre en Vista Previa"
            )
        }
        FileBody::Binary { bytes } => {
            let s = size(*bytes);
            trf!(
                "Binary file, {s} · v opens it with its app",
                "Fichero binario, {s} · v lo abre con su aplicación"
            )
        }
        FileBody::Text(_) | FileBody::Diff(_) => String::new(),
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
    let img = image::RgbImage::from_fn(w, h, |x, y| {
        image::Rgb([(x * 40) as u8, (y * 40) as u8, 200])
    });
    let mut out = std::io::Cursor::new(vec![]);
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kitty_iterm2_and_sixel_draw_images_and_half_blocks_fall_back_to_the_line() {
        use ratatui_image::picker::ProtocolType;
        assert_eq!(graphics(ProtocolType::Kitty), Some(Graphics::Kitty));
        assert_eq!(graphics(ProtocolType::Iterm2), Some(Graphics::Iterm2));
        assert_eq!(graphics(ProtocolType::Sixel), Some(Graphics::Sixel));
        assert_eq!(graphics(ProtocolType::Halfblocks), None);
        assert_eq!(graphics(ratatui_image::picker::Picker::halfblocks().protocol_type()), None, "a terminal that does not answer");
    }

    #[test]
    fn an_attachment_is_read_as_text_drawn_as_an_image_or_left_as_bytes() {
        let cases: [(&str, Vec<u8>, &str); 5] = [
            (
                "markdown",
                "# Plan\n\nUna línea con tilde.".as_bytes().to_vec(),
                "text",
            ),
            ("a PNG", png(4, 3), "image"),
            (
                "a PDF",
                b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n1 0 obj".to_vec(),
                "binary",
            ),
            ("text with NUL", b"abc\0def".to_vec(), "binary"),
            (
                "a PNG header over garbage",
                b"\x89PNG\r\n\x1a\n\0\0\0\rIHDRbroken".to_vec(),
                "binary",
            ),
        ];
        for (what, bytes, want) in cases {
            let got = match FileBody::classify(bytes) {
                FileBody::Text(_) => "text",
                FileBody::Image(_) => "image",
                FileBody::Binary { .. } => "binary",
                FileBody::Diff(_) => "diff",
            };
            assert_eq!(got, want, "{what}");
        }
    }
}
