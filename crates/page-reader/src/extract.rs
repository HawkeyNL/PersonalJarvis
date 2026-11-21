//! Page body to title and plain text: decode with `encoding_rs`, tokenize
//! HTML with the `html5ever` tokenizer (no tree, no scripts, no network),
//! keep visible text and drop scripts, styles, navigation, forms and the
//! like.

use std::cell::RefCell;

use encoding_rs::{Encoding, UTF_16BE, UTF_16LE, UTF_8};
use html5ever::{
    tendril::StrTendril,
    tokenizer::{
        states::RawKind, BufferQueue, Tag, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer,
        TokenizerOpts,
    },
};
use url::Url;

use crate::{Page, MAX_TEXT_BYTES, MAX_TITLE_CHARS};

/// Elements whose content is never page text.
const SKIPPED: [&str; 17] = [
    "script", "style", "noscript", "template", "svg", "math", "nav", "footer", "form", "aside",
    "iframe", "object", "embed", "button", "select", "textarea", "dialog",
];
/// Elements that start a new line.
const BLOCKS: [&str; 30] = [
    "p",
    "div",
    "br",
    "li",
    "ul",
    "ol",
    "dl",
    "dt",
    "dd",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "tr",
    "td",
    "th",
    "table",
    "section",
    "article",
    "main",
    "header",
    "blockquote",
    "pre",
    "hr",
    "figure",
    "figcaption",
    "address",
    "details",
];
/// How far into the body a `<meta charset>` is looked for.
const META_PRESCAN_BYTES: usize = 1024;

/// The page at `url` from its body. `cut` says the body was cut at the read
/// limit; the text is cut at [`MAX_TEXT_BYTES`] too and both set
/// `truncated`.
pub fn page(url: &Url, body: &[u8], html: bool, charset: Option<&str>, cut: bool) -> Page {
    let encoding = charset
        .and_then(|label| Encoding::for_label(label.as_bytes()))
        .or_else(|| html.then(|| meta_charset(body)).flatten())
        .unwrap_or(UTF_8);
    // `decode` lets a byte order mark win over the label.
    let (decoded, _, _) = encoding.decode(body);
    let (title, mut text) = if html {
        html_text(&decoded)
    } else {
        (String::new(), clean(&decoded))
    };
    let title: String = collapse(&title).chars().take(MAX_TITLE_CHARS).collect();
    let mut truncated = cut;
    if text.len() > MAX_TEXT_BYTES {
        let mut end = MAX_TEXT_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        truncated = true;
    }
    Page {
        url: url.to_string(),
        title,
        text: text.trim().to_string(),
        truncated,
    }
}

/// The label of a `charset=` in the first bytes, as `<meta charset>` and
/// `<meta http-equiv content>` both spell it.
fn meta_charset(body: &[u8]) -> Option<&'static Encoding> {
    let head = String::from_utf8_lossy(&body[..body.len().min(META_PRESCAN_BYTES)]).to_lowercase();
    let at = head.find("charset=")? + "charset=".len();
    let label: String = head[at..]
        .trim_start_matches(['"', '\''])
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'))
        .collect();
    // A document whose meta could be read as ASCII is not UTF-16.
    Encoding::for_label(label.as_bytes()).map(|encoding| {
        if encoding == UTF_16LE || encoding == UTF_16BE {
            UTF_8
        } else {
            encoding
        }
    })
}

/// Characters a page could use to hide text from a human reader: controls
/// other than line breaks and tabs, zero-width and bidi controls.
fn is_hidden(c: char) -> bool {
    (c.is_control() && !matches!(c, '\n' | '\t'))
        || matches!(
            c,
            '\u{ad}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2069}' | '\u{feff}'
        )
}

fn clean(text: &str) -> String {
    text.chars().filter(|c| !is_hidden(*c)).collect()
}

/// Whitespace runs to one space.
fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Default)]
struct Collect {
    title: String,
    text: String,
    in_title: bool,
    /// Open skipped elements, by name.
    skipped: Vec<(String, usize)>,
    space: bool,
}

impl Collect {
    fn skipping(&self) -> bool {
        self.skipped.iter().any(|(_, open)| *open > 0)
    }

    fn line(&mut self) {
        self.space = false;
        if !self.text.is_empty() && !self.text.ends_with('\n') {
            self.text.push('\n');
        }
    }

    fn chars(&mut self, chars: &str) {
        if self.in_title {
            if self.title.len() < MAX_TITLE_CHARS * 4 {
                self.title.push_str(chars);
            }
            return;
        }
        // A little beyond the cap, so `page` sees that it was crossed.
        if self.skipping() || self.text.len() > MAX_TEXT_BYTES {
            return;
        }
        for c in chars.chars().filter(|c| !is_hidden(*c)) {
            if self.text.len() > MAX_TEXT_BYTES {
                break;
            }
            if c.is_whitespace() {
                self.space = true;
            } else {
                if self.space && !self.text.is_empty() && !self.text.ends_with('\n') {
                    self.text.push(' ');
                }
                self.space = false;
                self.text.push(c);
            }
        }
    }

    fn tag(&mut self, tag: &Tag) -> TokenSinkResult<()> {
        let name: &str = &tag.name;
        let start = tag.kind == TagKind::StartTag;
        let raw = match name {
            "script" => Some(RawKind::ScriptData),
            "style" | "xmp" | "iframe" | "noembed" | "noframes" | "noscript" => {
                Some(RawKind::Rawtext)
            }
            "title" | "textarea" => Some(RawKind::Rcdata),
            _ => None,
        };
        if name == "title" {
            self.in_title = start && !tag.self_closing && self.title.is_empty();
        } else if SKIPPED.contains(&name) {
            let open = match self.skipped.iter().position(|(open, _)| open == name) {
                Some(at) => &mut self.skipped[at].1,
                None => {
                    self.skipped.push((name.to_string(), 0));
                    &mut self.skipped.last_mut().expect("just pushed").1
                }
            };
            // A raw-text element always runs to its end tag.
            if start && (raw.is_some() || !tag.self_closing) {
                *open += 1;
            } else if !start {
                *open = open.saturating_sub(1);
            }
        }
        if BLOCKS.contains(&name) {
            self.line();
        }
        match raw {
            Some(kind) if start => TokenSinkResult::RawData(kind),
            _ if start && name == "plaintext" => TokenSinkResult::Plaintext,
            _ => TokenSinkResult::Continue,
        }
    }
}

struct Sink(RefCell<Collect>);

impl TokenSink for Sink {
    type Handle = ();

    fn process_token(&self, token: Token, _line: u64) -> TokenSinkResult<()> {
        let mut collect = self.0.borrow_mut();
        match token {
            Token::CharacterTokens(chars) => collect.chars(&chars),
            Token::TagToken(tag) => return collect.tag(&tag),
            _ => {}
        }
        TokenSinkResult::Continue
    }
}

fn html_text(html: &str) -> (String, String) {
    let tokenizer = Tokenizer::new(Sink(RefCell::default()), TokenizerOpts::default());
    let input = BufferQueue::default();
    input.push_back(StrTendril::from(html));
    // The sink never returns `Script`, so one feed reads all input.
    let _ = tokenizer.feed(&input);
    tokenizer.end();
    let collect = tokenizer.sink.0.take();
    (collect.title, collect.text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(body: &str) -> Page {
        page(
            &Url::parse("https://example.com/a").unwrap(),
            body.as_bytes(),
            true,
            None,
            false,
        )
    }

    #[test]
    fn keeps_visible_text_and_the_title() {
        let page = read(
            "<!doctype html><html><head><title> Rente \n daalt </title>\
             <style>p { color: red }</style><script>var x = '</p>'; ignore()</script>\
             <meta name=description content=hidden></head><body>\
             <nav><a href=/>Home</a></nav><header><h1>De rente daalt</h1></header>\
             <main><p>De ECB  verlaagt de&nbsp;rente.</p><p>Tweede &amp; laatste.</p>\
             <form><input value=x><button>Verstuur</button></form>\
             <script type=module>alert('</script')</script><aside>Advertentie</aside>\
             <svg><text>logo</text></svg><!-- comment --></main>\
             <footer>Cookies</footer></body></html>",
        );
        assert_eq!(page.title, "Rente daalt");
        assert_eq!(
            page.text,
            "De rente daalt\nDe ECB verlaagt de rente.\nTweede & laatste."
        );
        assert!(!page.truncated);
        assert_eq!(page.url, "https://example.com/a");
    }

    #[test]
    fn hidden_characters_are_dropped() {
        let page = read("<p>a\u{200b}b\u{202e}c\u{0}d\u{7}e</p>");
        assert_eq!(page.text, "abcde");
    }

    #[test]
    fn unclosed_and_self_closing_skips() {
        assert_eq!(read("<p>a</p><svg/><p>b</p>").text, "a\nb");
        assert_eq!(read("<p>a</p><nav><p>menu").text, "a");
        assert_eq!(read("<p>a</p><script/>x</script><p>b</p>").text, "a\nb");
        assert_eq!(read("<p>a</p></nav></nav><p>b</p>").text, "a\nb");
        assert_eq!(read("<plaintext><b>x</b>").text, "<b>x</b>");
    }

    #[test]
    fn decodes_by_header_meta_and_bom() {
        let url = Url::parse("https://example.com/").unwrap();
        let latin = b"<p>caf\xe9</p>";
        assert_eq!(
            page(&url, latin, true, Some("windows-1252"), false).text,
            "café"
        );
        let meta = b"<meta charset=\"iso-8859-1\"><p>caf\xe9</p>";
        assert_eq!(page(&url, meta, true, None, false).text, "café");
        let http_equiv =
            b"<meta http-equiv=Content-Type content='text/html; charset=windows-1252'><p>\x80</p>";
        assert_eq!(page(&url, http_equiv, true, None, false).text, "€");
        // A byte order mark wins over the label.
        let bom = b"\xef\xbb\xbf<p>caf\xc3\xa9</p>";
        assert_eq!(
            page(&url, bom, true, Some("windows-1252"), false).text,
            "café"
        );
        // A meta claiming UTF-16 in an ASCII-readable document is ignored.
        let utf16 = b"<meta charset=utf-16><p>caf\xc3\xa9</p>";
        assert_eq!(page(&url, utf16, true, None, false).text, "café");
        // Invalid UTF-8 is replaced, not refused.
        assert_eq!(
            page(&url, b"<p>a\xffb</p>", true, None, false).text,
            "a\u{fffd}b"
        );
    }

    #[test]
    fn plain_text_is_kept_as_it_is() {
        let url = Url::parse("https://example.com/robots.txt").unwrap();
        let page = page(&url, b"<b>not html</b>\n\tline\x1b[31m", false, None, false);
        assert_eq!(page.text, "<b>not html</b>\n\tline[31m");
        assert_eq!(page.title, "");
    }

    #[test]
    fn text_and_title_are_capped() {
        let long = format!(
            "<title>{}</title><p>{}é</p>",
            "t".repeat(1000),
            "x".repeat(MAX_TEXT_BYTES - 1)
        );
        let page = read(&long);
        assert_eq!(page.title.chars().count(), MAX_TITLE_CHARS);
        assert!(page.text.len() <= MAX_TEXT_BYTES, "{}", page.text.len());
        assert!(page.truncated);
        let url = Url::parse("https://example.com/").unwrap();
        assert!(super::page(&url, b"<p>a</p>", true, None, true).truncated);
    }
}
