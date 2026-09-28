//! Opt-in stored-semantics comparison. The legacy diff remains independent.
use super::*;

const NORMALIZATION: &str = "Numbers use shortest f64 values; strings use visible text and merged effective runs (rPr is self-contained); font charset/family, style indexes, xfId/apply flags, row spans, column customWidth/bestFit and dimensions are ignored; RGB is uppercase ARGB, theme/indexed/auto remain distinct; views are separate. Unknown cell/style metadata is retained conservatively; no rendering or recalculation.";

fn attr<'a>(e: Option<&'a Element>, key: &str) -> Option<&'a str> {
    e.and_then(|e| e.attrs.get(key)).map(String::as_str)
}
fn flag(e: Option<&Element>, key: &str, default: bool) -> bool {
    attr(e, key).map_or(default, |s| s != "0" && s != "false")
}
fn numeric(s: &str) -> String {
    s.parse::<f64>()
        .ok()
        .filter(|n| n.is_finite())
        .map_or_else(|| s.to_owned(), |n| n.to_string())
}
fn number(e: Option<&Element>, key: &str) -> Value {
    attr(e, key).map_or(Value::Null, |s| {
        s.parse::<f64>()
            .ok()
            .filter(|n| n.is_finite())
            .map_or_else(|| json!(s), |n| json!(n))
    })
}
fn color(e: Option<&Element>) -> Value {
    let Some(e) = e else { return Value::Null };
    let mut v = json!({});
    if let Some(rgb) = e.attrs.get("rgb") {
        v["rgb"] = json!(if rgb.len() == 6 {
            format!("FF{}", rgb.to_ascii_uppercase())
        } else {
            rgb.to_ascii_uppercase()
        });
    }
    for key in ["theme", "indexed", "tint"] {
        if e.attrs.contains_key(key) && !(key == "tint" && number(Some(e), key) == json!(0.0)) {
            v[key] = number(Some(e), key);
        }
    }
    if flag(Some(e), "auto", false) {
        v["auto"] = json!(true);
    }
    extras(
        &mut v,
        Some(e),
        &["rgb", "theme", "indexed", "tint", "auto"],
        &[],
    );
    if v == json!({}) { Value::Null } else { v }
}
/// Keep fields outside the documented normalization instead of silently dropping edits.
fn extras(v: &mut Value, e: Option<&Element>, attrs: &[&str], children: &[&str]) {
    let Some(e) = e else { return };
    let a: BTreeMap<_, _> = e
        .attrs
        .iter()
        .filter(|(k, _)| !k.starts_with("xmlns") && !attrs.contains(&k.as_str()))
        .collect();
    let c: Vec<_> = e
        .elements()
        .filter(|c| !children.contains(&c.local_name()))
        .map(styles::canonical)
        .collect();
    if !a.is_empty() {
        v["attributes"] = json!(a);
    }
    if !c.is_empty() {
        v["extensions"] = json!(c);
    }
}
fn font(e: &Element) -> Value {
    let mut v = json!({});
    for (xml, key) in [
        ("name", "name"),
        ("rFont", "name"),
        ("sz", "size"),
        ("u", "underline"),
        ("vertAlign", "vertAlign"),
        ("scheme", "scheme"),
    ] {
        if let Some(child) = e.child(xml) {
            let val = attr(Some(child), "val").unwrap_or(if xml == "u" { "single" } else { "" });
            if val.is_empty()
                || (xml == "u" && val == "none")
                || (xml == "vertAlign" && val == "baseline")
                || (xml == "scheme" && val == "none")
            {
                continue;
            }
            v[key] = if xml == "sz" {
                number(Some(child), "val")
            } else {
                json!(val)
            };
        }
    }
    for (xml, key) in [
        ("b", "bold"),
        ("i", "italic"),
        ("strike", "strike"),
        ("outline", "outline"),
        ("shadow", "shadow"),
        ("condense", "condense"),
        ("extend", "extend"),
    ] {
        if e.child(xml).is_some_and(|e| flag(Some(e), "val", true)) {
            v[key] = json!(true);
        }
    }
    let c = color(e.child("color"));
    if !c.is_null() {
        v["color"] = c;
    }
    extras(
        &mut v,
        Some(e),
        &[],
        &[
            "name",
            "rFont",
            "sz",
            "b",
            "i",
            "strike",
            "u",
            "color",
            "vertAlign",
            "scheme",
            "charset",
            "family",
            "outline",
            "shadow",
            "condense",
            "extend",
        ],
    );
    v
}
fn record<'a>(root: &'a Element, collection: &str, id: usize) -> Result<&'a Element> {
    root.child(collection)
        .and_then(|c| c.elements().nth(id))
        .ok_or_else(|| invalid(format!("{collection} index {id} missing")))
}
fn fill(e: &Element) -> Value {
    if let Some(g) = e.child("gradientFill") {
        return json!({"gradient":styles::canonical(g)});
    }
    let p = e.child("patternFill");
    let kind = attr(p, "patternType").unwrap_or("none");
    let mut v = json!({});
    if kind != "none" {
        v["patternType"] = json!(kind);
        v["color"] = color(p.and_then(|p| p.child("fgColor")));
        if kind != "solid" {
            v["background"] = color(p.and_then(|p| p.child("bgColor")));
        }
    }
    extras(&mut v, Some(e), &[], &["patternFill"]);
    v
}
fn border(e: &Element) -> Value {
    let mut v = json!({});
    for side in [
        "left",
        "right",
        "top",
        "bottom",
        "diagonal",
        "vertical",
        "horizontal",
        "start",
        "end",
    ] {
        if let Some(edge) = e.child(side) {
            let style = attr(Some(edge), "style").unwrap_or("none");
            if style != "none" {
                v[side] = json!({"style":style,"color":color(edge.child("color"))});
            }
        }
    }
    for key in ["diagonalUp", "diagonalDown"] {
        if flag(Some(e), key, false) {
            v[key] = json!(true);
        }
    }
    if !flag(Some(e), "outline", true) {
        v["outline"] = json!(false);
    }
    extras(
        &mut v,
        Some(e),
        &["diagonalUp", "diagonalDown", "outline"],
        &[
            "left",
            "right",
            "top",
            "bottom",
            "diagonal",
            "vertical",
            "horizontal",
            "start",
            "end",
        ],
    );
    v
}
fn alignment(e: Option<&Element>) -> Value {
    let mut v = json!({"horizontal":attr(e,"horizontal").unwrap_or("general"),"vertical":attr(e,"vertical").unwrap_or("bottom")});
    for key in ["wrapText", "shrinkToFit", "justifyLastLine"] {
        v[key] = json!(flag(e, key, false));
    }
    for key in ["indent", "textRotation", "readingOrder", "relativeIndent"] {
        v[key] = attr(e, key).map_or(json!(0.0), |_| number(e, key));
    }
    extras(
        &mut v,
        e,
        &[
            "horizontal",
            "vertical",
            "wrapText",
            "shrinkToFit",
            "justifyLastLine",
            "indent",
            "textRotation",
            "readingOrder",
            "relativeIndent",
        ],
        &[],
    );
    v
}
fn normalized_style(book: &Book, id: usize) -> Result<Value> {
    let root = book.styles.root().map_err(invalid)?;
    let xf = record(root, "cellXfs", id)?;
    let component =
        |collection: &str, key: &str| record(root, collection, attr_u32(xf, key, 0)? as usize);
    let p = xf.child("protection");
    let mut protection = json!({"locked":flag(p,"locked",true),"hidden":flag(p,"hidden",false)});
    extras(&mut protection, p, &["locked", "hidden"], &[]);
    let mut v = json!({"font":font(component("fonts","fontId")?),"fill":fill(component("fills","fillId")?),"border":border(component("borders","borderId")?),"alignment":alignment(xf.child("alignment")),"numberFormat":styles::numfmt(root,attr_u32(xf,"numFmtId",0)? as usize),"protection":protection,"quotePrefix":flag(Some(xf),"quotePrefix",false)});
    let ignored: Vec<_> = xf
        .attrs
        .keys()
        .filter(|k| k.starts_with("apply"))
        .map(String::as_str)
        .chain([
            "fontId",
            "fillId",
            "borderId",
            "numFmtId",
            "xfId",
            "quotePrefix",
        ])
        .collect();
    extras(&mut v, Some(xf), &ignored, &["alignment", "protection"]);
    Ok(v)
}
struct View<'a> {
    book: &'a Book,
    styles: BTreeMap<usize, Value>,
    raw_styles: BTreeMap<usize, Value>,
}
impl<'a> View<'a> {
    fn new(book: &'a Book) -> Self {
        Self {
            book,
            styles: BTreeMap::new(),
            raw_styles: BTreeMap::new(),
        }
    }
    fn style(&mut self, id: usize) -> Result<Value> {
        if !self.styles.contains_key(&id) {
            self.styles.insert(id, normalized_style(self.book, id)?);
        }
        Ok(self.styles[&id].clone())
    }
    fn cell(&mut self, sheet: Option<&Sheet>, cell: &str) -> Result<Value> {
        let c = sheet.map(|s| s.cell(cell)).transpose()?.flatten();
        let id = sheet.map(|s| s.style_id(cell)).transpose()?.unwrap_or(0);
        let style = self.style(id)?;
        let mut v = content(self.book, c)?;
        v["runs"] = Value::Null;
        if v["kind"] == "string"
            && let Some(item) = c.map(|c| self.book.string_item(c)).transpose()?.flatten()
        {
            let mut runs: Vec<Value> = Vec::new();
            for e in item.elements() {
                let (text, format) = match e.local_name() {
                    "t" => (e.text(), style["font"].clone()),
                    "r" => (
                        e.child("t").map(Element::text).unwrap_or_default(),
                        e.child("rPr").map_or_else(|| style["font"].clone(), font),
                    ),
                    _ => continue,
                };
                if text.is_empty() {
                    continue;
                }
                if let Some(last) = runs.last_mut()
                    && last["format"] == format
                {
                    last["text"] = json!(format!("{}{}", last["text"].as_str().unwrap(), text));
                } else {
                    runs.push(json!({"text":text,"format":format}));
                }
            }
            if !(runs.is_empty() || runs.len() == 1 && runs[0]["format"] == style["font"]) {
                v["runs"] = json!(
                    runs.into_iter()
                        .map(|run| {
                            let mut f = run["format"].clone();
                            f["text"] = run["text"].clone();
                            f
                        })
                        .collect::<Vec<_>>()
                );
            }
            let unknown: Vec<_> = item
                .elements()
                .filter(|e| !["t", "r"].contains(&e.local_name()))
                .map(styles::canonical)
                .collect();
            if !unknown.is_empty() {
                v["annotations"] = json!(unknown);
            }
        }
        v["style"] = style;
        Ok(v)
    }
    fn raw(&mut self, sheet: Option<&Sheet>, cell: &str) -> Result<(Option<Value>, Option<Value>)> {
        let c = sheet.map(|s| s.cell(cell)).transpose()?.flatten();
        let v = c.map(|c| self.book.value(c)).transpose()?;
        let style = if c.is_some() {
            Some(
                self.book
                    .resolved_style(sheet.unwrap().style_id(cell)?, &mut self.raw_styles)?
                    .clone(),
            )
        } else {
            None
        };
        Ok((v, style))
    }
}
fn content(book: &Book, c: Option<&Element>) -> Result<Value> {
    let mut v = json!({"kind":"blank","text":"","formula":null,"cachedValue":null});
    let Some(c) = c else { return Ok(v) };
    let raw = c.child("v").map(Element::text);
    let t = attr(Some(c), "t").unwrap_or("n");
    let item = book.string_item(c)?;
    let kind = if item.is_some() || t == "str" {
        "string"
    } else if raw.is_none() {
        "blank"
    } else {
        match t {
            "n" => "number",
            "b" => "boolean",
            "e" => "error",
            "d" => "date",
            _ => "string",
        }
    };
    let text = if let Some(item) = item {
        string_text(item)
    } else {
        raw.clone().map_or_else(String::new, |s| match kind {
            "number" => numeric(&s),
            "boolean" => match s.as_str() {
                "1" | "true" => "true".into(),
                "0" | "false" => "false".into(),
                _ => s,
            },
            _ => s,
        })
    };
    if let Some(f) = c.child("f") {
        let mut formula = f.text();
        if formula.is_empty() && attr(Some(f), "t") == Some("shared") {
            formula = format!("shared:{}", attr(Some(f), "si").unwrap_or(""));
        }
        v["kind"] = json!("formula");
        v["formula"] = json!(formula);
        v["text"] = json!(format!("={formula}"));
        if raw.is_some() || item.is_some() {
            v["cachedValue"] = json!({"kind":kind,"text":text});
        }
    } else {
        v["kind"] = json!(kind);
        v["text"] = json!(text);
    }
    extras(&mut v, Some(c), &["r", "s", "t"], &["v", "f", "is"]);
    Ok(v)
}
fn labels(before: &Value, after: &Value) -> Vec<&'static str> {
    let mut tags = Vec::new();
    for (key, tag) in [
        ("kind", "type"),
        ("text", "text"),
        ("formula", "formula"),
        ("cachedValue", "cachedValue"),
    ] {
        if before[key] != after[key] {
            tags.push(tag);
        }
    }
    if before["text"] == after["text"] && before["runs"] != after["runs"] {
        tags.push("runs");
    }
    for key in [
        "font",
        "fill",
        "border",
        "alignment",
        "numberFormat",
        "protection",
        "quotePrefix",
    ] {
        if before["style"][key] != after["style"][key] {
            tags.push(key);
        }
    }
    if ["attributes", "extensions", "annotations"]
        .iter()
        .any(|k| before[*k] != after[*k] || before["style"][*k] != after["style"][*k])
    {
        tags.push("metadata");
    }
    tags
}
#[derive(Default)]
struct List {
    total: usize,
    details: Vec<Value>,
}
impl List {
    fn push(&mut self, v: Value, max: usize) {
        self.total += 1;
        if self.details.len() < max {
            self.details.push(v);
        }
    }
    fn value(self) -> Value {
        json!({"total":self.total,"truncated":self.total>self.details.len(),"details":self.details})
    }
}
fn increment(map: &mut BTreeMap<String, usize>, key: &str) {
    *map.entry(key.to_owned()).or_default() += 1;
}

type RowTokens = BTreeMap<u32, BTreeMap<u32, String>>;
type RowPair = (Option<u32>, Option<u32>);
fn row_tokens(book: &Book, sheet: Option<&Sheet>) -> Result<RowTokens> {
    let mut rows: RowTokens = BTreeMap::new();
    if let Some(sheet) = sheet {
        for cell in sheet.cells.keys() {
            let v = content(book, sheet.cell(cell)?)?;
            if nonempty(&v) {
                let (col, row) = address(cell)?;
                rows.entry(row).or_default().insert(
                    col,
                    json!([
                        v["kind"],
                        if v["kind"] == "formula" {
                            &v["formula"]
                        } else {
                            &v["text"]
                        }
                    ])
                    .to_string(),
                );
            }
        }
    }
    Ok(rows)
}
fn nonempty(v: &Value) -> bool {
    v["kind"] != "blank" && !(v["kind"] == "string" && v["text"] == "")
}
fn signatures(rows: &RowTokens, last: u32, ids: &mut BTreeMap<String, u32>) -> Vec<u32> {
    let mut out = vec![0; last as usize];
    for (row, tokens) in rows {
        let key = super::super::package::sha(serde_json::to_string(tokens).unwrap().as_bytes());
        let next = ids.len() as u32 + 1;
        let id = *ids.entry(key).or_insert(next);
        out[*row as usize - 1] = id;
    }
    out
}
/// Myers middle-snake bisection: linear memory, unlike retaining every frontier.
fn lcs(a: &[u32], b: &[u32], ao: usize, bo: usize, out: &mut Vec<(usize, usize)>) {
    let prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    out.extend((0..prefix).map(|i| (ao + i, bo + i)));
    let (a, b) = (&a[prefix..], &b[prefix..]);
    let (ao, bo) = (ao + prefix, bo + prefix);
    let suffix = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (x, y) = (&a[..a.len() - suffix], &b[..b.len() - suffix]);
    if !x.is_empty()
        && !y.is_empty()
        && let Some((i, j)) = middle(x, y)
        && (i != 0 || j != 0)
        && (i != x.len() || j != y.len())
    {
        lcs(&x[..i], &y[..j], ao, bo, out);
        lcs(&x[i..], &y[j..], ao + i, bo + j, out);
    }
    out.extend((0..suffix).map(|i| (ao + a.len() - suffix + i, bo + b.len() - suffix + i)));
}
fn middle(a: &[u32], b: &[u32]) -> Option<(usize, usize)> {
    let tokens: BTreeSet<_> = a.iter().collect();
    if !b.iter().any(|v| tokens.contains(v)) {
        return None;
    }
    let (n, m) = (a.len() as isize, b.len() as isize);
    let max = (n + m + 1) / 2;
    let offset = max + 1;
    let mut front = vec![-1; (2 * max + 3) as usize];
    let mut back = front.clone();
    front[(offset + 1) as usize] = 0;
    back[(offset + 1) as usize] = 0;
    let delta = n - m;
    let odd = delta % 2 != 0;
    for d in 0..=max {
        for k in (-d..=d).step_by(2) {
            let at = (offset + k) as usize;
            let mut x = if k == -d || k != d && front[at - 1] < front[at + 1] {
                front[at + 1]
            } else {
                front[at - 1] + 1
            };
            let mut y = x - k;
            while x >= 0 && y >= 0 && x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            front[at] = x;
            let reverse = delta - k;
            if odd
                && reverse >= -(d - 1)
                && reverse < d
                && back[(offset + reverse) as usize] >= 0
                && x + back[(offset + reverse) as usize] >= n
            {
                return Some((x as usize, y as usize));
            }
        }
        for k in (-d..=d).step_by(2) {
            let at = (offset + k) as usize;
            let mut x = if k == -d || k != d && back[at - 1] < back[at + 1] {
                back[at + 1]
            } else {
                back[at - 1] + 1
            };
            let mut y = x - k;
            while x >= 0
                && y >= 0
                && x < n
                && y < m
                && a[(n - x - 1) as usize] == b[(m - y - 1) as usize]
            {
                x += 1;
                y += 1;
            }
            back[at] = x;
            let forward = delta - k;
            if !odd
                && forward >= -d
                && forward <= d
                && front[(offset + forward) as usize] >= 0
                && x + front[(offset + forward) as usize] >= n
            {
                let fx = front[(offset + forward) as usize];
                return Some((fx as usize, (fx - forward) as usize));
            }
        }
    }
    None
}
fn pair_block(
    before: &[u32],
    after: &[u32],
    bt: &RowTokens,
    at: &RowTokens,
    out: &mut Vec<RowPair>,
) {
    let (n, m) = (before.len(), after.len());
    if n > 200 || m > 200 {
        let paired = n.min(m);
        out.extend((0..paired).map(|i| (Some(before[i]), Some(after[i]))));
        out.extend(before[paired..].iter().map(|r| (Some(*r), None)));
        out.extend(after[paired..].iter().map(|r| (None, Some(*r))));
        return;
    }
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    let score = |i: usize, j: usize| {
        let same = bt.get(&before[i]).map_or(0, |row| {
            row.iter()
                .filter(|(c, v)| at.get(&after[j]).and_then(|r| r.get(c)) == Some(*v))
                .count()
        });
        same * 201 + 1
    };
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = (score(i, j) + dp[i + 1][j + 1])
                .max(dp[i + 1][j])
                .max(dp[i][j + 1]);
        }
    }
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && dp[i][j] == score(i, j) + dp[i + 1][j + 1] {
            out.push((Some(before[i]), Some(after[j])));
            i += 1;
            j += 1;
        } else if i < n && (j == m || dp[i + 1][j] >= dp[i][j + 1]) {
            out.push((Some(before[i]), None));
            i += 1;
        } else {
            out.push((None, Some(after[j])));
            j += 1;
        }
    }
}
fn last_row(sheet: Option<&Sheet>) -> u32 {
    sheet.map_or(0, |s| {
        s.rows
            .last_key_value()
            .map_or(0, |(r, _)| *r)
            .max(s.merged.iter().map(|m| m.bottom).max().unwrap_or(0))
    })
}

fn row_pairs(
    before: &Book,
    after: &Book,
    bs: Option<&Sheet>,
    as_: Option<&Sheet>,
    align: bool,
) -> Result<Vec<RowPair>> {
    let (bl, al) = (last_row(bs), last_row(as_));
    let last = bl.max(al);
    if !align {
        return Ok((1..=last).map(|r| (Some(r), Some(r))).collect());
    }
    let bt = row_tokens(before, bs)?;
    let at = row_tokens(after, as_)?;
    let mut ids = BTreeMap::new();
    let b = signatures(&bt, last, &mut ids);
    let a = signatures(&at, last, &mut ids);
    let mut matches = Vec::new();
    lcs(&b, &a, 0, 0, &mut matches);
    let mut out = Vec::new();
    let (mut bi, mut ai) = (0usize, 0usize);
    for (br, ar) in matches
        .into_iter()
        .chain(std::iter::once((last as usize, last as usize)))
    {
        let b: Vec<_> = (bi..br)
            .map(|r| r as u32 + 1)
            .filter(|r| *r <= bl)
            .collect();
        let a: Vec<_> = (ai..ar)
            .map(|r| r as u32 + 1)
            .filter(|r| *r <= al)
            .collect();
        pair_block(&b, &a, &bt, &at, &mut out);
        if br < last as usize {
            out.push((Some(br as u32 + 1), Some(ar as u32 + 1)));
        }
        bi = br + 1;
        ai = ar + 1;
    }
    Ok(out)
}
fn alignment_report(name: &str, pairs: &[RowPair]) -> Value {
    let ranges = |before: bool| {
        let mut out: Vec<Value> = Vec::new();
        let key = if before { "beforeRow" } else { "afterRow" };
        for (b, a) in pairs {
            let row = if before && a.is_none() {
                *b
            } else if !before && b.is_none() {
                *a
            } else {
                None
            };
            if let Some(row) = row {
                if let Some(last) = out.last_mut()
                    && last[key].as_u64().unwrap() + last["count"].as_u64().unwrap() == row as u64
                {
                    last["count"] = json!(last["count"].as_u64().unwrap() + 1);
                } else {
                    out.push(json!({key:row,"count":1}));
                }
            }
        }
        out
    };
    let mut shifted: Vec<(u32, u32, u32, u32)> = Vec::new();
    for (b, a) in pairs {
        if let (Some(b), Some(a)) = (b, a)
            && b != a
        {
            if let Some(last) = shifted.last_mut()
                && last.1 + 1 == *b
                && last.3 + 1 == *a
            {
                last.1 = *b;
                last.3 = *a;
            } else {
                shifted.push((*b, *b, *a, *a));
            }
        }
    }
    json!({"sheet":name,"inserted":ranges(false),"deleted":ranges(true),"shifted":shifted.into_iter().map(|(b,e,a,z)|json!({"beforeRows":format!("{b}-{e}"),"afterRows":format!("{a}-{z}"),"offset":i64::from(a)-i64::from(b)})).collect::<Vec<_>>()})
}
fn row_layout(view: &mut View<'_>, sheet: Option<&Sheet>, row: Option<u32>) -> Result<Value> {
    let root = sheet.map(Sheet::root).transpose()?;
    let defaults = root.and_then(|r| r.child("sheetFormatPr"));
    let r = sheet.zip(row).map(|(s, r)| s.row(r)).transpose()?.flatten();
    let custom = flag(r, "customHeight", false);
    let mut height = number(r, "ht");
    if !custom && height == number(defaults, "defaultRowHeight") {
        height = Value::Null;
    }
    let style = if flag(r, "customFormat", false) {
        Some(view.style(attr_u32(r.unwrap(), "s", 0)? as usize)?)
    } else {
        None
    };
    Ok(
        json!({"height":height,"customHeight":custom,"hidden":flag(r,"hidden",false),"outlineLevel":attr(r,"outlineLevel").map_or(json!(0.0), |_|number(r,"outlineLevel")),"style":style}),
    )
}
fn column_layout(view: &mut View<'_>, sheet: Option<&Sheet>, col: u32) -> Result<Value> {
    let c = sheet.map(|s| s.column(col)).transpose()?.flatten();
    let defaults = sheet
        .map(Sheet::root)
        .transpose()?
        .and_then(|r| r.child("sheetFormatPr"));
    let mut width = number(c, "width");
    if width == number(defaults, "defaultColWidth") {
        width = Value::Null;
    }
    let id = c.map(|c| attr_u32(c, "style", 0)).transpose()?.unwrap_or(0) as usize;
    Ok(
        json!({"width":width,"hidden":flag(c,"hidden",false),"outlineLevel":attr(c,"outlineLevel").map_or(json!(0.0), |_|number(c,"outlineLevel")),"style":view.style(id)?}),
    )
}
fn columns(sheet: Option<&Sheet>) -> Result<BTreeSet<u32>> {
    let mut columns = BTreeSet::new();
    if let Some(cols) = sheet
        .map(Sheet::root)
        .transpose()?
        .and_then(|s| s.child("cols"))
    {
        for c in cols.elements() {
            columns.extend(attr_u32(c, "min", 0)?..=attr_u32(c, "max", 0)?);
        }
    }
    Ok(columns)
}
fn sheet_defaults(sheet: Option<&Sheet>) -> Result<Value> {
    let e = sheet
        .map(Sheet::root)
        .transpose()?
        .and_then(|r| r.child("sheetFormatPr"));
    let mut v = json!({});
    for key in [
        "baseColWidth",
        "defaultColWidth",
        "defaultRowHeight",
        "outlineLevelRow",
        "outlineLevelCol",
    ] {
        if attr(e, key).is_some() {
            v[key] = number(e, key);
        }
    }
    for key in ["customHeight", "zeroHeight", "thickTop", "thickBottom"] {
        if flag(e, key, false) {
            v[key] = json!(true);
        }
    }
    extras(
        &mut v,
        e,
        &[
            "baseColWidth",
            "defaultColWidth",
            "defaultRowHeight",
            "outlineLevelRow",
            "outlineLevelCol",
            "customHeight",
            "zeroHeight",
            "thickTop",
            "thickBottom",
        ],
        &[],
    );
    Ok(v)
}

fn sheet_elements(
    sheet: Option<&Sheet>,
    excluded: &[&str],
) -> Result<BTreeMap<String, Vec<Value>>> {
    let mut out: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    if let Some(root) = sheet.map(Sheet::root).transpose()? {
        for e in root
            .elements()
            .filter(|e| !excluded.contains(&e.local_name()))
        {
            out.entry(e.local_name().into())
                .or_default()
                .push(styles::canonical(e));
        }
    }
    Ok(out)
}
fn merges(sheet: Option<&Sheet>, mapping: Option<&BTreeMap<u32, u32>>) -> BTreeSet<String> {
    sheet
        .into_iter()
        .flat_map(|s| s.merged.iter())
        .map(|m| {
            if let Some(map) = mapping {
                if let (Some(top), Some(bottom)) = (map.get(&m.top), map.get(&m.bottom)) {
                    let mut moved = *m;
                    moved.top = *top;
                    moved.bottom = *bottom;
                    return moved.reference();
                }
                // A deleted endpoint cannot accidentally equal an after-side merge.
                return format!("deleted:{}", m.reference());
            }
            m.reference()
        })
        .collect()
}
fn sheet_names(book: &Book) -> Result<Vec<Value>> {
    Ok(book.workbook.root().map_err(invalid)?.child("sheets").into_iter().flat_map(Element::elements).map(|s|json!({"name":s.attrs.get("name"),"state":attr(Some(s),"state").unwrap_or("visible")})).collect())
}
fn names(book: &Book) -> Result<BTreeMap<(String, Option<String>), String>> {
    let sheets = sheet_names(book)?;
    let mut names = BTreeMap::new();
    for e in book
        .workbook
        .root()
        .map_err(invalid)?
        .child("definedNames")
        .into_iter()
        .flat_map(Element::elements)
    {
        let name = attr(Some(e), "name").ok_or_else(|| invalid("definedName missing name"))?;
        let scope = attr(Some(e), "localSheetId")
            .map(|s| {
                s.parse::<usize>()
                    .ok()
                    .and_then(|i| sheets.get(i))
                    .and_then(|s| s["name"].as_str())
                    .map(str::to_owned)
                    .ok_or_else(|| invalid("definedName localSheetId out of bounds"))
            })
            .transpose()?;
        names.insert((name.into(), scope), e.text());
    }
    Ok(names)
}

fn stored_columns(sheet: Option<&Sheet>) -> Result<BTreeMap<u32, BTreeSet<u32>>> {
    let mut rows: BTreeMap<u32, BTreeSet<u32>> = BTreeMap::new();
    for cell in sheet.into_iter().flat_map(|s| s.cells.keys()) {
        let (c, r) = address(cell)?;
        rows.entry(r).or_default().insert(c);
    }
    Ok(rows)
}
fn cap_array(values: Vec<Value>, max: usize) -> (Value, usize, bool) {
    let total = values.len();
    (
        json!(values.into_iter().take(max).collect::<Vec<_>>()),
        total,
        total > max,
    )
}
fn cap_nested(values: &mut [Value], keys: &[&str], max: usize) -> bool {
    let mut truncated = false;
    for value in values {
        for key in keys {
            if let Some(array) = value[*key].as_array_mut() {
                let total = array.len();
                if total > max {
                    array.truncate(max);
                    value[format!("{key}Total")] = json!(total);
                    value["truncated"] = json!(true);
                    truncated = true;
                }
            }
        }
    }
    truncated
}

impl Book {
    pub fn semantic_diff(&self, other: &Self, max: usize, align: bool) -> Result<Value> {
        let mut before = View::new(self);
        let mut after = View::new(other);
        let mut cells = List::default();
        let mut by_sheet = BTreeMap::new();
        let mut equivalent = 0usize;
        let mut eq_sheets = BTreeMap::new();
        let mut eq_reasons = BTreeMap::from([("value".into(), 0usize), ("style".into(), 0usize)]);
        let mut sample = Vec::new();
        let mut rows = List::default();
        let mut cols = List::default();
        let mut merge_changes = Vec::new();
        let mut defaults = Vec::new();
        let mut structure = Vec::new();
        let mut views = Vec::new();
        let mut alignment = Vec::new();
        let sheet_set: BTreeSet<_> = self.sheets.keys().chain(other.sheets.keys()).collect();
        for name in sheet_set {
            let bs = self.sheets.get(name);
            let as_ = other.sheets.get(name);
            let pairs = row_pairs(self, other, bs, as_, align)?;
            if align {
                alignment.push(alignment_report(name, &pairs));
            }
            let bc = stored_columns(bs)?;
            let ac = stored_columns(as_)?;
            for (br, ar) in &pairs {
                let columns: BTreeSet<_> = br
                    .and_then(|r| bc.get(&r))
                    .into_iter()
                    .flatten()
                    .chain(ar.and_then(|r| ac.get(&r)).into_iter().flatten())
                    .copied()
                    .collect();
                for col in columns {
                    let bcell = format!("{}{}", column_name(col), br.or(*ar).unwrap());
                    let acell = format!("{}{}", column_name(col), ar.or(*br).unwrap());
                    let bv = before.cell(if br.is_some() { bs } else { None }, &bcell)?;
                    let av = after.cell(if ar.is_some() { as_ } else { None }, &acell)?;
                    let mut tags = labels(&bv, &av);
                    if align && br.is_none() {
                        if !nonempty(&av) {
                            continue;
                        }
                        tags.insert(0, "inserted");
                    }
                    if align && ar.is_none() {
                        if !nonempty(&bv) {
                            continue;
                        }
                        tags.insert(0, "deleted");
                    }
                    if !tags.is_empty() {
                        increment(&mut by_sheet, name);
                        let mut detail = json!({"sheet":name,"cell":acell,"changes":tags,"before":bv,"after":av});
                        if align {
                            detail["beforeCell"] = if br.is_some() {
                                json!(bcell)
                            } else {
                                Value::Null
                            };
                        }
                        cells.push(detail, max);
                    } else {
                        let (rv, rs) = before.raw(bs, &bcell)?;
                        let (av, ast) = after.raw(as_, &acell)?;
                        if rv != av || rs != ast {
                            equivalent += 1;
                            increment(&mut eq_sheets, name);
                            if rv != av {
                                increment(&mut eq_reasons, "value");
                            }
                            if rs != ast {
                                increment(&mut eq_reasons, "style");
                            }
                            if sample.len() < 20 {
                                sample.push(format!("{name}!{acell}"));
                            }
                        }
                    }
                }
                // Omit absent default rows without materializing JSON for large gaps.
                let has_before = bs.zip(*br).is_some_and(|(s, r)| s.rows.contains_key(&r));
                let has_after = as_.zip(*ar).is_some_and(|(s, r)| s.rows.contains_key(&r));
                if has_before || has_after {
                    let b = row_layout(&mut before, bs, *br)?;
                    let a = row_layout(&mut after, as_, *ar)?;
                    if b != a {
                        let mut detail =
                            json!({"sheet":name,"row":ar.or(*br).unwrap(),"before":b,"after":a});
                        if align {
                            detail["beforeRow"] = json!(br);
                        }
                        rows.push(detail, max);
                    }
                }
            }
            let selected: BTreeSet<_> = columns(bs)?.union(&columns(as_)?).copied().collect();
            let mut spans: Vec<(u32, u32, Value, Value)> = Vec::new();
            for col in selected {
                let b = column_layout(&mut before, bs, col)?;
                let a = column_layout(&mut after, as_, col)?;
                if b != a {
                    if let Some(last) = spans.last_mut()
                        && last.1 + 1 == col
                        && last.2 == b
                        && last.3 == a
                    {
                        last.1 = col;
                    } else {
                        spans.push((col, col, b, a));
                    }
                }
            }
            for (first, last, b, a) in spans {
                let range = if first == last {
                    column_name(first)
                } else {
                    format!("{}:{}", column_name(first), column_name(last))
                };
                cols.push(
                    json!({"sheet":name,"columns":range,"before":b,"after":a}),
                    max,
                );
            }
            let map: BTreeMap<_, _> = pairs.iter().filter_map(|(b, a)| b.zip(*a)).collect();
            let bm = merges(bs, align.then_some(&map));
            let am = merges(as_, None);
            let added: Vec<_> = am.difference(&bm).cloned().collect();
            let removed: Vec<_> = bm
                .difference(&am)
                .map(|s| s.strip_prefix("deleted:").unwrap_or(s).to_owned())
                .collect();
            if !added.is_empty() || !removed.is_empty() {
                merge_changes.push(json!({"sheet":name,"added":added,"removed":removed}));
            }
            let be = sheet_elements(
                bs,
                &[
                    "sheetData",
                    "cols",
                    "mergeCells",
                    "sheetViews",
                    "dimension",
                    "sheetFormatPr",
                ],
            )?;
            let ae = sheet_elements(
                as_,
                &[
                    "sheetData",
                    "cols",
                    "mergeCells",
                    "sheetViews",
                    "dimension",
                    "sheetFormatPr",
                ],
            )?;
            let keys: BTreeSet<_> = be.keys().chain(ae.keys()).collect();
            let changed: Vec<_> = keys
                .into_iter()
                .filter(|k| be.get(*k) != ae.get(*k))
                .collect();
            if !changed.is_empty() {
                structure.push(json!({"sheet":name,"elements":changed}));
            }
            let b = sheet_defaults(bs)?;
            let a = sheet_defaults(as_)?;
            if b != a {
                defaults.push(json!({"sheet":name,"before":b,"after":a}));
            }
            let b = bs
                .map(Sheet::root)
                .transpose()?
                .and_then(|s| s.child("sheetViews"))
                .map(styles::canonical);
            let a = as_
                .map(Sheet::root)
                .transpose()?
                .and_then(|s| s.child("sheetViews"))
                .map(styles::canonical);
            if b != a {
                views.push(name);
            }
        }
        let bn = names(self)?;
        let an = names(other)?;
        let keys: BTreeSet<_> = bn.keys().chain(an.keys()).collect();
        let mut print = Vec::new();
        let mut defined = Vec::new();
        for key in keys {
            if bn.get(key) != an.get(key) {
                let (name, scope) = key;
                if ["_xlnm.Print_Area", "_xlnm.Print_Titles"].contains(&name.as_str())
                    && scope.is_some()
                {
                    print.push(
                        json!({"sheet":scope,"name":name,"before":bn.get(key),"after":an.get(key)}),
                    );
                } else {
                    defined.push(
                        json!({"name":name,"scope":scope,"before":bn.get(key),"after":an.get(key)}),
                    );
                }
            }
        }
        let bs = sheet_names(self)?;
        let as_ = sheet_names(other)?;
        let sheets = if bs == as_ {
            Value::Null
        } else {
            json!({"before":bs,"after":as_})
        };
        let theme = self.context["themeParts"] != other.context["themeParts"];
        let wb_views = self
            .workbook
            .root()
            .map_err(invalid)?
            .child("bookViews")
            .map(styles::canonical)
            != other
                .workbook
                .root()
                .map_err(invalid)?
                .child("bookViews")
                .map(styles::canonical);
        let mut total = cells.total
            + rows.total
            + cols.total
            + print.len()
            + defaults.len()
            + structure.len()
            + defined.len()
            + usize::from(!sheets.is_null())
            + usize::from(theme);
        total += merge_changes
            .iter()
            .map(|v| v["added"].as_array().unwrap().len() + v["removed"].as_array().unwrap().len())
            .sum::<usize>();
        let nested_merges = cap_nested(&mut merge_changes, &["added", "removed"], max);
        let nested_structure = cap_nested(&mut structure, &["elements"], max);
        let nested_alignment = cap_nested(&mut alignment, &["inserted", "deleted", "shifted"], max);
        let (alignment, alignment_total, at) = cap_array(alignment, max);
        let (merges, merge_total, mt) = cap_array(merge_changes, max);
        let (print, print_total, pt) = cap_array(print, max);
        let (defaults, default_total, dt) = cap_array(defaults, max);
        let (structure, structure_total, st) = cap_array(structure, max);
        let (defined, defined_total, nt) = cap_array(defined, max);
        let layout_truncated = rows.total > rows.details.len()
            || cols.total > cols.details.len()
            || mt
            || pt
            || dt
            || st
            || nested_merges
            || nested_structure;
        let truncated =
            layout_truncated || at || nested_alignment || cells.total > cells.details.len() || nt;
        let mut cell_changes = cells.value();
        cell_changes["bySheet"] = json!(by_sheet);
        Ok(
            json!({"alignRows":align,"cellChanges":cell_changes,"equivalentOnly":{"total":equivalent,"bySheet":eq_sheets,"byReason":eq_reasons,"sample":sample},"rowAlignment":alignment,"rowAlignmentTotal":alignment_total,
            "layoutChanges":{"rows":rows.value(),"columns":cols.value(),"merges":merges,"printAreas":print,"sheetDefaults":defaults,"otherStructure":structure,"totals":{"merges":merge_total,"printAreas":print_total,"sheetDefaults":default_total,"otherStructure":structure_total},"truncated":layout_truncated},
            "workbookChanges":{"sheets":sheets,"definedNames":defined,"definedNamesTotal":defined_total,"truncated":nt,"themeChanged":theme},"viewOnly":{"sheets":views,"workbook":wb_views},"summary":{"semanticTotal":total,"equivalentTotal":equivalent},"truncated":truncated,"normalization":NORMALIZATION}),
        )
    }
}
