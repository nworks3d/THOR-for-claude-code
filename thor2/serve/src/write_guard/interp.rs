//! A one-line program in an interpreter (`python -c`, `node -e`, `perl -e`, `ruby -e`, `php -r`,
//! `lua -e`, `awk`) that WRITES to a protected path.
//!
//! Only a write whose TARGET is a protected path counts: `open(<protected>, 'w')`,
//! `Path(<protected>).write_text(...)`, `os.remove(<protected>)`, `shutil.copy(x, <protected>)`,
//! `fs.writeFileSync(<protected>, ...)`, a sqlite connection to the memory that is not read-only
//! next to SQL that changes data, an awk redirect into it, `perl -pi` over it, a shell command
//! inside a string (`os.system("echo x > <protected>")`). A word like `append`, `remove` or
//! `write` somewhere else in the program, a read of a protected file, and a write of what was read
//! to some other path are free: `-I` is Python's isolated flag, `list.append` is not a file.
//!
//! The program text is read as text, not run. A path built at run time (`sys.argv`, an
//! environment variable, a join of parts that are not written out), a variable set in an earlier
//! step, `eval` and a script file are not seen: this guard does not claim to see them.

use super::{command_writes_protected, Protected};

/// Whether the program `source` (the whole command text it came in) writes a protected path.
pub(super) fn writes_protected(p: &Protected, program: &str, source: &str, cwd: Option<&str>, depth: usize) -> bool {
    let text = source.replace("\\\"", "\"");
    let vars = path_variables(&text);
    let mut calls = calls_of(&text);
    calls.extend(bare_calls_of(&text));
    for call in &calls {
        if call_writes_protected(p, call, &vars, cwd) {
            return true;
        }
    }
    if depth < 3 && shell_strings_write_protected(p, &calls, cwd, depth) {
        return true;
    }
    if matches!(program, "awk" | "gawk") && awk_redirects_into_protected(p, &text, cwd) {
        return true;
    }
    // `print "x" | "tee <file>"`: a command in a string of an awk program.
    if depth < 3 && matches!(program, "awk" | "gawk") {
        let reads_a_command = |l: &String| l.contains(' ') && (p.mentions_any(l) || p.mentions_a_locked_file(l));
        if nested_literals(&text, 3).iter().any(|l| reads_a_command(l) && command_writes_protected(p, l, cwd, depth + 1)) {
            return true;
        }
    }
    if is_sql_write_on_protected(p, &calls, &text, &vars, cwd) {
        return true;
    }
    false
}

/// A call `name(args)` in the text, with the text before its name (back to the start of the
/// statement) as the receiver of a method call.
struct Call {
    /// The dotted name in lower case: `os.remove`, `path.write_text`, `fs.writefilesync`.
    name: String,
    args: Vec<String>,
    /// The text of the statement before the name.
    before: String,
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '.' | '$' | ':')
}

fn calls_of(text: &str) -> Vec<Call> {
    let chars: Vec<char> = text.chars().collect();
    let mut calls = Vec::new();
    for (i, &c) in chars.iter().enumerate() {
        if c != '(' {
            continue;
        }
        let mut start = i;
        while start > 0 && is_name_char(chars[start - 1]) {
            start -= 1;
        }
        let name: String = chars[start..i].iter().collect::<String>().to_lowercase();
        let name = name.trim_matches(|c| c == '.' || c == ':' || c == '$').to_string();
        if name.is_empty() {
            continue;
        }
        let mut stmt = start;
        while stmt > 0 && !matches!(chars[stmt - 1], '\n' | ';') {
            stmt -= 1;
        }
        calls.push(Call { name, args: split_args(&chars, i + 1), before: chars[stmt..start].iter().collect() });
    }
    calls
}

/// Perl's calls without parentheses: `unlink "file"`, `open F, ">file"`, `rename "a", "b"`. The
/// arguments run to the end of the statement.
fn bare_calls_of(text: &str) -> Vec<Call> {
    let mut calls = Vec::new();
    for name in ["unlink", "open", "rename", "rmdir", "truncate"] {
        let mut from = 0;
        while let Some(at) = text[from..].find(name) {
            let start = from + at;
            let end = start + name.len();
            from = end;
            let before_ok = text[..start].chars().next_back().is_none_or(|c| !is_name_char(c));
            let rest = &text[end..];
            // The argument opens with a quote, a variable, or a bareword handle (`open F, ">file"`).
            let after = rest.trim_start();
            let handle: String = after.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            let spaced = rest.starts_with(' ')
                && (after.starts_with(['"', '\'', '$']) || (!handle.is_empty() && after[handle.len()..].trim_start().starts_with(',')));
            if !before_ok || !spaced {
                continue;
            }
            let statement = rest.split([';', '\n']).next().unwrap_or("");
            let chars: Vec<char> = format!("{statement})").chars().collect();
            calls.push(Call { name: name.to_string(), args: split_args(&chars, 0), before: String::new() });
        }
    }
    calls
}

/// The top-level, comma-separated arguments from `from` to the matching `)`.
fn split_args(chars: &[char], from: usize) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let (mut depth, mut quote) = (0usize, None::<char>);
    let mut i = from;
    while i < chars.len() {
        let c = chars[i];
        if let Some(q) = quote {
            current.push(c);
            if c == '\\' && i + 1 < chars.len() {
                current.push(chars[i + 1]);
                i += 1;
            } else if c == q {
                quote = None;
            }
        } else {
            match c {
                '\'' | '"' | '`' => {
                    quote = Some(c);
                    current.push(c);
                }
                '(' | '[' | '{' => {
                    depth += 1;
                    current.push(c);
                }
                ')' | ']' | '}' => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                    current.push(c);
                }
                ',' if depth == 0 => args.push(std::mem::take(&mut current)),
                _ => current.push(c),
            }
        }
        i += 1;
    }
    if !current.trim().is_empty() {
        args.push(current);
    }
    args
}

/// The string literals of a text and, inside them, the literals of those (an awk program in
/// single quotes holds the double-quoted command it pipes to).
fn nested_literals(text: &str, levels: usize) -> Vec<String> {
    let mut out = literals(text);
    if levels > 0 {
        let inner: Vec<String> = out.iter().filter(|l| l.contains('"') || l.contains('\'')).flat_map(|l| nested_literals(l, levels - 1)).collect();
        out.extend(inner);
    }
    out
}

/// The string literals in a text, without their quotes.
fn literals(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if matches!(chars[i], '\'' | '"' | '`') {
            let q = chars[i];
            let mut lit = String::new();
            i += 1;
            while i < chars.len() && chars[i] != q {
                if chars[i] == '\\' && chars.get(i + 1) == Some(&q) {
                    i += 1;
                }
                lit.push(chars[i]);
                i += 1;
            }
            out.push(lit);
        }
        i += 1;
    }
    out
}

/// Names a program assigns a path-like string to (`p = r'C:/x/y.jsonl'`, `$p = '...'`,
/// `const p = "..."`): name -> the literals of what it is set to.
fn path_variables(text: &str) -> Vec<(String, Vec<String>)> {
    let chars: Vec<char> = text.chars().collect();
    let mut vars = Vec::new();
    for (i, &c) in chars.iter().enumerate() {
        if c != '=' || chars.get(i + 1) == Some(&'=') || (i > 0 && matches!(chars[i - 1], '=' | '!' | '<' | '>' | '+' | '-')) {
            continue;
        }
        let mut end = i;
        while end > 0 && chars[end - 1] == ' ' {
            end -= 1;
        }
        let mut start = end;
        while start > 0 && (chars[start - 1].is_alphanumeric() || chars[start - 1] == '_') {
            start -= 1;
        }
        if start == end {
            continue;
        }
        let name: String = chars[start..end].iter().collect::<String>().to_lowercase();
        let mut rhs = String::new();
        let (mut depth, mut quote) = (0usize, None::<char>);
        for &r in &chars[i + 1..] {
            match quote {
                Some(q) => {
                    rhs.push(r);
                    if r == q {
                        quote = None;
                    }
                }
                None => match r {
                    '\'' | '"' | '`' => {
                        quote = Some(r);
                        rhs.push(r);
                    }
                    '(' | '[' | '{' => {
                        depth += 1;
                        rhs.push(r);
                    }
                    ')' | ']' | '}' => {
                        depth = depth.saturating_sub(1);
                        rhs.push(r);
                    }
                    '\n' | ';' if depth == 0 => break,
                    _ => rhs.push(r),
                },
            }
        }
        let lits = literals(&rhs);
        if lits.iter().any(|l| l.contains('/') || l.contains('\\')) {
            vars.push((name, lits));
        }
    }
    vars
}

/// Every path an argument may stand for: each literal in it, the literals joined (as
/// `os.path.join` and `a + '/' + b` do), and what a variable it names was set to, alone or
/// followed by the literals of the argument.
fn arg_paths(arg: &str, vars: &[(String, Vec<String>)]) -> Vec<String> {
    let own = literals(arg);
    let mut out = own.clone();
    // Perl's two-argument open carries the mode in front of the path: `">>file"`.
    out.extend(own.iter().map(|l| l.trim_start_matches(['>', '<', '+', '&', ' ']).to_string()).filter(|l| !own.contains(l)));
    if own.len() > 1 {
        out.push(own.join("/"));
    }
    let lower = arg.to_lowercase();
    let trimmed = lower.trim().trim_start_matches(['$', '@']);
    for (name, lits) in vars {
        let bare = trimmed == name;
        let wrapped = ["path(", "str(", "join(", "fspath("].iter().any(|w| lower.contains(&format!("{w}{name}")) || lower.contains(&format!("{w}${name}")));
        let joined = trimmed.starts_with(name.as_str()) && trimmed[name.len()..].trim_start().starts_with(['+', '/']);
        if bare || wrapped || joined {
            out.extend(lits.iter().cloned());
            let mut combo = lits.clone();
            combo.extend(own.iter().cloned());
            out.push(combo.join("/"));
        }
    }
    out
}

fn hits(p: &Protected, paths: &[String], cwd: Option<&str>, removal: bool) -> bool {
    paths.iter().any(|path| if removal { p.removal_protects(path, cwd) } else { p.protects(path, cwd) })
}

/// A mode of `open`: it creates, truncates, appends or updates.
fn is_write_mode(arg: &str) -> bool {
    let trimmed = arg.trim();
    let value = trimmed.strip_prefix("mode").map(|r| r.trim_start().trim_start_matches(['=', ':']).trim()).unwrap_or(trimmed);
    let lit = value.trim_matches(|c| c == '\'' || c == '"' || c == '`').trim();
    if lit.is_empty() || lit.len() > 4 {
        return false;
    }
    let ok = lit.chars().all(|c| "rwxab+tUe".contains(c));
    (ok && lit.chars().any(|c| matches!(c, 'w' | 'a' | 'x' | '+'))) || lit.starts_with('>') || lit.starts_with("+<")
}

fn call_writes_protected(p: &Protected, call: &Call, vars: &[(String, Vec<String>)], cwd: Option<&str>) -> bool {
    let last = call.name.rsplit(['.', ':']).next().unwrap_or(&call.name);
    let prefix = call.name.strip_suffix(last).unwrap_or("").trim_end_matches(['.', ':']);
    let module = matches!(prefix, "os" | "fs" | "fs.promises" | "shutil" | "file" | "fileutils" | "io" | "pathlib" | "fs_extra" | "fse" | "fsp")
        || [".file", ".directory"].iter().any(|s| prefix.ends_with(s));
    let writes_a_stream = call.args.iter().any(|a| {
        let l = a.to_lowercase();
        ["filemode.create", "filemode.append", "filemode.truncate", "filemode.openorcreate", "fileaccess.write", "fileaccess.readwrite"].iter().any(|m| l.contains(m))
    });
    let first = call.args.first().map(|a| arg_paths(a, vars)).unwrap_or_default();
    let named = |key: &str| call.args.iter().find_map(|a| a.trim_start().strip_prefix(key).map(|r| r.trim_start().trim_start_matches('=').to_string()));
    let first = if first.is_empty() { named("file").or_else(|| named("path")).map(|a| arg_paths(&a, vars)).unwrap_or_default() } else { first };
    let all: Vec<Vec<String>> = call.args.iter().map(|a| arg_paths(a, vars)).collect();
    let n = call.args.len();
    let direct = match last {
        "writefile" | "writefilesync" | "appendfile" | "appendfilesync" | "createwritestream" | "file_put_contents" | "truncate"
        | "truncatesync" | "touch" => {
            hits(p, &first, cwd, false)
        }
        "unlink" | "unlinksync" | "rmtree" | "rmsync" | "rmdirsync" | "removedirs" | "rm_rf" | "rm_r" | "rm_f" => hits(p, &first, cwd, true),
        "rmdir" => hits(p, &first, cwd, true),
        // .NET: File.WriteAllText, File.Create, new StreamWriter(path), new FileStream(path, FileMode.Create)
        "writealltext" | "writeallbytes" | "writealllines" | "appendalltext" | "appendalllines" | "appendtext" | "createtext" | "openwrite"
        | "streamwriter" => hits(p, &first, cwd, false),
        "create" if module => hits(p, &first, cwd, false),
        "filestream" if writes_a_stream => hits(p, &first, cwd, false),
        "remove" | "delete" | "rm" if module => hits(p, &first, cwd, true),
        "write" | "binwrite" if matches!(call.name.as_str(), "file.write" | "io.write" | "file.binwrite" | "io.binwrite") => hits(p, &first, cwd, false),
        "rename" | "renamesync" | "move" | "mv" if n >= 2 => all.iter().take(2).any(|paths| hits(p, paths, cwd, true)),
        "replace" if call.name == "os.replace" && n >= 2 => all.iter().take(2).any(|paths| hits(p, paths, cwd, true)),
        "copyfile" | "copyfilesync" | "copy2" | "copytree" | "cp" | "cpsync" | "copy_file" | "cp_r" if n >= 2 => hits(p, &all[1], cwd, false),
        "copy" if (module || prefix.is_empty()) && n >= 2 => hits(p, &all[1], cwd, false),
        "open" | "fopen" | "opensync" => {
            if n < 2 {
                return false;
            }
            let flags = ["o_wronly", "o_rdwr", "o_append", "o_creat", "o_trunc"];
            let perl_style = call.args.iter().skip(1).any(|a| a.trim().trim_matches(|c| c == '\'' || c == '"').starts_with(['>', '+']));
            let writes = perl_style || call.args.iter().skip(1).any(|a| is_write_mode(a) || flags.iter().any(|f| a.to_lowercase().contains(f)));
            writes && (hits(p, &first, cwd, false) || (perl_style && all.iter().skip(1).any(|paths| hits(p, paths, cwd, false))))
        }
        _ => false,
    };
    direct || receiver_writes(p, call, last, vars, cwd)
}

/// `Path(<protected>).write_text(...)`, `p.unlink()`: the target is what the method is called on.
fn receiver_writes(p: &Protected, call: &Call, last: &str, vars: &[(String, Vec<String>)], cwd: Option<&str>) -> bool {
    let removal = match last {
        "write_text" | "write_bytes" | "touch" | "truncate" => false,
        "unlink" | "rmdir" | "rename" => true,
        _ => return false,
    };
    if !call.name.contains('.') && !call.before.contains('(') {
        return false;
    }
    let mut receiver = immediate_receiver(&call.before);
    // `p.unlink()`: the receiver is the name before the dot.
    if let Some(stem) = call.name.strip_suffix(last).map(|s| s.trim_end_matches('.')).filter(|s| !s.is_empty()) {
        receiver.push_str(stem);
    }
    let mut paths = arg_paths(&receiver, vars);
    let lower = receiver.to_lowercase();
    let tail: String = lower.chars().rev().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$').collect::<String>().chars().rev().collect();
    for (name, lits) in vars {
        if tail.trim_start_matches('$') == name || lower.contains(&format!("path({name}")) || lower.contains(&format!("path(${name}")) {
            paths.extend(lits.iter().cloned());
        }
    }
    hits(p, &paths, cwd, removal)
}

/// The expression a method is called on: the names and the bracketed groups right before the dot.
fn immediate_receiver(before: &str) -> String {
    let chars: Vec<char> = before.chars().collect();
    let mut i = chars.len();
    loop {
        if i > 0 && is_name_char(chars[i - 1]) {
            i -= 1;
        } else if i > 0 && matches!(chars[i - 1], ')' | ']') {
            let mut depth = 0usize;
            while i > 0 {
                i -= 1;
                match chars[i] {
                    ')' | ']' => depth += 1,
                    '(' | '[' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
            }
        } else {
            break;
        }
    }
    chars[i..].iter().collect()
}

/// A shell command written out in a string and run (`os.system("echo x > <protected>")`,
/// `subprocess.run(["cp", "x", "<protected>"])`): read it like a command line.
fn shell_strings_write_protected(p: &Protected, calls: &[Call], cwd: Option<&str>, depth: usize) -> bool {
    const RUNNERS: &[&str] = &["system", "popen", "run", "call", "check_call", "check_output", "spawn", "spawnsync", "exec", "execsync", "execfile", "execfilesync", "shell_exec", "passthru"];
    for call in calls {
        let last = call.name.rsplit(['.', ':']).next().unwrap_or(&call.name);
        if !RUNNERS.contains(&last) {
            continue;
        }
        let args = call.args.join(" ");
        let lits = literals(&args);
        let joined = lits.join(" ");
        if lits.len() > 1 && (p.mentions_any(&joined) || p.mentions_a_locked_file(&joined)) && command_writes_protected(p, &joined, cwd, depth + 1) {
            return true;
        }
        if lits.iter().any(|l| l.contains(' ') && (p.mentions_any(l) || p.mentions_a_locked_file(l)) && command_writes_protected(p, l, cwd, depth + 1)) {
            return true;
        }
    }
    false
}

/// `print "x" > "<protected>"`, `printf ... >> file`: the word after a `>` of an awk program.
fn awk_redirects_into_protected(p: &Protected, text: &str, cwd: Option<&str>) -> bool {
    let chars: Vec<char> = text.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c != '>' {
            continue;
        }
        let mut j = i + 1;
        while j < chars.len() && (chars[j] == '>' || chars[j] == ' ') {
            j += 1;
        }
        let rest: String = chars[j..].iter().collect();
        let target = match rest.chars().next() {
            Some(q) if matches!(q, '"' | '\'') => rest[1..].split(q).next().unwrap_or("").to_string(),
            _ => rest.split(|c: char| c.is_whitespace() || matches!(c, ';' | '}' | ')')).next().unwrap_or("").to_string(),
        };
        if !target.is_empty() && p.protects(&target, cwd) {
            return true;
        }
    }
    false
}

/// A sqlite connection to a protected database that is not read-only, with SQL that changes data.
fn is_sql_write_on_protected(p: &Protected, calls: &[Call], text: &str, vars: &[(String, Vec<String>)], cwd: Option<&str>) -> bool {
    let lower = text.to_lowercase();
    if !super::SQL_WRITES.iter().any(|m| lower.contains(m)) {
        return false;
    }
    calls.iter().any(|call| {
        let last = call.name.rsplit(['.', ':']).next().unwrap_or(&call.name);
        if !matches!(last, "connect" | "open" | "database" | "databasesync") || call.name.starts_with("os.") {
            return false;
        }
        let Some(first) = call.args.first() else { return false };
        let lit = first.to_lowercase();
        if lit.contains("mode=ro") || lit.contains("immutable=1") {
            return false;
        }
        let paths: Vec<String> = arg_paths(first, vars)
            .into_iter()
            .map(|s| s.to_lowercase().trim_start_matches("file:").split('?').next().unwrap_or("").to_string())
            .collect();
        hits(p, &paths, cwd, false)
    })
}
