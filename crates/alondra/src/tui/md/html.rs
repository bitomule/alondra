//! The HTML Markdown lets through, read just enough for a document: the tags, their attributes and
//! the text between them. Nothing is interpreted here; the Markdown builder decides what a tag means.

#[derive(Debug, PartialEq)]
pub enum Piece<'a> {
    Text(String),
    Tag(Tag<'a>),
    Comment,
}

#[derive(Debug, PartialEq)]
pub struct Tag<'a> {
    pub name: String,
    pub closing: bool,
    pub attrs: Vec<(String, String)>,
    /// The tag as written, for a tag the builder leaves as text.
    pub raw: &'a str,
}

impl Tag<'_> {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// The pieces of `src`, and what is left of it when it ends inside a tag or a comment: Markdown
/// hands an HTML block over line by line, so a tag may go on in the next one.
pub fn pieces(src: &str) -> (Vec<Piece<'_>>, &str) {
    let mut out = vec![];
    let mut text = String::new();
    let mut i = 0;
    while i < src.len() {
        let rest = &src[i..];
        if !rest.starts_with('<') {
            let end = rest.find('<').unwrap_or(rest.len());
            text.push_str(&rest[..end]);
            i += end;
            continue;
        }
        let read = if rest.starts_with("<!--") {
            match rest[4..].find("-->") {
                Some(end) => Read::Piece(Piece::Comment, 4 + end + 3),
                None => Read::Incomplete,
            }
        } else {
            tag(rest)
        };
        match read {
            Read::Piece(piece, len) => {
                if !text.is_empty() {
                    out.push(Piece::Text(decode(&std::mem::take(&mut text))));
                }
                out.push(piece);
                i += len;
            }
            Read::Incomplete => {
                if !text.is_empty() {
                    out.push(Piece::Text(decode(&text)));
                }
                return (out, rest);
            }
            Read::NotATag => {
                text.push('<');
                i += 1;
            }
        }
    }
    if !text.is_empty() {
        out.push(Piece::Text(decode(&text)));
    }
    (out, "")
}

enum Read<'a> {
    Piece(Piece<'a>, usize),
    Incomplete,
    NotATag,
}

fn tag(src: &str) -> Read<'_> {
    let bytes = src.as_bytes();
    let closing = bytes.get(1) == Some(&b'/');
    let start = if closing { 2 } else { 1 };
    if !bytes.get(start).is_some_and(u8::is_ascii_alphabetic) {
        return Read::NotATag;
    }
    let mut i = start;
    while bytes
        .get(i)
        .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'-')
    {
        i += 1;
    }
    let name = src[start..i].to_ascii_lowercase();
    let mut attrs = vec![];
    loop {
        while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        match bytes.get(i) {
            None => return Read::Incomplete,
            Some(b'>') => break,
            Some(b'/') => {
                i += 1;
                continue;
            }
            Some(_) => {}
        }
        let key_start = i;
        while bytes
            .get(i)
            .is_some_and(|b| !b.is_ascii_whitespace() && !matches!(b, b'=' | b'>' | b'/'))
        {
            i += 1;
        }
        let key = src[key_start..i].to_ascii_lowercase();
        let mut j = i;
        while bytes.get(j).is_some_and(u8::is_ascii_whitespace) {
            j += 1;
        }
        if bytes.get(j) != Some(&b'=') {
            attrs.push((key, String::new()));
            continue;
        }
        j += 1;
        while bytes.get(j).is_some_and(u8::is_ascii_whitespace) {
            j += 1;
        }
        let value = match bytes.get(j) {
            None => return Read::Incomplete,
            Some(&quote @ (b'"' | b'\'')) => {
                let Some(len) = src[j + 1..].find(quote as char) else {
                    return Read::Incomplete;
                };
                let value = &src[j + 1..j + 1 + len];
                i = j + 1 + len + 1;
                value
            }
            Some(_) => {
                let value_start = j;
                while bytes
                    .get(j)
                    .is_some_and(|b| !b.is_ascii_whitespace() && *b != b'>')
                {
                    j += 1;
                }
                i = j;
                &src[value_start..j]
            }
        };
        attrs.push((key, decode(value)));
    }
    let len = i + 1;
    Read::Piece(
        Piece::Tag(Tag {
            name,
            closing,
            attrs,
            raw: &src[..len],
        }),
        len,
    )
}

/// The character references a README uses: the named ones HTML escapes with, and numeric ones.
pub fn decode(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest[1..].find(';').map(|e| e + 1).filter(|&e| e <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let name = &rest[1..end];
        let ch = match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some('\u{a0}'),
            _ => name
                .strip_prefix("#x")
                .or_else(|| name.strip_prefix("#X"))
                .map(|hex| u32::from_str_radix(hex, 16))
                .or_else(|| name.strip_prefix('#').map(str::parse))
                .and_then(Result::ok)
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn only_tag(src: &str) -> Tag<'_> {
        match pieces(src) {
            (mut p, "") if p.len() == 1 => match p.remove(0) {
                Piece::Tag(tag) => tag,
                other => panic!("{src}: {other:?}"),
            },
            other => panic!("{src}: {other:?}"),
        }
    }

    #[test]
    fn attributes_read_in_any_order_and_with_any_quotes() {
        for src in [
            r#"<img src="a.png" alt="Logo" width=120>"#,
            r#"<img width=120 alt='Logo' src='a.png'>"#,
            r#"<IMG ALT=Logo SRC=a.png />"#,
            "<img\n  alt=\"Logo\"\n  src = \"a.png\"/>",
        ] {
            let tag = only_tag(src);
            assert_eq!(tag.name, "img", "{src}");
            assert!(!tag.closing, "{src}");
            assert_eq!(tag.attr("src"), Some("a.png"), "{src}");
            assert_eq!(tag.attr("alt"), Some("Logo"), "{src}");
            assert_eq!(tag.raw, src);
        }
    }

    #[test]
    fn values_decode_their_references_and_keep_what_the_other_quote_holds() {
        let tag = only_tag(r#"<a href="https://x.dev/?a=1&amp;b=2" title='say "hi"' hidden>"#);
        assert_eq!(tag.attr("href"), Some("https://x.dev/?a=1&b=2"));
        assert_eq!(tag.attr("title"), Some("say \"hi\""));
        assert_eq!(tag.attr("hidden"), Some(""));
        assert_eq!(only_tag("</A >").name, "a");
        assert!(only_tag("</a>").closing);
    }

    #[test]
    fn text_comments_and_lone_angle_brackets() {
        let (p, rest) = pieces("a &lt;b&gt; &#169; &#x1F600; &bogus; & c <!-- hidden --> 3 < 4");
        assert_eq!(
            p,
            vec![
                Piece::Text("a <b> © 😀 &bogus; & c ".into()),
                Piece::Comment,
                Piece::Text(" 3 < 4".into()),
            ]
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn a_tag_or_comment_cut_at_the_end_is_left_for_the_next_line() {
        assert_eq!(pieces("hi <img src=\"a"), (vec![Piece::Text("hi ".into())], "<img src=\"a"));
        assert_eq!(pieces("<img\n").1, "<img\n");
        assert_eq!(pieces("<!-- open").1, "<!-- open");
    }
}
