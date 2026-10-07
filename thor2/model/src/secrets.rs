//! Real secret tokens in a stretch of text: the ONE definition, shared by the
//! write gate (a note carrying one is refused), the log's own scrubbing of a
//! served command (`serve::deliver::redact_secrets`) and doctor's scan of what
//! is already stored.
//!
//! WHY THE WRITE GATE LOOKS. The log is append-only, a retract keeps history,
//! and the daily backup is that log pushed to a git remote. A token typed into
//! a note can therefore never be removed, only rotated. Refusing it at the
//! door is the only moment this is cheap.
//!
//! WHY SO FEW SHAPES. A false refusal is the most expensive thing the gate can
//! do, so only shapes that are all but never ordinary prose are listed, and
//! each carries a length floor: a bare `ghp_` or `sk_live_` in a rule ABOUT
//! secrets passes. Deliberately NOT here: the words password, token or key,
//! `name=value`, a long run of hex (a commit sha) and base64 in general. Those
//! would refuse "never pass --password on the command line" and "primary key:
//! id", which is the failure this module exists to avoid.
//!
//! No regex: `model` has no such dependency, and every shape here is a literal
//! prefix followed by a run of allowed characters, which a scan over the bytes
//! says plainly. Every match is pure ASCII, so its offsets are always char
//! boundaries and a slice of the text at them is safe.

/// One secret found in a text: which shape, and where (byte offsets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Found {
    pub shape: &'static str,
    pub start: usize,
    pub end: usize,
}

impl Found {
    /// The least that tells the writer WHICH token this is without handing it
    /// back: its first 6 and last 2 characters. A PEM header is public by
    /// nature and has no value to hide, so it carries no hint at all.
    pub fn hint(&self, text: &str) -> Option<String> {
        let token = &text[self.start..self.end];
        (self.shape != PEM && token.len() >= 12).then(|| format!("{}...{}", &token[..6], &token[token.len() - 2..]))
    }
}

const PEM: &str = "PEM private key header";

/// The characters a token's body may use after its prefix.
#[derive(Clone, Copy)]
enum Run {
    Alnum,
    AlnumUnderscore,
    AlnumDash,
    AlnumDashUnderscore,
    UpperDigit,
}

impl Run {
    fn allows(self, byte: u8) -> bool {
        let alnum = byte.is_ascii_alphanumeric();
        match self {
            Run::Alnum => alnum,
            Run::AlnumUnderscore => alnum || byte == b'_',
            Run::AlnumDash => alnum || byte == b'-',
            Run::AlnumDashUnderscore => alnum || byte == b'-' || byte == b'_',
            Run::UpperDigit => byte.is_ascii_uppercase() || byte.is_ascii_digit(),
        }
    }

    /// How many bytes at the start of `bytes` this run allows.
    fn len(self, bytes: &[u8]) -> usize {
        bytes.iter().take_while(|b| self.allows(**b)).count()
    }
}

/// A literal prefix, then at least `min` characters of `run`. `take` caps the
/// body at an exact length (an AWS key id is 16 long and nothing after it
/// belongs to it).
struct Shape {
    name: &'static str,
    prefixes: &'static [&'static str],
    run: Run,
    min: usize,
    take: Option<usize>,
}

const SHAPES: &[Shape] = &[
    Shape {
        name: "Stripe-style key",
        prefixes: &["sk_live_", "sk_test_", "pk_live_", "pk_test_", "rk_live_", "rk_test_"],
        run: Run::Alnum,
        min: 8,
        take: None,
    },
    Shape {
        name: "GitHub token",
        prefixes: &["ghp_", "gho_", "ghu_", "ghs_", "ghr_"],
        run: Run::Alnum,
        min: 20,
        take: None,
    },
    Shape { name: "GitHub fine-grained token", prefixes: &["github_pat_"], run: Run::AlnumUnderscore, min: 20, take: None },
    Shape {
        name: "Slack token",
        prefixes: &["xoxa-", "xoxb-", "xoxp-", "xoxr-", "xoxs-"],
        run: Run::AlnumDash,
        min: 10,
        take: None,
    },
    Shape { name: "AWS access key id", prefixes: &["AKIA"], run: Run::UpperDigit, min: 16, take: Some(16) },
    Shape { name: "Tailscale key", prefixes: &["tskey-"], run: Run::AlnumDash, min: 20, take: None },
    Shape { name: "Anthropic API key", prefixes: &["sk-ant-"], run: Run::AlnumDashUnderscore, min: 20, take: None },
    Shape { name: "Google API key", prefixes: &["AIza"], run: Run::AlnumDashUnderscore, min: 35, take: None },
];

/// Every secret in `text`, in order, none overlapping.
pub fn find(text: &str) -> Vec<Found> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        match match_at(bytes, at) {
            Some(hit) => {
                at = hit.end;
                found.push(hit);
            }
            None => at += 1,
        }
    }
    found
}

fn match_at(bytes: &[u8], at: usize) -> Option<Found> {
    if bytes[at] == b'-' {
        return pem_at(bytes, at);
    }
    // A token starts at a word boundary: `task_live_...` is not `sk_live_...`.
    if at > 0 && (bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_') {
        return None;
    }
    jwt_at(bytes, at).or_else(|| prefixed_at(bytes, at))
}

fn prefixed_at(bytes: &[u8], at: usize) -> Option<Found> {
    let rest = &bytes[at..];
    for shape in SHAPES {
        for prefix in shape.prefixes {
            if !rest.starts_with(prefix.as_bytes()) {
                continue;
            }
            let body = shape.run.len(&rest[prefix.len()..]);
            if body < shape.min {
                continue;
            }
            let body = shape.take.map_or(body, |exact| exact.min(body));
            return Some(Found { shape: shape.name, start: at, end: at + prefix.len() + body });
        }
    }
    None
}

/// `eyJ...` then two more dot-separated base64url parts: header.payload.signature.
fn jwt_at(bytes: &[u8], at: usize) -> Option<Found> {
    const FLOORS: [usize; 3] = [10, 10, 5];
    let rest = &bytes[at..];
    if !rest.starts_with(b"eyJ") {
        return None;
    }
    let mut end = at + 3;
    for (part, floor) in FLOORS.iter().enumerate() {
        let run = Run::AlnumDashUnderscore.len(&bytes[end..]);
        if run < *floor {
            return None;
        }
        end += run;
        if part < 2 {
            if bytes.get(end) != Some(&b'.') {
                return None;
            }
            end += 1;
        }
    }
    Some(Found { shape: "JWT", start: at, end })
}

/// `-----BEGIN [words] PRIVATE KEY-----`: the header alone is the shape.
fn pem_at(bytes: &[u8], at: usize) -> Option<Found> {
    const OPEN: &[u8] = b"-----BEGIN ";
    const CLOSE: &[u8] = b"-----";
    const LABEL_LIMIT: usize = 40;
    let label_at = at + OPEN.len();
    if !bytes[at..].starts_with(OPEN) {
        return None;
    }
    let window = &bytes[label_at..bytes.len().min(label_at + LABEL_LIMIT + CLOSE.len())];
    let close = window.windows(CLOSE.len()).position(|w| w == CLOSE)?;
    let label = &window[..close];
    let words_only = label.iter().all(|b| b.is_ascii_uppercase() || *b == b' ');
    let private = label.windows(b"PRIVATE KEY".len()).any(|w| w == b"PRIVATE KEY");
    (words_only && private).then_some(Found { shape: PEM, start: at, end: label_at + close + CLOSE.len() })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fixtures are assembled in code from obviously fake filler, with the
    // prefix split in two, so no secret scanner (and no private-data hook)
    // ever sees a whole token in the source.
    fn fake(prefix: (&str, &str), filler: &str, count: usize) -> String {
        format!("{}{}{}", prefix.0, prefix.1, filler.repeat(count))
    }

    fn shapes_of(text: &str) -> Vec<&'static str> {
        find(text).iter().map(|f| f.shape).collect()
    }

    #[test]
    fn every_listed_shape_is_found() {
        let jwt = format!("{}.{}.{}", fake(("ey", "J"), "h", 15), "i".repeat(15), "j".repeat(8));
        let cases = [
            (fake(("sk_", "live_"), "a1", 6), "Stripe-style key"),
            (fake(("pk_", "test_"), "b2", 6), "Stripe-style key"),
            (fake(("gh", "p_"), "c", 30), "GitHub token"),
            (fake(("gh", "s_"), "d", 30), "GitHub token"),
            (fake(("github_", "pat_"), "E", 30), "GitHub fine-grained token"),
            (fake(("xo", "xb-"), "1", 14), "Slack token"),
            (fake(("AK", "IA"), "Z", 16), "AWS access key id"),
            (fake(("tsk", "ey-"), "f-", 12), "Tailscale key"),
            (fake(("sk-", "ant-"), "g", 30), "Anthropic API key"),
            (fake(("AI", "za"), "H", 36), "Google API key"),
            (jwt, "JWT"),
            ("-----BEGIN RSA PRIVATE KEY-----".to_string(), "PEM private key header"),
            ("-----BEGIN PRIVATE KEY-----".to_string(), "PEM private key header"),
        ];
        for (token, shape) in cases {
            let text = format!("see {token} for the thing");
            assert_eq!(shapes_of(&text), vec![shape], "{shape}");
        }
    }

    #[test]
    fn a_bare_prefix_in_a_sentence_about_secrets_is_not_found() {
        for text in [
            "a GitHub token starts with ghp_ and is 40 characters",
            "Stripe keys start with sk_live_ or sk_test_",
            "an AWS key id starts with AKIA",
            "the header is eyJ and then two more parts",
            "sk-ant- is the Anthropic prefix, tskey- the Tailscale one",
            "the old dev key (tskey-auth-kExample...) expired",
            "github_pat_ tokens are fine-grained",
            "xoxb- is a Slack bot token",
            "AIza keys belong to Google",
            "never pass --password on the command line",
            "primary key: id",
            "commit 4b825dc642cb6eb9a060e54bf8d69288fbee4904 fixed it",
            "set API_TOKEN in the environment, never in the note",
            "a PEM file begins with a BEGIN line, never paste it",
            "-----BEGIN CERTIFICATE-----",
        ] {
            assert!(find(text).is_empty(), "{text}");
        }
    }

    #[test]
    fn a_token_must_start_at_a_word_boundary() {
        let glued = format!("task_live_{}", "a".repeat(20));
        assert!(find(&glued).is_empty());
        let inner = format!("my_{}", fake(("gh", "p_"), "a", 30));
        assert!(find(&inner).is_empty());
        let spaced = format!("({})", fake(("gh", "p_"), "a", 30));
        assert_eq!(find(&spaced).len(), 1);
    }

    #[test]
    fn an_aws_key_id_ends_after_sixteen_characters() {
        let text = format!("AKIA{}tail", "A".repeat(16));
        let hit = find(&text)[0];
        assert_eq!(&text[hit.start..hit.end], format!("AKIA{}", "A".repeat(16)));
    }

    #[test]
    fn two_secrets_are_both_found_in_order() {
        let text = format!("one {} two {}", fake(("gh", "p_"), "a", 25), fake(("sk_", "live_"), "b", 12));
        assert_eq!(shapes_of(&text), vec!["GitHub token", "Stripe-style key"]);
    }

    #[test]
    fn the_hint_is_six_and_two_characters_and_never_the_whole_value() {
        let token = fake(("gh", "p_"), "z", 30);
        let text = format!("x {token} y");
        let hint = find(&text)[0].hint(&text).expect("a token has a hint");
        assert_eq!(hint, "ghp_zz...zz");
        assert!(!hint.contains(&token));
        let pem = "-----BEGIN PRIVATE KEY-----";
        assert_eq!(find(pem)[0].hint(pem), None);
    }

    #[test]
    fn text_with_multibyte_characters_around_a_token_is_scanned_safely() {
        let token = fake(("gh", "p_"), "q", 25);
        let text = format!("caf\u{e9} \u{1f600} {token} \u{e9}");
        let hit = find(&text)[0];
        assert_eq!(&text[hit.start..hit.end], token);
    }
}
