//! Deterministic privacy checks for text that may leave the Home Node in a
//! web search: the research question or a search query. Names and
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
/// Digits split by separators (`4111 1111`, `06-1234`) are private
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
        || has_digit_run(question, false)
        || has_amount(&lower)
        || has_key(question)
        || has_possessive(&lower)
        || held_symbols
            .iter()
            .any(|symbol| has_word(&lower, &symbol.trim().to_lowercase()))
}

/// Whether text that leaves the host in a search carries a personal
/// identifier: an email address, a key-like token, a possessive near a money
/// or account word, or a long number (account, card or phone; a plain date
/// is not one). Narrower than [`is_private`]: no amounts, `k` suffix or held
/// symbols, so ordinary price and date searches pass. It gates every
/// Research question and every search query a worker sees.
pub fn leaks_identifier(text: &str) -> bool {
    has_email(text)
        || has_key(text)
        || has_possessive(&text.to_lowercase())
        || has_digit_run(text, true)
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

/// How many separator characters may join two digit groups, of which at
/// most one may be other than whitespace (`06 - 1234`, not `1234--5678`).
const MAX_SEPARATOR_RUN: usize = 3;

/// 6+ contiguous digits, or digit groups joined by separators (see
/// [`digit_runs`]) with 8+ digits or 6+ groups in total (IBAN, card and
/// phone numbers). Any numeric character counts, superscripts and fullwidth
/// digits included. Other punctuation or
/// a letter ends a number, so `06,1234,5678` passes: this is a heuristic,
/// not a complete detector.
/// With `dates_allowed`, a run that is exactly one date is not private.
fn has_digit_run(text: &str, dates_allowed: bool) -> bool {
    digit_runs(text).iter().any(|run| {
        let digits = |group: &String| group.chars().count();
        let total: usize = run.iter().map(|(_, group)| digits(group)).sum();
        (run.iter().any(|(_, group)| digits(group) >= MIN_DIGIT_RUN)
            || total >= MIN_SEPARATED_DIGITS
            || run.len() >= MIN_DIGIT_RUN)
            && !(dates_allowed && (is_date(run) || is_year_range(run)))
    })
}

/// Two plausible years and nothing else (`2024-2025`, `1990 – 2000`).
fn is_year_range(run: &[(char, String)]) -> bool {
    let year = |digits: &String| {
        digits.len() == 4
            && digits
                .parse::<u16>()
                .is_ok_and(|year| (1900..=2099).contains(&year))
    };
    matches!(run, [(_, from), (_, to)] if year(from) && year(to))
}

/// Maximal runs of digit groups, each with the separator before it: the
/// single separator character, `' '` for a longer joint, `'\0'` for the
/// first. Groups are joined by up to [`MAX_SEPARATOR_RUN`] separators (see
/// [`is_separator`]) with at most one that is not whitespace. Invisible
/// characters are dropped first, so they neither join nor split a number.
fn digit_runs(text: &str) -> Vec<Vec<(char, String)>> {
    let (mut runs, mut run) = (Vec::new(), Vec::new());
    let (mut group, mut joint, mut before) = (String::new(), String::new(), '\0');
    let visible = text.chars().filter(|c| !is_invisible(*c));
    for c in visible.chain(['\0']) {
        // Any numeric character, superscripts included: only an amount
        // needs a real digit (see `is_digit`).
        if c.is_numeric() {
            if group.is_empty() {
                before = match (run.is_empty(), joint.chars().count()) {
                    (true, _) => '\0',
                    (false, 1) => joint.chars().next().unwrap_or(' '),
                    (false, _) => ' ',
                };
                joint.clear();
            }
            group.push(c);
            continue;
        }
        if !group.is_empty() {
            run.push((before, std::mem::take(&mut group)));
        }
        let joins = !run.is_empty()
            && is_separator(c)
            && joint.chars().count() < MAX_SEPARATOR_RUN
            && (c.is_whitespace() || joint.chars().all(char::is_whitespace));
        if joins {
            joint.push(c);
            continue;
        }
        // Any other character, or too long a joint, ends the run.
        joint.clear();
        if !run.is_empty() {
            runs.push(std::mem::take(&mut run));
        }
    }
    runs
}

/// Any whitespace (no-break spaces included), `-`, `.`, `/`, `_`, and their
/// Unicode look-alikes: hyphens and dashes, the minus sign, middle dots and
/// bullet operators, fullwidth and small forms, the fraction and division
/// slashes.
fn is_separator(c: char) -> bool {
    const LOOK_ALIKES: [char; 17] = [
        '\u{b7}', '\u{2027}', '\u{2043}', '\u{2044}', '\u{2212}', '\u{2215}', '\u{2219}',
        '\u{22c5}', '\u{2e3a}', '\u{2e3b}', '\u{30fb}', '\u{fe58}', '\u{fe63}', '\u{ff0d}',
        '\u{ff0e}', '\u{ff0f}', '\u{ff65}',
    ];
    c.is_whitespace()
        || matches!(c, '-' | '.' | '/' | '_')
        || ('\u{2010}'..='\u{2015}').contains(&c)
        || LOOK_ALIKES.contains(&c)
}

/// Ignored inside a number, so it can neither split nor hide one:
/// non-whitespace control characters, format and bidi controls, zero-width
/// characters, blank fillers, combining marks, variation selectors and tags.
/// Visible symbols (`€`, `×`, `→`) and letters still end a number.
fn is_invisible(c: char) -> bool {
    (c.is_control() && !c.is_whitespace())
        || matches!(
            c,
            '\u{ad}'
                | '\u{300}'..='\u{36f}'
                | '\u{483}'..='\u{489}'
                | '\u{61c}'
                | '\u{115f}'
                | '\u{1160}'
                | '\u{17b4}'
                | '\u{17b5}'
                | '\u{180b}'..='\u{180f}'
                | '\u{1ab0}'..='\u{1aff}'
                | '\u{1dc0}'..='\u{1dff}'
                | '\u{200b}'..='\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2060}'..='\u{206f}'
                | '\u{20d0}'..='\u{20ff}'
                | '\u{2800}'
                | '\u{3164}'
                | '\u{fe00}'..='\u{fe0f}'
                | '\u{fe20}'..='\u{fe2f}'
                | '\u{feff}'
                | '\u{ffa0}'
                | '\u{fff0}'..='\u{fffb}'
                | '\u{1bca0}'..='\u{1bca3}'
                | '\u{1d173}'..='\u{1d17a}'
                | '\u{e0000}'..='\u{e0fff}'
        )
}

/// A digit in an amount: any script (fullwidth included), but not a
/// superscript, subscript, fraction or Roman numeral, so `m²` or `½` next to
/// a currency word is no amount.
fn is_digit(c: char) -> bool {
    c.is_numeric()
        && !matches!(
            c,
            '\u{b2}' | '\u{b3}' | '\u{b9}' | '\u{bc}'..='\u{be}' | '\u{2070}'..='\u{209f}' | '\u{2150}'..='\u{218f}'
        )
}

/// The whole run is one plausible date: `YYYY-MM-DD`, `YYYY/MM/DD`,
/// `DD-MM-YYYY`, `DD/MM/YYYY` or `DD.MM.YYYY`, with a 1-2 digit day and
/// month and a year from 1900 to 2099, each joined by one ASCII separator.
/// A digit group joined to it makes the run longer, so it is no longer a
/// date.
fn is_date(run: &[(char, String)]) -> bool {
    let [(_, first), (separator, middle), (second_separator, last)] = run else {
        return false;
    };
    let (year, day) = match separator {
        '-' | '/' if first.len() == 4 => (first, last),
        '-' | '/' | '.' => (last, first),
        _ => return false,
    };
    // ASCII digits only: `parse` refuses others, so `len` counts digits.
    let number = |digits: &str| digits.parse::<u16>().ok();
    separator == second_separator
        && year.len() == 4
        && (1..=2).contains(&day.len())
        && (1..=2).contains(&middle.len())
        && number(year).is_some_and(|year| (1900..=2099).contains(&year))
        && number(middle).is_some_and(|month| (1..=12).contains(&month))
        && number(day).is_some_and(|day| (1..=31).contains(&day))
}

/// A currency sign or word with a number anywhere in the question, or a
/// number with a `k` suffix ("80k").
fn has_amount(lower: &str) -> bool {
    let currency =
        lower.contains(['€', '$', '£']) || CURRENCY_WORDS.iter().any(|word| has_word(lower, word));
    (currency && lower.contains(is_digit))
        || runs(lower, char::is_alphanumeric).any(|word| {
            word.strip_suffix('k')
                .is_some_and(|digits| !digits.is_empty() && digits.chars().all(is_digit))
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
            ("bel 06\u{a0}1234\u{a0}5678", true),
            ("bel 06\u{202f}12\u{202f}34\u{202f}56\u{202f}78", true),
            ("4111\u{a0}1111\u{a0}1111\u{a0}1111 kaart", true),
            ("4111–1111–1111–1111", true),
            ("4111_1111_1111_1111", true),
            ("06\t1234\t5678", true),
        ] {
            assert_eq!(is_private(question, &held), private, "{question}");
        }
        // Without holdings a symbol is just a word.
        assert!(!is_private("ASML nieuws vandaag", &[]));
        assert!(!is_private("blank symbol", &[" ".to_owned()]));
    }

    #[test]
    fn identifier_gate_table() {
        for (text, leaks) in [
            ("bitcoin price usd 2026", false),
            ("nieuws 2026-11-10", false),
            ("EUR/USD koers 2026", false),
            ("bitcoin 100k prediction", false),
            ("ASML kwartaalcijfers Q3 2026", false),
            ("10/11/2026 storm", false),
            ("storm 2026/11/10 en 1-1-2027", false),
            ("verkiezingen 29.10.2025 uitslag", false),
            ("is €2500 per maand veel?", false),
            ("Rust 1.82.0 release notes", false),
            ("my bike was stolen", false),
            ("jan@example.com", true),
            ("06-12345678", true),
            ("0612345678", true),
            ("06 1234 5678", true),
            ("+31 6 12 34 56 78", true),
            ("NL91ABNA0417164300", true),
            ("NL91 ABNA 0417 1643 00", true),
            ("4111 1111 1111 1111", true),
            ("4111-1111-1111-1111", true),
            ("06\u{a0}1234\u{a0}5678", true),
            ("06\u{202f}12\u{202f}34\u{202f}56\u{202f}78", true),
            ("4111\u{a0}1111\u{a0}1111\u{a0}1111", true),
            ("4111\u{202f}1111\u{202f}1111\u{202f}1111", true),
            ("06–1234–5678", true),
            ("10\u{a0}11\u{a0}2026", true),
            ("10–11–2026", true),
            ("4111—1111—1111—1111", true),
            ("06\u{2011}1234\u{2011}5678", true),
            ("06\u{2212}1234\u{2212}5678", true),
            ("06\u{200b}1234\u{200b}5678", true),
            ("06\u{ad}1234\u{ad}5678", true),
            ("06123\u{200b}\u{200b}45678", true),
            ("06 - 1234 - 5678", true),
            ("06 \u{2013} 12 34 56 78", true),
            ("０６１２３４５６７８", true),
            ("06\u{ff0d}1234\u{ff0d}5678", true),
            ("4111\u{ff0e}1111\u{ff0e}1111\u{ff0e}1111", true),
            ("2026 - 11 - 10", true),
            ("1234--5678", false),
            ("1234 -- 5678", false),
            ("06\u{200e}1234\u{200e}5678", true),
            ("06\u{fe0f}1234\u{fe0f}5678", true),
            ("06\u{301}1234\u{301}5678", true),
            ("06\u{e0020}1234\u{e0020}5678", true),
            ("06·1234·5678", true),
            ("06\u{30fb}1234\u{30fb}5678", true),
            ("06\u{22c5}1234\u{22c5}5678", true),
            ("06\u{1f}1234\u{1f}5678", true),
            ("06\u{7f}1234\u{7f}5678", true),
            ("06\u{3164}1234\u{3164}5678", true),
            ("06\u{202e}1234\u{202e}5678", true),
            // Visible symbols end a number: price ranges and resolutions.
            ("laptop €1000 - €2000", false),
            ("prijs €1500 – €2500", false),
            ("1920×1080 monitor", false),
            ("2024→2025", false),
            ("prijs per m² in 2026", false),
            ("⁰⁶¹²³⁴⁵⁶⁷⁸", true),
            ("₀₆₁₂₃₄₅₆₇₈", true),
            ("06\u{2800}1234\u{2800}5678", true),
            ("06\u{1bca0}1234\u{1bca0}5678", true),
            ("06\u{1d173}1234\u{1d173}5678", true),
            // Two years are a range, not a number; a third group is.
            ("inflatie 2024 - 2025", false),
            ("2024-2025 verkiezingen", false),
            ("1990 – 2000", false),
            ("2024 - 2025 06", true),
            ("1234-2025", true),
            // Only `-`, `/` and `.` make a date.
            ("2024_11_08", true),
            ("2024–11–08", true),
            ("2024\u{a0}11\u{a0}08", true),
            ("2024\n11\n08", true),
            ("mijn portfolio rendement", true),
            ("my brokerage account", true),
            ("sk-proj-a1b2c3d4e5f6g7h8i9j0k1l2", true),
            ("token a1b2c3d4e5f6g7h8i9j0k1l2m3", true),
            // A number joined to a date makes the run longer: not a date.
            ("2026-11-10 06", true),
            ("06 10-11-2026", true),
            ("10-11-2026-1234", true),
            ("2026-11-10/12", true),
            // Not a plausible date: the digits count.
            ("06-12-3456", true),
            ("31-13-2026", true),
            ("00-11-2026", true),
            ("2026-11-32", true),
            ("1234-11-10", true),
            ("2026.11.10", true),
            ("10-11/2026", true),
            ("10 11 2026", true),
            ("100-11-2026", true),
            ("10-011-2026", true),
            // Year limits, a short date, and dates joined into one run.
            ("01-01-1899", true),
            ("31-12-2099", false),
            ("01-01-2100", true),
            ("1/1/2026", false),
            ("10-11-2026 10-11-2026", true),
            ("10-11-2026  0612", true),
            ("10-11-2026, 0612", false),
        ] {
            assert_eq!(leaks_identifier(text), leaks, "{text}");
        }
        // The auto-mode gate still counts dates as long numbers.
        assert!(is_private("nieuws 2026-11-10", &[]));
        assert!(!is_private("1234--5678", &[]));
        assert!(is_private("is €２５００ per maand veel?", &[]));
        assert!(is_private("salary of ８０k", &[]));
        assert!(is_private("inflatie 2024 - 2025", &[]));
        assert!(!is_private("prijs per m² in de stad", &[]));
        assert!(!is_private("prijs in euro per m²", &[]));
        assert!(!is_private("½ liter melk", &[]));
    }
}
