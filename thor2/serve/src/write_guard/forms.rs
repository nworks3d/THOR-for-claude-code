//! The small, pure helpers of the write guard: other spellings of one path, shell-style wildcards,
//! flag clusters (`curl -sSLo f`), PowerShell's `-Name:value` parameters and `-EncodedCommand`.
//! None of them knows what is protected.

/// Hosts that mean this very computer in a share path (`\\localhost\c$\x`).
fn is_local_host(host: &str) -> bool {
    let computer = std::env::var("COMPUTERNAME").unwrap_or_default().to_ascii_lowercase();
    matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]" | "." | "localhost.localdomain") || (!computer.is_empty() && host == computer)
}

/// `//localhost/c$/x` (in folded form, anywhere in a text) as `c:/x`: the administrative share of
/// this computer's own drive names the same file.
pub(super) fn unshare(text: &str) -> String {
    if !text.contains("$/") {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("//") {
        out.push_str(&rest[..at]);
        let tail = &rest[at + 2..];
        let host_end = tail.find('/').unwrap_or(tail.len());
        let (host, after) = tail.split_at(host_end);
        let share = after.strip_prefix('/').unwrap_or("");
        let mut share_chars = share.chars();
        let letter = share_chars.next().filter(char::is_ascii_alphabetic);
        let is_drive_share = letter.is_some() && share_chars.next() == Some('$') && matches!(share_chars.next(), None | Some('/'));
        match letter {
            Some(l) if is_local_host(host) && is_drive_share => {
                out.push(l);
                out.push(':');
                rest = &share[2..];
            }
            _ => {
                out.push_str("//");
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

/// One path (already in comparison form) as the file system names it: Windows drops trailing dots
/// and spaces in every part, an alternate data stream (`thor.db::$data`, `thor.db:x`) is the same
/// file, and an 8.3 short name (`PROGRA~1`) is looked up when the folder exists.
pub(super) fn clean_path(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut parts: Vec<String> = Vec::new();
    for (i, seg) in path.split('/').enumerate() {
        if seg.is_empty() || seg == ".." {
            if seg == ".." {
                parts.push(seg.to_string());
            }
            continue;
        }
        let is_drive = i == 0 && seg.len() == 2 && seg.ends_with(':');
        let mut seg = seg;
        if !is_drive {
            if let Some(cut) = seg.find(':') {
                seg = &seg[..cut];
            }
            seg = seg.trim_end_matches(['.', ' ']);
        }
        if !seg.is_empty() {
            parts.push(seg.to_string());
        }
    }
    let joined = parts.join("/");
    let cleaned = if absolute { format!("/{joined}") } else { joined };
    long_form(&cleaned)
}

/// An 8.3 short name (`PROGRA~1`, `USERNA~1`) written out in full, from the folder as it exists.
fn long_form(path: &str) -> String {
    let has_short = path.split('/').any(|s| s.split_once('~').is_some_and(|(_, d)| d.chars().next().is_some_and(|c| c.is_ascii_digit())));
    if !has_short {
        return path.to_string();
    }
    let parts: Vec<&str> = path.split('/').collect();
    for cut in (1..=parts.len()).rev() {
        let head = parts[..cut].join("/");
        let Ok(full) = std::fs::canonicalize(if head.len() == 2 && head.ends_with(':') { format!("{head}/") } else { head }) else {
            continue;
        };
        let mut folded = super::fold(&full.to_string_lossy());
        for rest in &parts[cut..] {
            folded.push('/');
            folded.push_str(rest);
        }
        return folded;
    }
    path.to_string()
}

/// Whether `text` matches a shell wildcard (`*` any run, `?` one character), both in folded form.
pub(super) fn wildcard_matches(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    let (mut pi, mut ti, mut star, mut mark) = (0, 0, None::<usize>, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

pub(super) fn has_wildcard(text: &str) -> bool {
    text.contains('*') || text.contains('?')
}

/// PowerShell's `-Name:value` as the two words `-Name` and `value` (`-Path:C:\x`). A word that
/// is not of that shape stays as it is.
pub(super) fn split_colon_params(words: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(words.len());
    for w in words {
        let named = w.strip_prefix('-').filter(|r| !r.starts_with('-'));
        match named.and_then(|r| r.split_once(':')) {
            // `-oC:/x` (7z, curl) is a flag with a drive path; `-Path:C:/x` is a parameter with a value.
            Some((name, value))
                if name.len() >= 2
                    && name.chars().all(|c| c.is_ascii_alphabetic())
                    && !value.is_empty()
                    && !(name.len() <= 3 && value.starts_with(['/', '\\'])) =>
            {
                out.push(format!("-{name}"));
                out.push(value.to_string());
            }
            _ => out.push(w.clone()),
        }
    }
    out
}

/// The values of the short options of a cluster that take one: `-sSLo f` gives `('o', "f")`,
/// `-qOf` gives `('O', "f")`. `takes` lists the option letters that take a value. The value is
/// the rest of the word, or the next word.
pub(super) fn cluster_values(rest: &[String], takes: &[char]) -> Vec<(char, String)> {
    let mut found = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        let w = &rest[i];
        if w.starts_with('-') && !w.starts_with("--") && w.len() > 1 {
            let letters: Vec<char> = w.chars().skip(1).collect();
            for (j, c) in letters.iter().enumerate() {
                if takes.contains(c) {
                    let attached: String = letters[j + 1..].iter().collect();
                    if !attached.is_empty() {
                        found.push((*c, attached));
                    } else if let Some(next) = rest.get(i + 1) {
                        found.push((*c, next.clone()));
                        i += 1;
                    }
                    break;
                }
            }
        }
        i += 1;
    }
    found
}

/// The program of a `-EncodedCommand` (base64 of UTF-16 little endian text), or `None`.
pub(super) fn decode_encoded_command(payload: &str) -> Option<String> {
    let mut bytes = Vec::new();
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in payload.trim().trim_end_matches('=').chars() {
        let v = match c {
            'A'..='Z' => c as u32 - 'A' as u32,
            'a'..='z' => c as u32 - 'a' as u32 + 26,
            '0'..='9' => c as u32 - '0' as u32 + 52,
            '+' => 62,
            '/' => 63,
            _ => return None,
        };
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    if bytes.len() < 2 {
        return None;
    }
    let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    Some(String::from_utf16_lossy(&units))
}

/// Whether a flag is `-EncodedCommand` or one of its accepted short forms (`-e`, `-ec`, `-enc`...).
pub(super) fn is_encoded_command_flag(flag: &str) -> bool {
    let lower = flag.to_ascii_lowercase();
    lower == "-ec" || (lower.len() >= 2 && lower.starts_with('-') && "-encodedcommand".starts_with(lower.as_str()) && !lower.starts_with("--"))
}
