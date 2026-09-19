use super::{
    Error, Operation, Result, invalid,
    package::Package,
    styles,
    xml::{self, Document, Element, Node},
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
const MAIN: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const MAX_STORED_CELLS: usize = 200000;
pub fn modeled_part(p: &str) -> bool {
    p == "xl/workbook.xml"
        || p == "xl/styles.xml"
        || p == "xl/sharedStrings.xml"
        || p.starts_with("xl/worksheets/") && !p.contains("/_rels/")
}
pub struct Book {
    sheets: BTreeMap<String, Sheet>,
    styles: Document,
    shared: Vec<Element>,
    append_cache: styles::AppendCache,
    workbook: Document,
    style_path: String,
    dirty: BTreeSet<String>,
    context: Value,
    signed: bool,
}
struct Sheet {
    part: String,
    doc: Document,
    // Positions within sheetData/row children remain stable: v1 changes cell
    // contents and attributes, never inserts or removes rows or cells.
    rows: BTreeMap<u32, usize>,
    cells: BTreeMap<String, (usize, usize)>,
    merged: Vec<Area>,
}
fn attr_u32(e: &Element, name: &str, default: u32) -> Result<u32> {
    e.attrs.get(name).map_or(Ok(default), |s| {
        s.parse()
            .map_err(|_| invalid(format!("invalid numeric attribute {name}")))
    })
}
fn check_root(doc: &Document, name: &str) -> Result<()> {
    let root = doc.root().map_err(invalid)?;
    let key = root
        .name
        .rsplit_once(':')
        .map_or_else(|| "xmlns".to_string(), |(p, _)| format!("xmlns:{p}"));
    if root.local_name() != name || root.attrs.get(&key).map(String::as_str) != Some(MAIN) {
        return Err(invalid(format!("unsupported {name} XML namespace")));
    }
    // v1 deliberately supports one stable prefix for SpreadsheetML per part.
    // Refuse namespace rebinding instead of interpreting a foreign child as a
    // workbook element solely because it has a familiar local name.
    fn stable(e: &Element, key: &str) -> bool {
        !e.attrs.iter().any(|(k, v)| {
            (k == key && v != MAIN) || (k.starts_with("xmlns") && k != key && v == MAIN)
        }) && e.elements().all(|child| stable(child, key))
    }
    if !stable(root, &key) {
        return Err(invalid(
            "namespace rebinding or multiple SpreadsheetML prefixes require native application",
        ));
    }
    Ok(())
}
fn target_path(s: &str) -> Result<String> {
    let path = if s.starts_with('/') {
        s.trim_start_matches('/').to_string()
    } else {
        format!("xl/{s}")
    };
    if path.contains('\\') || path.split('/').any(|s| s == ".." || s == ".") {
        return Err(invalid("unsupported relationship target path"));
    }
    Ok(path)
}
impl Book {
    pub fn load(pkg: &Package) -> Result<Self> {
        let workbook = pkg.xml("xl/workbook.xml")?;
        check_root(&workbook, "workbook")?;
        let rels = pkg.xml("xl/_rels/workbook.xml.rels")?;
        let mut relationships = BTreeMap::new();
        let mut style_path = None;
        let mut shared_path = None;
        for r in rels.root().map_err(invalid)?.elements() {
            if r.local_name() != "Relationship" {
                continue;
            }
            let id = r
                .attrs
                .get("Id")
                .ok_or_else(|| invalid("relationship missing Id"))?;
            if relationships.insert(id.clone(), r.clone()).is_some() {
                return Err(invalid("duplicate relationship Id"));
            }
            if let Some(t) = r.attrs.get("Type") {
                if t == &format!("{REL}/styles") {
                    style_path = Some(target_path(
                        r.attrs
                            .get("Target")
                            .ok_or_else(|| invalid("missing styles target"))?,
                    )?)
                }
                if t == &format!("{REL}/sharedStrings") {
                    shared_path = Some(target_path(
                        r.attrs
                            .get("Target")
                            .ok_or_else(|| invalid("missing SST target"))?,
                    )?)
                }
            }
        }
        let style_path = style_path
            .ok_or_else(|| invalid("a styles relationship is required for this capability"))?;
        let styles_doc = pkg.xml(&style_path)?;
        check_root(&styles_doc, "styleSheet")?;
        let shared = if let Some(path) = shared_path {
            let d = pkg.xml(&path)?;
            check_root(&d, "sst")?;
            d.root()
                .map_err(invalid)?
                .elements()
                .filter(|e| e.local_name() == "si")
                .cloned()
                .collect()
        } else {
            vec![]
        };
        let root = workbook.root().map_err(invalid)?;
        let sheet_list = root
            .child("sheets")
            .ok_or_else(|| invalid("missing sheets"))?;
        let mut sheets = BTreeMap::new();
        let mut cell_count = 0;
        for sheet in sheet_list.elements() {
            if sheet.local_name() != "sheet" {
                continue;
            }
            let name = sheet
                .attrs
                .get("name")
                .ok_or_else(|| invalid("sheet missing name"))?
                .clone();
            let id = sheet
                .attrs
                .iter()
                .find(|(k, _)| k.ends_with(":id"))
                .map(|(_, v)| v)
                .ok_or_else(|| invalid("sheet missing relationship Id"))?;
            let rel = relationships
                .get(id)
                .ok_or_else(|| invalid("sheet relationship missing"))?;
            if rel.attrs.get("TargetMode").is_some_and(|v| v == "External") {
                return Err(invalid("external sheet relationship"));
            }
            if rel.attrs.get("Type").map(String::as_str)
                != Some(format!("{REL}/worksheet").as_str())
            {
                return Err(invalid("only worksheet sheets are supported"));
            }
            let part = target_path(
                rel.attrs
                    .get("Target")
                    .ok_or_else(|| invalid("sheet target missing"))?,
            )?;
            let doc = pkg.xml(&part)?;
            check_root(&doc, "worksheet")?;
            let s = Sheet::new(part, doc)?;
            cell_count += s.cells.len();
            if cell_count > MAX_STORED_CELLS {
                return Err(invalid(
                    "workbook exceeds 200000 stored cells for edit/diff capability",
                ));
            }
            if sheets.insert(name, s).is_some() {
                return Err(invalid("duplicate sheet name"));
            }
        }
        let context = json!({"themeParts":pkg.parts.iter().filter(|(p,_)|p.starts_with("xl/theme/")).map(|(p,b)|(p.clone(),super::package::sha(b))).collect::<BTreeMap<_,_>>(),
            "indexedColors":styles_doc.root().map_err(invalid)?.child("colors").map(styles::canonical), "additionalStyleDefinitions":styles_doc.root().map_err(invalid)?.elements().filter(|e| !["numFmts","fonts","fills","borders","cellStyleXfs","cellXfs","colors"].contains(&e.local_name())).map(styles::canonical).collect::<Vec<_>>()});
        Ok(Self {
            sheets,
            styles: styles_doc,
            shared,
            append_cache: styles::AppendCache::default(),
            workbook,
            style_path,
            dirty: BTreeSet::new(),
            context,
            signed: pkg.parts.keys().any(|p| p.starts_with("_xmlsignatures/")),
        })
    }
    fn sheet(&self, name: &str) -> Result<&Sheet> {
        self.sheets
            .get(name)
            .ok_or_else(|| invalid(format!("sheet not found: {name}")))
    }
    fn describe(
        &self,
        name: &str,
        cell: &str,
        cache: &mut BTreeMap<usize, Value>,
    ) -> Result<Value> {
        let s = self.sheet(name)?;
        let pos = address(cell)?;
        let c = s.cell(cell)?;
        let mut v = if let Some(c) = c {
            self.value(c)?
        } else {
            json!({"text":null,"value":null,"kind":"blank","formula":null,"richText":null})
        };
        v["sheet"] = json!(name);
        v["cell"] = json!(cell);
        v["blank"] = json!(c.is_none());
        let id = s.style_id(cell)?;
        v["style"] = self.resolved_style(id, cache)?.clone();
        v["row"] = json!(s.row(pos.1)?.map(|r| r.attrs.clone()));
        v["column"] = json!(s.column(pos.0)?.map(|c| c.attrs.clone()));
        v["merge"] = json!(
            s.merged
                .iter()
                .copied()
                .find(|a| a.contains(pos))
                .map(|a| a.reference())
        );
        Ok(v)
    }
    fn string_item<'a>(&'a self, c: &'a Element) -> Result<Option<&'a Element>> {
        match c.attrs.get("t").map(String::as_str) {
            Some("s") => {
                let i = c
                    .child("v")
                    .ok_or_else(|| invalid("shared string index missing"))?
                    .text()
                    .parse::<usize>()
                    .map_err(|_| invalid("invalid shared string index"))?;
                Ok(Some(self.shared.get(i).ok_or_else(|| {
                    invalid("shared string index out of bounds")
                })?))
            }
            Some("inlineStr") => Ok(c.child("is")),
            _ => Ok(None),
        }
    }
    fn value(&self, c: &Element) -> Result<Value> {
        let item = self.string_item(c)?;
        let rich = item.is_some_and(|s| s.elements().any(|e| e.local_name() == "r"));
        let string = item.map(string_text).or_else(|| {
            if c.attrs.get("t").is_some_and(|s| s == "str") {
                c.child("v").map(Element::text)
            } else {
                None
            }
        });
        let mut other = c.attrs.clone();
        for name in ["r", "s", "t"] {
            other.remove(name);
        }
        Ok(
            json!({"text":string,"value":if item.is_some(){None}else{c.child("v").map(Element::text)},
            "kind":if item.is_some(){"string"}else{c.attrs.get("t").map(String::as_str).unwrap_or("n")},
            "formula":c.child("f").map(styles::canonical),"richText":if rich{item.map(styles::canonical)}else{None},"attributes":other,
            "extensions":c.elements().filter(|e|!["f","v","is"].contains(&e.local_name())).map(styles::canonical).collect::<Vec<_>>()}),
        )
    }
    pub fn inspect(&self, ranges: &[String], max: usize) -> Result<Value> {
        let mut cells = Vec::new();
        let mut cache = BTreeMap::new();
        let mut total = 0usize;
        let mut sheet_info = BTreeMap::new();
        for range in ranges {
            let (name, area) = range
                .rsplit_once('!')
                .ok_or_else(|| invalid("selector must be Sheet!A1:B2"))?;
            let name = if name.starts_with('\'') && name.ends_with('\'') {
                name[1..name.len() - 1].replace("''", "'")
            } else {
                name.to_owned()
            };
            let s = self.sheet(&name)?;
            let area = Area::parse(area)?;
            total += area.len();
            if total > 10000 {
                return Err(invalid("inspect target budget exceeds 10000 cells"));
            }
            if !sheet_info.contains_key(&name) {
                sheet_info.insert(name.clone(), layout_summary(&s.layout()?));
            }
            for cell in area.iter_cells().take(max.saturating_sub(cells.len())) {
                cells.push(self.describe(&name, &cell, &mut cache)?);
            }
        }
        Ok(
            json!({"cells":cells,"totalCells":total,"truncated":total>max,"sheets":sheet_info,"styleContext":self.context,"styleComparison":"resolved stored style records, including inherited base; not rendered formatting"}),
        )
    }
    pub fn diff(&self, other: &Self, max: usize) -> Result<Value> {
        let mut total = 0usize;
        let mut detail = Vec::new();
        let mut structures = Vec::new();
        let mut before_styles = BTreeMap::new();
        let mut after_styles = BTreeMap::new();
        let names: BTreeSet<_> = self.sheets.keys().chain(other.sheets.keys()).collect();
        for name in names {
            let before = self.sheets.get(name);
            let after = other.sheets.get(name);
            let cells: BTreeSet<_> = before
                .into_iter()
                .flat_map(|s| s.cells.keys())
                .chain(after.into_iter().flat_map(|s| s.cells.keys()))
                .collect();
            for cell in cells {
                let bc = before.map(|s| s.cell(cell)).transpose()?.flatten();
                let ac = after.map(|s| s.cell(cell)).transpose()?.flatten();
                let mut bv = bc.map(|c| self.value(c)).transpose()?;
                let mut av = ac.map(|c| other.value(c)).transpose()?;
                let bs = if bc.is_some() {
                    Some(self.resolved_style(before.unwrap().style_id(cell)?, &mut before_styles)?)
                } else {
                    None
                };
                let ast = if ac.is_some() {
                    Some(other.resolved_style(after.unwrap().style_id(cell)?, &mut after_styles)?)
                } else {
                    None
                };
                if bv != av || bs != ast {
                    total += 1;
                    if detail.len() < max {
                        // Most cells are unchanged. Only materialize the large
                        // resolved style trees for details actually returned.
                        if let (Some(v), Some(style)) = (&mut bv, bs) {
                            v["style"] = style.clone();
                        }
                        if let (Some(v), Some(style)) = (&mut av, ast) {
                            v["style"] = style.clone();
                        }
                        detail.push(json!({"sheet":name,"cell":cell,"before":bv,"after":av}));
                    }
                }
            }
            let bl = before.map(Sheet::layout).transpose()?;
            let al = after.map(Sheet::layout).transpose()?;
            if bl != al {
                structures.push(json!({"sheet":name,"before":bl.as_ref().map(layout_summary),"after":al.as_ref().map(layout_summary)}));
            }
        }
        let wb_before = workbook_structure(&self.workbook)?;
        let wb_after = workbook_structure(&other.workbook)?;
        Ok(
            json!({"cellChanges":{"total":total,"details":detail,"truncated":total>max},
            "sheetStructureChanges":{"total":structures.len(),"details":structures.into_iter().take(max).collect::<Vec<_>>()},
            "workbookStructureChanged":wb_before!=wb_after,"styleContextChanged":self.context!=other.context,
            "styleComparison":"resolved stored components; style indexes alone are not compared"}),
        )
    }
    fn resolved_style<'a>(
        &self,
        id: usize,
        cache: &'a mut BTreeMap<usize, Value>,
    ) -> Result<&'a Value> {
        use std::collections::btree_map::Entry;
        Ok(match cache.entry(id) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => e.insert(styles::resolved(&self.styles, id)?),
        })
    }
    pub fn apply(&mut self, op: &Operation) -> Result<usize> {
        if self.signed {
            return Err(invalid(
                "digitally signed workbooks require native handling",
            ));
        }
        let name = op.sheet();
        let sheet = self.sheet(name)?;
        if sheet
            .doc
            .root()
            .map_err(invalid)?
            .child("sheetProtection")
            .is_some()
        {
            return Err(invalid("protected worksheet requires native application"));
        }
        let part = sheet.part.clone();
        let mut styles_changed = false;
        let count = match op {
            Operation::SetText {
                cell,
                expected_text,
                value,
                ..
            } => {
                let area = Area::parse(cell)?;
                if area.len() != 1 {
                    return Err(invalid("setText requires one cell"));
                }
                sheet.check_merge(&area)?;
                let c = sheet
                    .cell(cell)?
                    .ok_or_else(|| invalid("setText target must be an existing cell"))?;
                if c.child("f").is_some() {
                    return Err(invalid("formula cells cannot be edited as plain text"));
                }
                let item = self.string_item(c)?;
                if item.is_some_and(|i| i.elements().any(|e| e.local_name() != "t")) {
                    return Err(invalid(
                        "rich text or annotated string requires native application",
                    ));
                }
                let old = self.value(c)?;
                let old = old["text"]
                    .as_str()
                    .or_else(|| {
                        if c.child("v").is_none() {
                            Some("")
                        } else {
                            None
                        }
                    })
                    .ok_or_else(|| invalid("setText only accepts plain string or blank cells"))?;
                if old != expected_text {
                    return Err(Error::Request(format!(
                        "expectedText does not match {name}!{cell}"
                    )));
                }
                if value.encode_utf16().count() > 32767 || value.chars().any(|c| !xml_char(c)) {
                    return Err(invalid(
                        "text exceeds Excel length or contains illegal XML characters",
                    ));
                }
                // OOXML reserves _xHHHH_ escape sequences. Encoding them correctly
                // needs a separate text codec, so refuse ambiguous targets in v1.
                if has_excel_escape(value) || has_excel_escape(old) {
                    return Err(invalid("OOXML escaped text requires native application"));
                }
                let c = self.sheets.get_mut(name).unwrap().cell_mut(cell)?;
                c.attrs.insert("t".into(), "inlineStr".into());
                c.children.retain(
                    |n| !matches!(n,Node::Element(e) if ["v","is"].contains(&e.local_name())),
                );
                let mut is = styles::make(c, "is");
                let mut t = styles::make(c, "t");
                t.attrs.insert("xml:space".into(), "preserve".into());
                t.children.push(Node::Text(value.clone()));
                is.children.push(Node::Element(t));
                let at = c
                    .children
                    .iter()
                    .position(|n| matches!(n,Node::Element(e) if e.local_name()=="extLst"))
                    .unwrap_or(c.children.len());
                c.children.insert(at, Node::Element(is));
                1
            }
            Operation::SetStyle { range, style, .. } => {
                let area = Area::parse(range)?;
                sheet.check_merge(&area)?;
                let cells = area.cells();
                let mut ids = Vec::new();
                for cell in &cells {
                    let c = sheet
                        .cell(cell)?
                        .ok_or_else(|| invalid(format!("style target {cell} must exist")))?;
                    if (style.font_color.is_some()
                        || style.bold.is_some()
                        || style.strike.is_some())
                        && self.string_item(c)?.is_some_and(|i| i.child("r").is_some())
                    {
                        return Err(invalid(
                            "font edits on rich-text cells require native application",
                        ));
                    }
                    ids.push(sheet.style_id(cell)?);
                }
                let mut cache = BTreeMap::new();
                let mut assigned = Vec::new();
                for id in ids {
                    let id = if let Some(new) = cache.get(&id) {
                        *new
                    } else {
                        let new =
                            styles::edit(&mut self.styles, &mut self.append_cache, id, style)?;
                        cache.insert(id, new);
                        new
                    };
                    assigned.push(id);
                }
                for (cell, id) in cells.iter().zip(assigned) {
                    self.sheets
                        .get_mut(name)
                        .unwrap()
                        .cell_mut(cell)?
                        .attrs
                        .insert("s".into(), id.to_string());
                }
                styles_changed = true;
                cells.len()
            }
            Operation::CopyStyle {
                range,
                from_cell,
                components,
                ..
            } => {
                let area = Area::parse(range)?;
                sheet.check_merge(&area)?;
                sheet
                    .cell(from_cell)?
                    .ok_or_else(|| invalid("style donor must exist"))?;
                let donor = sheet.style_id(from_cell)?;
                let cells = area.cells();
                let mut ids = Vec::new();
                for cell in &cells {
                    let c = sheet
                        .cell(cell)?
                        .ok_or_else(|| invalid("style target must exist"))?;
                    if components.iter().any(|x| x == "font")
                        && self.string_item(c)?.is_some_and(|i| i.child("r").is_some())
                    {
                        return Err(invalid(
                            "font copy onto rich-text requires native application",
                        ));
                    }
                    ids.push(sheet.style_id(cell)?);
                }
                let mut cache = BTreeMap::new();
                let mut assigned = Vec::new();
                for id in ids {
                    let id = if let Some(new) = cache.get(&id) {
                        *new
                    } else {
                        let new = styles::copy(
                            &mut self.styles,
                            &mut self.append_cache,
                            id,
                            donor,
                            components,
                        )?;
                        cache.insert(id, new);
                        new
                    };
                    assigned.push(id);
                }
                for (cell, id) in cells.iter().zip(assigned) {
                    self.sheets
                        .get_mut(name)
                        .unwrap()
                        .cell_mut(cell)?
                        .attrs
                        .insert("s".into(), id.to_string());
                }
                styles_changed = true;
                cells.len()
            }
            Operation::RowHeight { row, height, .. } => {
                if !(0.0..=409.0).contains(height) {
                    return Err(invalid("row height must be 0-409 points"));
                }
                let r = self.sheets.get_mut(name).unwrap().row_mut(*row)?;
                r.attrs.insert("ht".into(), height.to_string());
                r.attrs.insert("customHeight".into(), "1".into());
                1
            }
            Operation::RowVisibility { row, hidden, .. } => {
                self.sheets
                    .get_mut(name)
                    .unwrap()
                    .row_mut(*row)?
                    .attrs
                    .insert("hidden".into(), if *hidden { "1" } else { "0" }.into());
                1
            }
            Operation::ColumnWidth { column, width, .. } => {
                let col = column_number(column)?;
                if !(0.0..=255.0).contains(width) {
                    return Err(invalid("column width must be 0-255 character units"));
                }
                self.sheets.get_mut(name).unwrap().set_column(col, *width)?;
                1
            }
        };
        self.dirty.insert(part);
        if styles_changed {
            self.dirty.insert(self.style_path.clone());
        }
        Ok(count)
    }
    pub fn updates(&self, source: &Package) -> Result<BTreeMap<String, Vec<u8>>> {
        let mut result = BTreeMap::new();
        for part in &self.dirty {
            let doc = if part == &self.style_path {
                &self.styles
            } else {
                &self
                    .sheets
                    .values()
                    .find(|s| &s.part == part)
                    .ok_or_else(|| invalid("dirty sheet missing"))?
                    .doc
            };
            let bytes = doc.serialize().into_bytes();
            xml::parse(std::str::from_utf8(&bytes).unwrap()).map_err(invalid)?;
            if source.parts.get(part) != Some(&bytes) {
                result.insert(part.clone(), bytes);
            }
        }
        Ok(result)
    }
}
fn string_text(item: &Element) -> String {
    item.elements()
        .filter_map(|e| match e.local_name() {
            "t" => Some(e.text()),
            "r" => e.child("t").map(Element::text),
            _ => None,
        })
        .collect()
}
fn has_excel_escape(s: &str) -> bool {
    s.as_bytes().windows(7).any(|w| {
        w[0] == b'_' && w[1] == b'x' && w[6] == b'_' && w[2..6].iter().all(u8::is_ascii_hexdigit)
    })
}
fn xml_char(c: char) -> bool {
    c == '\t'
        || c == '\n'
        || c == '\r'
        || ('\u{20}'..='\u{d7ff}').contains(&c)
        || ('\u{e000}'..='\u{fffd}').contains(&c)
        || c >= '\u{10000}'
}
fn workbook_structure(d: &Document) -> Result<Value> {
    Ok(styles::canonical(d.root().map_err(invalid)?))
}
fn layout_summary(v: &Value) -> Value {
    json!({"sha256":super::package::sha(v.to_string().as_bytes()),"rowRecords":v["rows"].as_object().map_or(0,|x|x.len()),
        "properties":v["structure"].as_array().map(|a|a.iter().map(|x|json!({"name":x["name"],"sha256":super::package::sha(x.to_string().as_bytes())})).collect::<Vec<_>>()),
        "detail":"Selected cell row/column/merge attributes appear in inspect.cells; structure fingerprints include all stored sheet metadata."})
}
impl Sheet {
    fn root(&self) -> Result<&Element> {
        self.doc.root().map_err(invalid)
    }
    fn data(&self) -> Result<&Element> {
        self.root()?
            .child("sheetData")
            .ok_or_else(|| invalid("sheetData missing"))
    }
    fn new(part: String, doc: Document) -> Result<Self> {
        let root = doc.root().map_err(invalid)?;
        let data = root
            .child("sheetData")
            .ok_or_else(|| invalid("sheetData missing"))?;
        let mut rows = BTreeMap::new();
        let mut cells = BTreeMap::new();
        let mut last_row = 0;
        for (row_pos, node) in data.children.iter().enumerate() {
            let Node::Element(row) = node else { continue };
            if row.local_name() != "row" {
                return Err(invalid("unsupported sheetData child"));
            }
            let r = attr_u32(row, "r", 0)?;
            if r <= last_row || r > 1048576 {
                return Err(invalid("rows must have unique ordered explicit indexes"));
            }
            rows.insert(r, row_pos);
            last_row = r;
            let mut last_col = 0;
            for (cell_pos, node) in row.children.iter().enumerate() {
                let Node::Element(cell) = node else { continue };
                if cell.local_name() != "c" {
                    continue;
                }
                let reference = cell
                    .attrs
                    .get("r")
                    .ok_or_else(|| invalid("cell reference missing"))?;
                let pos = address(reference)?;
                if pos.1 != r || pos.0 <= last_col {
                    return Err(invalid(
                        "cells must have unique ordered explicit references",
                    ));
                }
                last_col = pos.0;
                cells.insert(reference.clone(), (row_pos, cell_pos));
                if cells.len() > MAX_STORED_CELLS {
                    return Err(invalid("worksheet exceeds 200000 stored cells"));
                }
            }
        }
        if let Some(cols) = root.child("cols") {
            let mut last = 0;
            for c in cols.elements() {
                let a = attr_u32(c, "min", 0)?;
                let b = attr_u32(c, "max", 0)?;
                if a == 0 || a <= last || a > b || b > 16384 {
                    return Err(invalid("overlapping/unsorted/invalid column spans"));
                }
                last = b;
            }
        }
        let merged = root.child("mergeCells").map_or(Ok(vec![]), |m| {
            m.elements()
                .map(|e| {
                    Area::parse_unbounded(
                        e.attrs
                            .get("ref")
                            .ok_or_else(|| invalid("merge reference missing"))?,
                    )
                })
                .collect::<Result<Vec<_>>>()
        })?;
        Ok(Self {
            part,
            doc,
            rows,
            cells,
            merged,
        })
    }
    fn row(&self, n: u32) -> Result<Option<&Element>> {
        if n == 0 || n > 1048576 {
            return Err(invalid("row outside Excel bounds"));
        }
        self.rows
            .get(&n)
            .map(|i| element_at(self.data()?, *i))
            .transpose()
    }
    fn row_mut(&mut self, n: u32) -> Result<&mut Element> {
        if n == 0 || n > 1048576 {
            return Err(invalid("row outside Excel bounds"));
        }
        let pos = *self
            .rows
            .get(&n)
            .ok_or_else(|| invalid("target row must already exist"))?;
        let data = self
            .doc
            .root_mut()
            .map_err(invalid)?
            .child_mut("sheetData")
            .ok_or_else(|| invalid("sheetData missing"))?;
        element_at_mut(data, pos)
    }
    fn cell(&self, cell: &str) -> Result<Option<&Element>> {
        address(cell)?;
        self.cells
            .get(cell)
            .map(|(r, c)| element_at(element_at(self.data()?, *r)?, *c))
            .transpose()
    }
    fn cell_mut(&mut self, cell: &str) -> Result<&mut Element> {
        address(cell)?;
        let (r, c) = *self
            .cells
            .get(cell)
            .ok_or_else(|| invalid("target cell must already exist"))?;
        let data = self
            .doc
            .root_mut()
            .map_err(invalid)?
            .child_mut("sheetData")
            .ok_or_else(|| invalid("sheetData missing"))?;
        element_at_mut(element_at_mut(data, r)?, c)
    }
    fn column(&self, n: u32) -> Result<Option<&Element>> {
        let Some(cols) = self.root()?.child("cols") else {
            return Ok(None);
        };
        for c in cols.elements() {
            if attr_u32(c, "min", 0)? <= n && n <= attr_u32(c, "max", 0)? {
                return Ok(Some(c));
            }
        }
        Ok(None)
    }
    fn style_id(&self, cell: &str) -> Result<usize> {
        let p = address(cell)?;
        if let Some(c) = self.cell(cell)?
            && c.attrs.contains_key("s")
        {
            return Ok(attr_u32(c, "s", 0)? as usize);
        }
        if let Some(r) = self.row(p.1)?
            && r.attrs
                .get("customFormat")
                .is_some_and(|s| s == "1" || s == "true")
        {
            return Ok(attr_u32(r, "s", 0)? as usize);
        }
        Ok(self
            .column(p.0)?
            .map(|c| attr_u32(c, "style", 0))
            .transpose()?
            .unwrap_or(0) as usize)
    }
    fn check_merge(&self, area: &Area) -> Result<()> {
        for merge in &self.merged {
            if merge.intersects(area) && !area.contains((merge.left, merge.top)) {
                return Err(invalid(
                    "target is a covered merged cell; select its anchor",
                ));
            }
            if merge.intersects(area)
                && area.len() > 1
                && !(area.contains((merge.left, merge.top))
                    && area.contains((merge.right, merge.bottom)))
            {
                return Err(invalid("range partially intersects merged cells"));
            }
        }
        Ok(())
    }
    fn layout(&self) -> Result<Value> {
        let root = self.root()?;
        let mut rows = BTreeMap::new();
        for r in self.data()?.elements() {
            let mut attrs = r.attrs.clone();
            attrs.remove("r");
            if !attrs.is_empty() {
                rows.insert(r.attrs.get("r").cloned().unwrap_or_default(), attrs);
            }
        }
        Ok(
            json!({"rows":rows,"worksheetAttributes":root.attrs,"structure":root.elements().filter(|e|e.local_name()!="sheetData").map(styles::canonical).collect::<Vec<_>>()}),
        )
    }
    fn set_column(&mut self, n: u32, width: f64) -> Result<()> {
        let root = self.doc.root_mut().map_err(invalid)?;
        if root.child("cols").is_none() {
            let new = styles::make(root, "cols");
            let at = root
                .children
                .iter()
                .position(|x| matches!(x,Node::Element(e) if e.local_name()=="sheetData"))
                .ok_or_else(|| invalid("sheetData missing"))?;
            root.children.insert(at, Node::Element(new));
        }
        let cols = root.child_mut("cols").unwrap();
        let mut records = Vec::new();
        let mut found = false;
        for c in cols.elements() {
            let a = attr_u32(c, "min", 0)?;
            let b = attr_u32(c, "max", 0)?;
            if a <= n && n <= b {
                if a < n {
                    let mut left = c.clone();
                    left.attrs.insert("max".into(), (n - 1).to_string());
                    records.push(left);
                }
                let mut mid = c.clone();
                mid.attrs.insert("min".into(), n.to_string());
                mid.attrs.insert("max".into(), n.to_string());
                mid.attrs.insert("width".into(), width.to_string());
                mid.attrs.insert("customWidth".into(), "1".into());
                records.push(mid);
                found = true;
                if b > n {
                    let mut right = c.clone();
                    right.attrs.insert("min".into(), (n + 1).to_string());
                    records.push(right);
                }
            } else {
                records.push(c.clone());
            }
        }
        if !found {
            let mut c = styles::make(cols, "col");
            for (k, v) in [
                ("min", n.to_string()),
                ("max", n.to_string()),
                ("width", width.to_string()),
                ("customWidth", "1".into()),
            ] {
                c.attrs.insert(k.into(), v);
            }
            records.push(c);
        }
        records.sort_by_key(|c| c.attrs["min"].parse::<u32>().unwrap_or(0));
        cols.children = records.into_iter().map(Node::Element).collect();
        Ok(())
    }
}
fn element_at(parent: &Element, index: usize) -> Result<&Element> {
    match parent.children.get(index) {
        Some(Node::Element(e)) => Ok(e),
        _ => Err(invalid("worksheet index is inconsistent")),
    }
}
fn element_at_mut(parent: &mut Element, index: usize) -> Result<&mut Element> {
    match parent.children.get_mut(index) {
        Some(Node::Element(e)) => Ok(e),
        _ => Err(invalid("worksheet index is inconsistent")),
    }
}
#[derive(Clone, Copy)]
struct Area {
    left: u32,
    top: u32,
    right: u32,
    bottom: u32,
}
impl Area {
    fn parse(s: &str) -> Result<Self> {
        let a = Self::parse_unbounded(s)?;
        if a.len() > 10000 {
            return Err(invalid("range exceeds 10000 cells"));
        }
        Ok(a)
    }
    fn parse_unbounded(s: &str) -> Result<Self> {
        let (a, b) = s.split_once(':').map_or((s, s), |(a, b)| (a, b));
        let (left, top) = address(a)?;
        let (right, bottom) = address(b)?;
        if left > right || top > bottom {
            return Err(invalid("range must run top-left to bottom-right"));
        }
        Ok(Self {
            left,
            top,
            right,
            bottom,
        })
    }
    fn len(self) -> usize {
        (self.right - self.left + 1) as usize * (self.bottom - self.top + 1) as usize
    }
    fn contains(self, (c, r): (u32, u32)) -> bool {
        c >= self.left && c <= self.right && r >= self.top && r <= self.bottom
    }
    fn intersects(self, o: &Self) -> bool {
        self.left <= o.right && o.left <= self.right && self.top <= o.bottom && o.top <= self.bottom
    }
    fn reference(self) -> String {
        format!(
            "{}{}:{}{}",
            column_name(self.left),
            self.top,
            column_name(self.right),
            self.bottom
        )
    }
    fn iter_cells(self) -> impl Iterator<Item = String> {
        (self.top..=self.bottom).flat_map(move |r| {
            (self.left..=self.right).map(move |c| format!("{}{r}", column_name(c)))
        })
    }
    fn cells(self) -> Vec<String> {
        self.iter_cells().collect()
    }
}
fn address(s: &str) -> Result<(u32, u32)> {
    let split = s
        .bytes()
        .position(|b| !b.is_ascii_uppercase())
        .ok_or_else(|| invalid("cell requires row number"))?;
    let c = column_number(&s[..split])?;
    let row = &s[split..];
    if row.is_empty() || row.starts_with('0') || !row.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid("invalid cell reference"));
    }
    let r = row
        .parse::<u32>()
        .map_err(|_| invalid("invalid cell row"))?;
    if r == 0 || r > 1048576 {
        return Err(invalid("cell outside Excel row bounds"));
    }
    Ok((c, r))
}
fn column_number(s: &str) -> Result<u32> {
    if s.is_empty() || s.len() > 3 || !s.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(invalid("column must be A through XFD"));
    }
    let n = s.bytes().fold(0, |n, b| n * 26 + (b - b'A' + 1) as u32);
    if n > 16384 {
        return Err(invalid("column beyond XFD"));
    }
    Ok(n)
}
fn column_name(mut n: u32) -> String {
    let mut s = Vec::new();
    while n > 0 {
        n -= 1;
        s.push((b'A' + (n % 26) as u8) as char);
        n /= 26;
    }
    s.into_iter().rev().collect()
}
