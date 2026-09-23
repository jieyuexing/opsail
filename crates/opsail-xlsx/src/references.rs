//! Token-based A1 relocation. Only row digits are replaced; all other formula
//! bytes (including quoted strings, whitespace and case) are retained.
use super::{Result, invalid};

pub const MAX_ROW: u32 = 1_048_576;
pub struct Shift<'a> {
    pub sheet: &'a str,
    pub sheets: &'a [String],
    pub before: u32,
    pub count: u32,
}
#[derive(Default, Debug)]
pub struct Rewritten {
    pub text: String,
    pub references_target: bool,
    pub touches: bool,
}
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Cell,
    Row,
    Column,
}
struct Endpoint {
    end: usize,
    digits: Option<(usize, usize, u32)>,
    kind: Kind,
}
fn name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '.' | '\\')
}
fn boundary(s: &str, pos: usize) -> bool {
    s[pos..]
        .chars()
        .next()
        .is_none_or(|c| !name_char(c) && c != '$' && c != '[')
}
fn skip_space(s: &str, start: usize) -> usize {
    start + s[start..].len() - s[start..].trim_start_matches(char::is_whitespace).len()
}
fn segment(s: &str, start: usize) -> Option<(usize, String)> {
    if s.as_bytes().get(start) == Some(&b'\'') {
        let mut pos = start + 1;
        let mut name = String::new();
        while pos < s.len() {
            let ch = s[pos..].chars().next()?;
            pos += ch.len_utf8();
            if ch == '\'' {
                if s.as_bytes().get(pos) == Some(&b'\'') {
                    name.push('\'');
                    pos += 1;
                } else {
                    return Some((pos, name));
                }
            } else {
                name.push(ch);
            }
        }
        None
    } else {
        let mut end = start;
        // External workbook qualifiers are consumed as a whole, never treated
        // as same-workbook references, even when their sheet name matches.
        if s.as_bytes().get(end) == Some(&b'[') {
            end += s[end..].find(']')? + 1;
        }
        while let Some(ch) = s[end..].chars().next().filter(|c| name_char(*c)) {
            end += ch.len_utf8();
        }
        (end > start).then(|| (end, s[start..end].into()))
    }
}
fn qualifier(s: &str, start: usize) -> Option<(usize, String)> {
    let (mut end, mut name) = segment(s, start)?;
    if s.as_bytes().get(end) == Some(&b':') {
        let (next, last) = segment(s, end + 1)?;
        name.push(':');
        name.push_str(&last);
        end = next;
    }
    (s.as_bytes().get(end) == Some(&b'!')).then_some((end + 1, name))
}
fn endpoint(s: &str, start: usize) -> Option<Endpoint> {
    let bytes = s.as_bytes();
    let mut p = start;
    if bytes.get(p) == Some(&b'$') {
        p += 1;
    }
    let letters = p;
    while bytes.get(p).is_some_and(u8::is_ascii_alphabetic) {
        p += 1;
    }
    let has_col = p > letters;
    if has_col {
        if p - letters > 3 {
            return None;
        }
        let col = bytes[letters..p].iter().fold(0u32, |n, b| {
            n * 26 + u32::from(b.to_ascii_uppercase() - b'A' + 1)
        });
        if col > 16384 {
            return None;
        }
        if bytes.get(p) == Some(&b'$') {
            p += 1;
        }
    }
    let digits = p;
    while bytes.get(p).is_some_and(u8::is_ascii_digit) {
        p += 1;
    }
    if p > digits {
        let row: u32 = s[digits..p].parse().ok()?;
        if row == 0 || row > MAX_ROW || bytes[digits] == b'0' {
            return None;
        }
        Some(Endpoint {
            end: p,
            digits: Some((digits, p, row)),
            kind: if has_col { Kind::Cell } else { Kind::Row },
        })
    } else if has_col && bytes.get(p.wrapping_sub(1)) != Some(&b'$') {
        Some(Endpoint {
            end: p,
            digits: None,
            kind: Kind::Column,
        })
    } else {
        None
    }
}
impl Shift<'_> {
    pub fn row(&self, row: u32) -> Result<u32> {
        let shifted = if row >= self.before {
            row.checked_add(self.count)
        } else {
            Some(row)
        };
        shifted
            .filter(|r| *r <= MAX_ROW)
            .ok_or_else(|| invalid("shifted row exceeds 1048576; requires native application"))
    }
    pub fn zero_row(&self, row: u32) -> Result<u32> {
        if row >= MAX_ROW {
            return Err(invalid(
                "invalid zero-based row; requires native application",
            ));
        }
        Ok(self.row(row + 1)? - 1)
    }
    fn target(&self, qualifier: &str) -> Result<bool> {
        if qualifier.contains(['[', ']']) {
            return Ok(false);
        }
        if let Some((first, last)) = qualifier.split_once(':') {
            let find = |name: &str| {
                self.sheets
                    .iter()
                    .position(|s| s.eq_ignore_ascii_case(name))
            };
            let spans = match (find(first), find(last), find(self.sheet)) {
                (Some(a), Some(b), Some(t)) => (a.min(b)..=a.max(b)).contains(&t),
                _ => {
                    return Err(invalid(format!(
                        "unresolved 3D reference {qualifier}; requires native application"
                    )));
                }
            };
            if spans {
                return Err(invalid(format!(
                    "3D reference {qualifier} spans {}; requires native application",
                    self.sheet
                )));
            }
            return Ok(false);
        }
        Ok(qualifier.eq_ignore_ascii_case(self.sheet))
    }
    pub fn formula(&self, s: &str, unqualified: bool) -> Result<Rewritten> {
        let mut result = Rewritten::default();
        let mut pos = 0;
        let mut copied = 0;
        while pos < s.len() {
            let ch = s[pos..].chars().next().unwrap();
            if ch == '"' {
                pos += 1;
                while pos < s.len() {
                    if s.as_bytes()[pos] == b'"' {
                        pos += 1;
                        if s.as_bytes().get(pos) != Some(&b'"') {
                            break;
                        }
                    }
                    pos += s[pos..].chars().next().unwrap().len_utf8();
                }
                continue;
            }
            let start_ok = pos == 0
                || s[..pos]
                    .chars()
                    .next_back()
                    .is_none_or(|c| !name_char(c) && !matches!(c, '$' | '!' | '\'' | ']'));
            if start_ok {
                let qualified = qualifier(s, pos);
                let (start, target) = match &qualified {
                    Some((end, name)) => (*end, self.target(name)?),
                    None => (pos, unqualified),
                };
                if let Some(first) = endpoint(s, start) {
                    let mut end = first.end;
                    let mut endpoints = vec![first];
                    let colon = skip_space(s, end);
                    if s.as_bytes().get(colon) == Some(&b':')
                        && let Some(last) = endpoint(s, skip_space(s, colon + 1))
                        && last.kind == endpoints[0].kind
                    {
                        end = last.end;
                        endpoints.push(last);
                    }
                    let valid = (endpoints.len() == 2 || endpoints[0].kind == Kind::Cell)
                        && boundary(s, end)
                        && !s[end..].trim_start().starts_with('(');
                    if valid {
                        if target {
                            result.references_target = true;
                            if endpoints[0].kind == Kind::Column {
                                result.touches = true;
                            }
                            for point in endpoints {
                                if let Some((a, b, row)) = point.digits
                                    && row >= self.before
                                {
                                    result.touches = true;
                                    result.text.push_str(&s[copied..a]);
                                    result.text.push_str(&self.row(row)?.to_string());
                                    copied = b;
                                }
                            }
                        }
                        pos = end;
                        continue;
                    }
                }
                if let Some((end, _)) = qualified {
                    pos = end;
                    continue;
                }
            }
            // Structured references are names, not A1 references. Nested
            // brackets also cover column labels such as Table1[[A7]:[A9]].
            if ch == '[' {
                let mut depth = 1;
                pos += 1;
                while pos < s.len() && depth != 0 {
                    match s.as_bytes()[pos] {
                        b'[' => depth += 1,
                        b']' => depth -= 1,
                        _ => {}
                    }
                    pos += s[pos..].chars().next().unwrap().len_utf8();
                }
            } else {
                pos += ch.len_utf8();
            }
        }
        result.text.push_str(&s[copied..]);
        Ok(result)
    }
}
