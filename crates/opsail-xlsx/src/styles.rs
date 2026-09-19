use super::xml::{Document, Element, Node};
use super::{Result, Style, invalid};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub fn canonical(e: &Element) -> Value {
    let attrs: BTreeMap<_, _> = e
        .attrs
        .iter()
        .filter(|(k, _)| !k.starts_with("xmlns"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let children: Vec<_> = e
        .children
        .iter()
        .filter_map(|n| match n {
            Node::Element(x) => Some(canonical(x)),
            Node::Text(t) if !t.trim().is_empty() || e.local_name() == "t" => Some(json!(t)),
            _ => None,
        })
        .collect();
    json!({"name":e.local_name(),"attributes":attrs,"children":children})
}
pub fn make(parent: &Element, name: &str) -> Element {
    Element::new(
        &parent
            .name
            .rsplit_once(':')
            .map_or_else(|| name.to_owned(), |(p, _)| format!("{p}:{name}")),
    )
}
fn index(e: &Element, name: &str) -> Result<usize> {
    e.attrs.get(name).map_or(Ok(0), |n| {
        n.parse()
            .map_err(|_| invalid(format!("invalid style index {name}")))
    })
}
fn record(root: &Element, collection: &str, i: usize) -> Result<Element> {
    root.child(collection)
        .and_then(|s| s.elements().nth(i))
        .cloned()
        .ok_or_else(|| invalid(format!("{collection} index {i} missing")))
}
fn numfmt(root: &Element, id: usize) -> Value {
    root.child("numFmts")
        .and_then(|x| {
            x.elements().find(|x| {
                x.attrs
                    .get("numFmtId")
                    .and_then(|s| s.parse::<usize>().ok())
                    == Some(id)
            })
        })
        .map_or_else(
            || json!({"builtInId":id}),
            |x| json!({"formatCode":x.attrs.get("formatCode")}),
        )
}
fn components(root: &Element, xf: &Element) -> Result<Value> {
    let mut attrs = xf.attrs.clone();
    for k in ["fontId", "fillId", "borderId", "numFmtId", "xfId"] {
        attrs.remove(k);
    }
    Ok(
        json!({"font":canonical(&record(root,"fonts",index(xf,"fontId")?)?),"fill":canonical(&record(root,"fills",index(xf,"fillId")?)?),
        "border":canonical(&record(root,"borders",index(xf,"borderId")?)?),"numberFormat":numfmt(root,index(xf,"numFmtId")?),
        "alignment":xf.child("alignment").map(canonical),"protection":xf.child("protection").map(canonical),"flags":attrs,
        "extensions":xf.elements().filter(|e|!["alignment","protection"].contains(&e.local_name())).map(canonical).collect::<Vec<_>>()}),
    )
}
pub fn resolved(doc: &Document, id: usize) -> Result<Value> {
    let root = doc.root().map_err(invalid)?;
    let xf = record(root, "cellXfs", id)?;
    let mut v = components(root, &xf)?;
    if root.child("cellStyleXfs").is_some() {
        v["baseStyle"] = components(root, &record(root, "cellStyleXfs", index(&xf, "xfId")?)?)?;
    }
    Ok(v)
}
/// Valid only for one mutable style document. v1 appends records and never
/// changes existing ones, so the first matching index remains stable.
#[derive(Default)]
pub struct AppendCache {
    collections: BTreeMap<String, BTreeMap<String, usize>>,
}
impl AppendCache {
    fn append(&mut self, root: &mut Element, name: &str, e: Element) -> Result<usize> {
        let list = root
            .child_mut(name)
            .ok_or_else(|| invalid(format!("missing styles {name}")))?;
        let signatures = self.collections.entry(name.to_owned()).or_insert_with(|| {
            let mut map = BTreeMap::new();
            for (i, record) in list.elements().enumerate() {
                map.entry(canonical(record).to_string()).or_insert(i);
            }
            map
        });
        let signature = canonical(&e).to_string();
        if let Some(i) = signatures.get(&signature) {
            return Ok(*i);
        }
        let i = list.elements().count();
        if i >= 65000 {
            return Err(invalid("style collection limit exceeded"));
        }
        list.children.push(Node::Element(e));
        list.attrs.insert("count".into(), (i + 1).to_string());
        signatures.insert(signature, i);
        Ok(i)
    }
}
fn set_child(parent: &mut Element, name: &str, new: Option<Element>) {
    let i = parent
        .children
        .iter()
        .position(|n| matches!(n,Node::Element(e) if e.local_name()==name));
    if let Some(i) = i {
        parent.children.remove(i);
        if let Some(e) = new {
            parent.children.insert(i, Node::Element(e));
        }
    } else if let Some(e) = new {
        // extLst must stay last in xf/font content
        let at = parent
            .children
            .iter()
            .position(|n| matches!(n,Node::Element(x) if x.local_name()=="extLst" || (name=="alignment" && x.local_name()=="protection")))
            .unwrap_or(parent.children.len());
        parent.children.insert(at, Node::Element(e));
    }
}
fn rgb(s: &str) -> Result<String> {
    if ![6, 8].contains(&s.len()) || !s.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(invalid("color must be 6-digit RGB or 8-digit ARGB"));
    }
    Ok(if s.len() == 6 {
        format!("FF{}", s.to_ascii_uppercase())
    } else {
        s.to_ascii_uppercase()
    })
}
pub fn edit(doc: &mut Document, cache: &mut AppendCache, id: usize, s: &Style) -> Result<usize> {
    if s.font_color.is_none()
        && s.fill_color.is_none()
        && s.bold.is_none()
        && s.strike.is_none()
        && s.wrap_text.is_none()
        && s.horizontal.is_none()
        && s.vertical.is_none()
        && s.indent.is_none()
    {
        return Err(invalid("style patch must specify at least one field"));
    }
    if s.horizontal.as_deref().is_some_and(|x| {
        ![
            "general",
            "left",
            "center",
            "right",
            "fill",
            "justify",
            "centerContinuous",
            "distributed",
        ]
        .contains(&x)
    }) || s
        .vertical
        .as_deref()
        .is_some_and(|x| !["top", "center", "bottom", "justify", "distributed"].contains(&x))
        || s.indent.is_some_and(|x| x > 250)
    {
        return Err(invalid("alignment outside supported bounds"));
    }
    let root = doc.root_mut().map_err(invalid)?;
    let mut xf = record(root, "cellXfs", id)?;
    for (requested, flag) in [
        (
            s.font_color.is_some() || s.bold.is_some() || s.strike.is_some(),
            "applyFont",
        ),
        (s.fill_color.is_some(), "applyFill"),
        (
            s.wrap_text.is_some()
                || s.horizontal.is_some()
                || s.vertical.is_some()
                || s.indent.is_some(),
            "applyAlignment",
        ),
    ] {
        if requested && xf.attrs.get(flag).is_some_and(|v| v == "0" || v == "false") {
            return Err(invalid(format!(
                "{flag}=false requires native application to preserve inherited formatting"
            )));
        }
    }
    if s.font_color.is_some() || s.bold.is_some() || s.strike.is_some() {
        let mut font = record(root, "fonts", index(&xf, "fontId")?)?;
        if let Some(color) = &s.font_color {
            let mut e = make(&font, "color");
            e.attrs.insert("rgb".into(), rgb(color)?);
            set_child(&mut font, "color", Some(e));
        }
        for (k, value) in [("b", s.bold), ("strike", s.strike)] {
            if let Some(value) = value {
                let mut e = make(&font, k);
                e.attrs
                    .insert("val".into(), if value { "1" } else { "0" }.into());
                set_child(&mut font, k, Some(e));
            }
        }
        let fid = cache.append(root, "fonts", font)?;
        xf.attrs.insert("fontId".into(), fid.to_string());
        xf.attrs.insert("applyFont".into(), "1".into());
    }
    if let Some(color) = &s.fill_color {
        let mut fill = make(root, "fill");
        let mut pattern = make(root, "patternFill");
        pattern.attrs.insert("patternType".into(), "solid".into());
        let mut fg = make(root, "fgColor");
        fg.attrs.insert("rgb".into(), rgb(color)?);
        pattern.children.push(Node::Element(fg));
        let mut bg = make(root, "bgColor");
        bg.attrs.insert("indexed".into(), "64".into());
        pattern.children.push(Node::Element(bg));
        fill.children.push(Node::Element(pattern));
        let fid = cache.append(root, "fills", fill)?;
        xf.attrs.insert("fillId".into(), fid.to_string());
        xf.attrs.insert("applyFill".into(), "1".into());
    }
    if s.wrap_text.is_some() || s.horizontal.is_some() || s.vertical.is_some() || s.indent.is_some()
    {
        let mut a = xf
            .child("alignment")
            .cloned()
            .unwrap_or_else(|| make(&xf, "alignment"));
        if let Some(v) = s.wrap_text {
            a.attrs
                .insert("wrapText".into(), if v { "1" } else { "0" }.into());
        }
        if let Some(v) = &s.horizontal {
            a.attrs.insert("horizontal".into(), v.clone());
        }
        if let Some(v) = &s.vertical {
            a.attrs.insert("vertical".into(), v.clone());
        }
        if let Some(v) = s.indent {
            a.attrs.insert("indent".into(), v.to_string());
        }
        set_child(&mut xf, "alignment", Some(a));
        xf.attrs.insert("applyAlignment".into(), "1".into());
    }
    cache.append(root, "cellXfs", xf)
}
pub fn copy(
    doc: &mut Document,
    cache: &mut AppendCache,
    target: usize,
    donor: usize,
    selected: &[String],
) -> Result<usize> {
    if selected.is_empty() || selected.len() > 5 {
        return Err(invalid("copyStyle requires 1-5 distinct components"));
    }
    let unique: std::collections::BTreeSet<_> = selected.iter().collect();
    if unique.len() != selected.len() {
        return Err(invalid("duplicate style components"));
    }
    let root = doc.root_mut().map_err(invalid)?;
    let mut to = record(root, "cellXfs", target)?;
    let from = record(root, "cellXfs", donor)?;
    for name in selected {
        let (attribute, flag) = match name.as_str() {
            "font" => ("fontId", "applyFont"),
            "fill" => ("fillId", "applyFill"),
            "border" => ("borderId", "applyBorder"),
            "numberFormat" => ("numFmtId", "applyNumberFormat"),
            "alignment" => ("", "applyAlignment"),
            _ => return Err(invalid("unsupported copyStyle component")),
        };
        if name == "alignment" {
            set_child(&mut to, "alignment", from.child("alignment").cloned());
        } else {
            // absent IDs mean the default record, not the target's previous record
            to.attrs.insert(
                attribute.into(),
                from.attrs
                    .get(attribute)
                    .cloned()
                    .unwrap_or_else(|| "0".into()),
            );
        }
        if let Some(value) = from.attrs.get(flag) {
            to.attrs.insert(flag.into(), value.clone());
        } else {
            to.attrs.remove(flag);
        }
        // Differing base styles with disabled application are ambiguous. Refuse
        // instead of claiming a component was copied while inheritance wins.
        if index(&from, "xfId")? != index(&to, "xfId")?
            && from.attrs.get(flag).is_none_or(|v| v != "1")
        {
            return Err(invalid(
                "copyStyle across different inherited base styles requires native application",
            ));
        }
    }
    cache.append(root, "cellXfs", to)
}
