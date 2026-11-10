//! Deterministic privacy checks for text that may leave the Home Node in a
//! web search: the research question, a search query or a URL. Names and
//! other free-text personal details are not detectable; the research prompt
//! asks the model to keep them out of queries.

/// How many words after a possessive still count as near.
const POSSESSIVE_REACH: usize = 3;
/// With the typographic apostrophe phone keyboards insert, too.
const POSSESSIVES: [&str; 7] = ["my", "our", "mijn", "m'n", "m’n", "ons", "onze"];
/// Prefixes, so plurals and compounds match too ("posities", "brokerage").
const PERSONAL_WORDS: [&str; 29] = [
    "portfolio",
    "positie",
    "position",
    "holding",
    "account",
    "rekening",
    "ibkr",
    "broker",
    "salaris",
    "salary",
    "wachtwoord",
    "password",
    "aandeel",
    "aandelen",
    "belegging",
    "hypotheek",
    "pensioen",
    "saldo",
    "spaar",
    "schuld",
    "share",
    "stock",
    "invest",
    "mortgage",
    "pension",
    "balance",
    "debt",
    "loan",
    "lening",
];
/// Currency words that make a number an amount (whole words, any case).
const CURRENCY_WORDS: [&str; 7] = ["eur", "euro", "euros", "usd", "dollar", "dollars", "gbp"];
/// A contiguous digit run this long is private (phone, account numbers).
const MIN_DIGIT_RUN: usize = 6;
/// Digits split by single separators (`4111 1111`, `06-1234`) are private
/// from this many digits on, or from [`MIN_DIGIT_RUN`] groups on (`1 2 3 4 5 6`).
const MIN_SEPARATED_DIGITS: usize = 8;
const KEY_PREFIXES: [&str; 3] = ["sk-", "ghp_", "AKIA"];
const MIN_KEY_RUN: usize = 24;

/// Whether the question may carry personal or financial details and must
/// stay an offer: an email address, a long number, a currency amount, a
/// key-like token, a possessive near a money or account word, or a held
/// symbol.
pub fn is_private(question: &str, held_symbols: &[String]) -> bool {
    let lower = question.to_lowercase();
    has_email(question)
        || has_digit_run(question)
        || has_amount(&lower)
        || has_key(question)
        || has_possessive(&lower)
        || held_symbols
            .iter()
            .any(|symbol| has_word(&lower, &symbol.trim().to_lowercase()))
}

fn has_email(text: &str) -> bool {
    text.split_whitespace().any(|token| {
        token.split_once('@').is_some_and(|(local, domain)| {
            local.chars().any(char::is_alphanumeric)
                && domain.split_once('.').is_some_and(|(host, tld)| {
                    !host.is_empty() && tld.chars().any(char::is_alphanumeric)
                })
        })
    })
}

/// 6+ contiguous digits, or digit groups joined by single ` `, `-`, `.` or
/// `/` with 8+ digits or 6+ groups in total (IBAN, card and phone numbers).
fn has_digit_run(text: &str) -> bool {
    let (mut total, mut groups, mut group, mut after_separator) = (0, 0, 0, false);
    for c in text.chars() {
        if c.is_ascii_digit() {
            groups += usize::from(group == 0);
            group += 1;
            total += 1;
            after_separator = false;
            if group >= MIN_DIGIT_RUN || total >= MIN_SEPARATED_DIGITS || groups >= MIN_DIGIT_RUN {
                return true;
            }
        } else if group > 0 && !after_separator && matches!(c, ' ' | '-' | '.' | '/') {
            after_separator = true;
            group = 0;
        } else {
            (total, groups, group, after_separator) = (0, 0, 0, false);
        }
    }
    false
}

/// A currency sign or word with a number anywhere in the question, or a
/// number with a `k` suffix ("80k").
fn has_amount(lower: &str) -> bool {
    let currency =
        lower.contains(['€', '$', '£']) || CURRENCY_WORDS.iter().any(|word| has_word(lower, word));
    (currency && lower.contains(|c: char| c.is_ascii_digit()))
        || runs(lower, char::is_alphanumeric).any(|word| {
            word.strip_suffix('k').is_some_and(|digits| {
                !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
            })
        })
}

fn has_key(text: &str) -> bool {
    text.contains("-----BEGIN")
        || KEY_PREFIXES
            .iter()
            .any(|prefix| has_prefix_word(text, prefix))
        || runs(text, |c| c.is_ascii_alphanumeric()).any(|run| {
            run.len() >= MIN_KEY_RUN
                && run.contains(|c: char| c.is_ascii_digit())
                && run.contains(|c: char| c.is_ascii_alphabetic())
        })
}

fn has_possessive(lower: &str) -> bool {
    let words: Vec<&str> = runs(lower, |c| c.is_alphanumeric() || c == '\'' || c == '’').collect();
    words.iter().enumerate().any(|(index, word)| {
        POSSESSIVES.contains(word)
            && words[index + 1..]
                .iter()
                .take(POSSESSIVE_REACH)
                .any(|next| {
                    PERSONAL_WORDS
                        .iter()
                        .any(|personal| next.starts_with(personal))
                })
    })
}

/// Maximal runs of characters that satisfy `keep`.
fn runs(text: &str, keep: impl Fn(char) -> bool) -> impl Iterator<Item = &str> {
    text.split(move |c: char| !keep(c))
        .filter(|run| !run.is_empty())
}

/// `needle` (already lower case) as a whole word: no letter or digit
/// directly before or after it.
fn has_word(lower: &str, needle: &str) -> bool {
    !needle.is_empty()
        && lower.match_indices(needle).any(|(at, _)| {
            !lower[..at]
                .chars()
                .next_back()
                .is_some_and(char::is_alphanumeric)
                && !lower[at + needle.len()..]
                    .chars()
                    .next()
                    .is_some_and(char::is_alphanumeric)
        })
}

/// `prefix` at the start of a word, case sensitive.
fn has_prefix_word(text: &str, prefix: &str) -> bool {
    text.match_indices(prefix).any(|(at, _)| {
        !text[..at]
            .chars()
            .next_back()
            .is_some_and(char::is_alphanumeric)
    })
}

/// Whether a URL may be opened or searched without carrying personal data:
/// http(s) only, no user info, no `?` query or `#` fragment, a domain host
/// (never an IP literal) and a path that [`is_private`] does not flag.
pub fn url_is_safe(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    matches!(parsed.scheme(), "http" | "https")
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.query().is_none()
        && parsed.fragment().is_none()
        // `domain()` is `None` for an IPv4 or IPv6 literal.
        && parsed.domain().is_some()
        && !is_private(&percent_decoded(parsed.path()), &[])
}

/// `%XX` escapes decoded (lossy), so an encoded `@` or digit still counts.
fn percent_decoded(path: &str) -> String {
    let mut decoded = Vec::with_capacity(path.len());
    let mut rest = path.as_bytes();
    while let Some((&first, tail)) = rest.split_first() {
        let escaped = tail
            .get(..2)
            .filter(|hex| first == b'%' && hex.iter().all(u8::is_ascii_hexdigit))
            .and_then(|hex| u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok());
        match escaped {
            Some(byte) => {
                decoded.push(byte);
                rest = &tail[2..];
            }
            None => {
                decoded.push(first);
                rest = tail;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn privacy_gate_table() {
        let held = ["ASML".to_owned(), "BRK.B".to_owned()];
        for (question, private) in [
            ("weer Amsterdam morgen", false),
            ("What is the latest Rust release?", false),
            ("Wat is de koers van EUR/USD vandaag?", false),
            ("Wie won de Tour de France in 2024?", false),
            ("Is a risk-free rate the same as a bond yield?", false),
            ("Hoe werkt een Nederlandse bankrekening?", false),
            ("Mijn fiets is gestolen, wat nu?", false),
            ("mail jan.devries@example.nl over het weer", true),
            ("mijn telefoon is 0612345678", true),
            ("NL91ABNA0417164300 rente?", true),
            ("is €2500 per maand veel?", true),
            ("is 2500 EUR per maand veel?", true),
            ("$40 for a flight to Rome?", true),
            ("salary of 80k in Amsterdam", true),
            ("is sk-proj-abc123 still valid?", true),
            ("leaked ghp_abcdef?", true),
            ("what does AKIAIOSFODNN7EXAMPLE belong to?", true),
            ("-----BEGIN OPENSSH PRIVATE KEY-----", true),
            ("token a1b2c3d4e5f6g7h8i9j0k1l2m3", true),
            ("hoe doet mijn portfolio het?", true),
            ("Hoe staan mijn IBKR posities?", true),
            ("how is my brokerage account doing", true),
            ("is m'n wachtwoord veilig?", true),
            ("hoe staat m’n rekening ervoor?", true),
            ("my salary vs inflation", true),
            ("ASML nieuws vandaag", true),
            ("nieuws over asml?", true),
            ("koers brk.b", true),
            ("ASMLX nieuws", false),
            ("Rust 1.82.0 release notes", false),
            ("top 10 films van 2024", false),
            ("hoe laat is het in Tokyo om 9 uur?", false),
            ("waarom is ons land zo nat?", false),
            ("NL91 ABNA 0417 1643 00", true),
            ("is 4111 1111 1111 1111 een testkaart?", true),
            ("bel 06 1234 5678", true),
            ("bel +31 6 12 34 56 78", true),
            ("06-1234-5678 nummer van wie?", true),
            ("1 2 3 4 5 6", true),
            ("is 2500 euro per maand veel?", true),
            ("100 dollars to yen", true),
            ("is £300 a lot?", true),
            ("300 GBP in EUR", true),
            ("hoe staat mijn aandelen pakket?", true),
            ("onze hypotheek rente", true),
            ("is our mortgage too high", true),
            ("my pension fund returns", true),
            ("my stocks today", true),
        ] {
            assert_eq!(is_private(question, &held), private, "{question}");
        }
        // Without holdings a symbol is just a word.
        assert!(!is_private("ASML nieuws vandaag", &[]));
        assert!(!is_private("blank symbol", &[" ".to_owned()]));
    }

    #[test]
    fn url_gate_table() {
        for (url, safe) in [
            ("https://www.rust-lang.org/", true),
            ("http://example.org/news/rust-release", true),
            ("https://nl.wikipedia.org/wiki/Hypotheek", true),
            ("https://example.org", true),
            ("https://example.org/a%20b", true),
            ("ftp://example.org/file", false),
            ("javascript:alert(1)", false),
            ("file:///etc/passwd", false),
            ("data:text/plain,hi", false),
            ("example.org/news", false),
            ("not a url", false),
            ("", false),
            ("https://user@example.org/", false),
            ("https://user:pw@example.org/", false),
            ("https://:pw@example.org/", false),
            ("https://example.org/search?q=weather", false),
            ("https://example.org/?", false),
            ("https://example.org/page#section", false),
            ("https://example.org/#", false),
            ("http://93.184.215.14/", false),
            ("http://127.0.0.1/", false),
            ("http://2130706433/", false),
            ("http://0x7f.1/", false),
            ("http://[::1]/", false),
            ("https://[2001:db8::1]/page", false),
            ("https://example.org/u/jan.devries@example.nl", false),
            ("https://example.org/u/jan.devries%40example.nl", false),
            ("https://example.org/call/0612345678", false),
            ("https://example.org/call/%30612345678", false),
            ("https://example.org/my-portfolio/today", false),
            ("https://example.org/mijn/rekening", false),
            ("https://example.org/pay/2500-eur", false),
            ("https://example.org/key/sk-proj-abc", false),
            ("https://example.org/archive/2024/11/08/rust", false),
            ("https://example.org/%zz/%4", true),
        ] {
            assert_eq!(url_is_safe(url), safe, "{url}");
        }
    }
}
