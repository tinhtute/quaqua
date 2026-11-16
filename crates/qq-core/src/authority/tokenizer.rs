//! Literal port of the `herdr-gate.sh` shell tokenizer (`tok` + `_gw_*`).
//!
//! `tokenize` splits shell source into simple commands; each simple command is
//! the list of its words, with quotes and escapes resolved, command
//! substitutions recursed into, and redirect targets / heredoc bodies dropped.
//! The state machine mirrors the bash original one branch at a time so the two
//! stay diffable.

/// One entry per simple command; each entry holds that command's words.
pub fn tokenize(src: &str) -> Vec<Vec<String>> {
    let chars: Vec<char> = src.chars().collect();
    let mut out: Vec<Vec<String>> = Vec::new();
    tok(&mut out, &chars);
    out
}

/// Per-invocation state (the `tok` locals in bash). `_gw_*` mutate these
/// through dynamic scope in bash; here they are the fields of one frame.
struct Frame {
    word: String,
    have: bool,
    line: Vec<String>,
    quote: char,
    skip: bool,
    want_hdoc: bool,
    hdoc_strip: char,
    hdocs: Vec<(char, String)>,
}

impl Frame {
    fn new() -> Self {
        Frame {
            word: String::new(),
            have: false,
            line: Vec::new(),
            quote: '\0',
            skip: false,
            want_hdoc: false,
            hdoc_strip: 'N',
            hdocs: Vec::new(),
        }
    }
}

/// `_gw_fw`: emit the current word unless it is empty or a redirect target.
fn fw(f: &mut Frame) {
    if f.have {
        if f.want_hdoc {
            f.want_hdoc = false;
            // element = strip-tabs marker (N or T) + delimiter
            f.hdocs.push((f.hdoc_strip, f.word.clone()));
        } else if f.skip {
            f.skip = false;
        } else if !f.word.is_empty() {
            // Bash joins the words with TAB and re-splits with
            // `IFS=$'\t' read -r -a`, so empty words vanish. An empty word is
            // never the executable — dropping it here keeps the two in parity
            // (`$(true) herdr pane split` must still resolve `herdr`).
            f.line.push(f.word.clone());
        }
    }
    f.word.clear();
    f.have = false;
}

/// `_gw_fs`: end the current simple command.
fn fs(f: &mut Frame, out: &mut Vec<Vec<String>>) {
    fw(f);
    if !f.line.is_empty() {
        out.push(std::mem::take(&mut f.line));
    }
    f.line.clear();
    f.skip = false;
    f.want_hdoc = false;
}

/// `_gw_redir`: `s[i]` is the first `<` or `>` of a redirect operator; consume
/// the operator and leave `i` on its last char. Sets `skip` (drop the next
/// word — the detached or attached target) or `want_hdoc`.
fn redir(f: &mut Frame, s: &[char], i: &mut usize) {
    let n = s.len();
    let mut lts = 0usize;
    if s[*i] == '<' {
        lts = 1; // s[i] itself is the first op char
    }
    while *i + 1 < n && (s[*i + 1] == '>' || s[*i + 1] == '<') {
        if s[*i + 1] == '<' {
            lts += 1;
        }
        *i += 1;
    }
    if *i + 1 < n && s[*i + 1] == '&' {
        // >&fd / <&fd — the target is attached to the operator; nothing to skip
        *i += 1;
        while *i + 1 < n && is_fd_char(s[*i + 1]) {
            *i += 1;
        }
        return;
    }
    if lts == 2 {
        // <<DELIM / <<-DELIM heredoc: the next word is the delimiter, the body
        // (skipped at the next newline) is literal text, not commands
        f.want_hdoc = true;
        f.hdoc_strip = 'N';
        if *i + 1 < n && s[*i + 1] == '-' {
            *i += 1;
            f.hdoc_strip = 'T';
        }
        return;
    }
    f.skip = true;
}

fn is_fd_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '~' | '-')
}

/// `_gw_subst`: `s[i]` is `` ` `` or `$` with `s[i+1] == '('`. Returns the inner
/// text and the index just past the closer.
fn subst(s: &[char], i: usize) -> (String, usize) {
    let n = s.len();
    let mut inner = String::new();
    if s[i] == '`' {
        let mut j = i + 1;
        while j < n && s[j] != '`' {
            inner.push(s[j]);
            j += 1;
        }
        let mut se = j;
        if se < n {
            se += 1;
        }
        return (inner, se);
    }
    // s[i] == '$', s[i+1] == '('
    let mut j = i + 2;
    let mut q: char = '\0';
    let mut depth = 1usize;
    while j < n {
        let c = s[j];
        if q == '\'' {
            if c == '\'' {
                q = '\0';
            }
            inner.push(c);
            j += 1;
            continue;
        }
        if q == '"' {
            match c {
                '"' => q = '\0',
                '\\' => {
                    inner.push(c);
                    j += 1;
                    if j < n {
                        inner.push(s[j]);
                    }
                }
                _ => {}
            }
            if c != '\\' {
                inner.push(c);
            }
            j += 1;
            continue;
        }
        match c {
            '\'' => q = '\'',
            '"' => q = '"',
            '\\' => {
                inner.push(c);
                j += 1;
                if j < n {
                    inner.push(s[j]);
                }
                j += 1;
                continue;
            }
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    j += 1;
                    break;
                }
            }
            _ => {}
        }
        inner.push(c);
        j += 1;
    }
    (inner, j)
}

/// Heredoc bodies: literal lines until each delimiter line (tab-stripped for
/// `<<-`). Called on a newline separator with pending heredocs.
fn drain_hdocs(f: &mut Frame, s: &[char], i: &mut usize) {
    let n = s.len();
    while !f.hdocs.is_empty() && *i + 1 < n {
        let (strip, delim) = f.hdocs.remove(0);
        while *i + 1 < n {
            *i += 1;
            let mut ln = String::new();
            while *i < n && s[*i] != '\n' {
                ln.push(s[*i]);
                *i += 1;
            }
            let lns = if strip == 'T' {
                ln.trim_start_matches('\t').to_string()
            } else {
                ln
            };
            if lns == delim {
                break;
            }
        }
    }
    f.hdocs.clear();
}

fn recurse(out: &mut Vec<Vec<String>>, inner: &str) {
    if inner.is_empty() {
        return;
    }
    let chars: Vec<char> = inner.chars().collect();
    tok(out, &chars);
}

/// `tok <text>`: append one element per simple command to `out`.
fn tok(out: &mut Vec<Vec<String>>, s: &[char]) {
    let n = s.len();
    let mut f = Frame::new();
    let mut i = 0usize;
    while i < n {
        let c = s[i];
        if f.quote == '\'' {
            if c == '\'' {
                f.quote = '\0';
            } else {
                f.word.push(c);
            }
            i += 1;
            continue;
        }
        if f.quote == '"' {
            match c {
                '"' => f.quote = '\0',
                '\\' => {
                    i += 1;
                    if i < n && s[i] != '\n' {
                        f.word.push(s[i]);
                    }
                }
                '`' => {
                    let (inner, se) = subst(s, i);
                    i = se;
                    recurse(out, &inner);
                    continue;
                }
                '$' => {
                    if i + 1 < n && s[i + 1] == '(' {
                        let (inner, se) = subst(s, i);
                        i = se;
                        recurse(out, &inner);
                        continue;
                    }
                    f.word.push(c);
                }
                _ => f.word.push(c),
            }
            i += 1;
            continue;
        }
        match c {
            '\'' => {
                f.quote = '\'';
                f.have = true;
            }
            '"' => {
                f.quote = '"';
                f.have = true;
            }
            '\\' => {
                i += 1;
                if i < n && s[i] != '\n' {
                    f.word.push(s[i]);
                    f.have = true;
                }
            }
            '`' => {
                let (inner, se) = subst(s, i);
                i = se;
                recurse(out, &inner);
                f.have = true;
                continue;
            }
            '$' => {
                if i + 1 < n && s[i + 1] == '(' {
                    let (inner, se) = subst(s, i);
                    i = se;
                    recurse(out, &inner);
                    f.have = true;
                    continue;
                }
                f.word.push(c);
                f.have = true;
            }
            ';' | '|' | '(' | ')' | '\n' => {
                fs(&mut f, out);
                if c == '\n' && !f.hdocs.is_empty() {
                    drain_hdocs(&mut f, s, &mut i);
                }
            }
            '&' => {
                if i + 1 < n && s[i + 1] == '>' {
                    fw(&mut f);
                    i += 1;
                    redir(&mut f, s, &mut i);
                } else {
                    fs(&mut f, out);
                }
            }
            '>' | '<' => {
                // a bare fd number before the operator belongs to it (2>file)
                if f.have && !f.word.is_empty() && f.word.chars().all(|x| x.is_ascii_digit()) {
                    f.word.clear();
                    f.have = false;
                } else {
                    fw(&mut f);
                }
                redir(&mut f, s, &mut i);
            }
            '#' => {
                if !f.have {
                    while i < n && s[i] != '\n' {
                        i += 1;
                    }
                    continue;
                }
                f.word.push(c);
            }
            ' ' | '\t' => fw(&mut f),
            _ => {
                f.word.push(c);
                f.have = true;
            }
        }
        i += 1;
    }
    fs(&mut f, out);
}
