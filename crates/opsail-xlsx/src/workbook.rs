use super::{
    Error, Operation, Result, Style, invalid,
    package::Package,
    styles,
    xml::{self, Document, Element, Node},
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
#[path = "insert_rows.rs"]
mod insert_rows;
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
    ancillary: BTreeMap<String, Vec<u8>>,
    ancillary_updates: BTreeMap<String, Document>,
}
#[derive(Clone)]
struct Sheet {
    part: String,
    doc: Document,
    // Physical XML child positions; insertion shifts the affected indexes.
    touched_rows: BTreeSet<u32>,
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
        let shared = if let Some(path) = &shared_path {
            let d = pkg.xml(path)?;
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
        let ancillary = pkg
            .parts
            .iter()
            .filter(|(p, _)| {
                (p.ends_with(".xml") || p.ends_with(".rels") || p.ends_with(".vml"))
                    && *p != "xl/workbook.xml"
                    && *p != &style_path
                    && Some(p.as_str()) != shared_path.as_deref()
                    && !sheets.values().any(|s| &s.part == *p)
            })
            .map(|(p, b)| (p.clone(), b.clone()))
            .collect();
        Ok(Self {
            ancillary,
            ancillary_updates: BTreeMap::new(),
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
    fn describe_compact(
        &self,
        name: &str,
        cell: &str,
        cache: &mut BTreeMap<usize, Value>,
    ) -> Result<Value> {
        let sheet = self.sheet(name)?;
        let pos = address(cell)?;
        let c = sheet.cell(cell)?;
        let id = sheet.style_id(cell)?;
        if let std::collections::btree_map::Entry::Vacant(entry) = cache.entry(id) {
            entry.insert(styles::compact(&self.styles, id)?);
        }
        let mut result = cache[&id].clone();
        // Avoid constructing canonical rich-string trees in compact mode.
        let item = c.map(|c| self.string_item(c)).transpose()?.flatten();
        let row = sheet.row(pos.1)?;
        let column = sheet.column(pos.0)?;
        let number = |e: Option<&Element>, key: &str| {
            e.and_then(|e| e.attrs.get(key))
                .and_then(|v| v.parse::<f64>().ok())
        };
        let value = c.and_then(|c| c.child("v")).map(Element::text);
        result["sheet"] = json!(name);
        result["cell"] = json!(cell);
        result["blank"] = json!(c.is_none());
        result["kind"] = json!(if item.is_some() {
            "string"
        } else {
            c.map_or("blank", |c| c.attrs.get("t").map_or("n", String::as_str))
        });
        result["text"] = json!(item.map(string_text).or_else(|| {
            c.filter(|c| c.attrs.get("t").is_some_and(|t| t == "str"))
                .and(value.clone())
        }));
        result["value"] = if result["text"].is_null() {
            json!(value)
        } else {
            Value::Null
        };
        result["formula"] = json!(c.is_some_and(|c| c.child("f").is_some()));
        result["richText"] = json!(item.is_some_and(|i| i.child("r").is_some()));
        result["merge"] = json!(
            sheet
                .merged
                .iter()
                .find(|m| m.contains(pos))
                .map(|m| m.reference())
        );
        result["row"] = json!({"height":number(row,"ht"),"customHeight":row.is_some_and(|r|r.attrs.get("customHeight").is_some_and(|v|v=="1"||v=="true"))});
        result["column"] = json!({"width":number(column,"width")});
        let defaults = sheet.root()?.child("sheetFormatPr");
        if result["row"]["customHeight"] == false
            && number(row, "ht") == number(defaults, "defaultRowHeight")
        {
            result["row"].as_object_mut().unwrap().remove("height");
        }
        if number(column, "width") == number(defaults, "defaultColWidth") {
            result["column"].as_object_mut().unwrap().remove("width");
        }
        styles::omit_defaults(&mut result);
        Ok(result)
    }
    fn editable_text(&self, cell: Option<&Element>, numeric: bool) -> Result<String> {
        let Some(c) = cell else {
            return Ok(String::new());
        };
        if c.child("f").is_some() {
            return Err(invalid("formula cells cannot be edited"));
        }
        let item = self.string_item(c)?;
        if item.is_some_and(|i| i.elements().any(|e| e.local_name() != "t")) {
            return Err(invalid(
                "rich text or annotated string requires native application",
            ));
        }
        if let Some(item) = item {
            return Ok(string_text(item));
        }
        let kind = c.attrs.get("t").map(String::as_str).unwrap_or("n");
        if let Some(v) = c.child("v") {
            if kind == "str" || numeric && kind == "n" {
                return Ok(v.text());
            }
            return Err(invalid(
                "operation only accepts plain string or blank cells (setNumber also accepts numeric cells)",
            ));
        }
        Ok(String::new())
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
    pub fn inspect(&self, ranges: &[String], max: usize, compact: bool) -> Result<Value> {
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
                cells.push(if compact {
                    self.describe_compact(&name, &cell, &mut cache)?
                } else {
                    self.describe(&name, &cell, &mut cache)?
                });
            }
        }
        let mut report =
            json!({"cells":cells,"totalCells":total,"truncated":total>max,"sheets":sheet_info});
        if !compact {
            report["styleContext"] = self.context.clone();
            report["styleComparison"] = json!(
                "resolved stored style records, including inherited base; not rendered formatting"
            );
        }
        Ok(report)
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
    /// Failed validation operations restore all mutable state, including append
    /// caches and row/cell indexes. Normal patch failures never publish a book.
    pub fn apply_bounded(
        &mut self,
        op: &Operation,
        remaining: usize,
        rollback: bool,
    ) -> Result<usize> {
        let name = op.sheet();
        let sheet = self.sheet(name)?;
        let targets = match op {
            Operation::SetText { cell, .. }
            | Operation::SetNumber { cell, .. }
            | Operation::AppendText { cell, .. }
            | Operation::SetFormula { cell, .. } => Some(Area::parse(cell)?),
            Operation::SetStyle { range, .. } | Operation::CopyStyle { range, .. } => {
                Some(Area::parse(range)?)
            }
            _ => None,
        };
        let count = match op {
            Operation::InsertRows { count, .. } => *count as usize,
            _ => targets.map_or(1, Area::len),
        };
        if count > remaining {
            return Err(Error::Request(
                "patch target budget exceeds 10000 cells/rows/columns".into(),
            ));
        }
        if let Some(area) = targets {
            let created = area
                .iter_cells()
                .filter(|c| !sheet.cells.contains_key(c))
                .count();
            if self.sheets.values().map(|s| s.cells.len()).sum::<usize>() + created
                > MAX_STORED_CELLS
            {
                return Err(invalid(
                    "workbook exceeds 200000 stored cells for edit/diff capability",
                ));
            }
            for cell in area.iter_cells().filter(|c| !sheet.cells.contains_key(c)) {
                sheet.check_merge(&Area::parse(&cell)?)?;
            }
        }
        // insertRows stages every affected part and commits only on success.
        if matches!(op, Operation::InsertRows { .. }) {
            return self.apply(op);
        }
        let saved_sheet = rollback.then(|| sheet.clone());
        let saved_styles = (rollback
            && matches!(op, Operation::SetStyle { .. } | Operation::CopyStyle { .. }))
        .then(|| (self.styles.clone(), self.append_cache.clone()));
        let saved_workbook =
            (rollback && matches!(op, Operation::SetFormula { .. })).then(|| self.workbook.clone());
        let result = self.apply(op);
        if result.is_err() {
            if let Some(sheet) = saved_sheet {
                self.sheets.insert(name.into(), sheet);
            }
            if let Some((styles, cache)) = saved_styles {
                self.styles = styles;
                self.append_cache = cache;
            }
            if let Some(workbook) = saved_workbook {
                self.workbook = workbook;
            }
            // Dirty parts are marked only after a successful operation.
        }
        result
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
        let styles_revision = self.append_cache.revision();
        let count = match op {
            Operation::InsertRows {
                before,
                count,
                style_from,
                ..
            } => {
                return self.insert_rows(name, *before, *count, style_from);
            }
            Operation::SetFormula {
                cell,
                expected_text,
                formula,
                ..
            } => {
                address(cell)?;
                sheet.check_merge(&Area::parse(cell)?)?;
                let formula = plain_formula(formula)?;
                let old = if let Some(c) = sheet.cell(cell)? {
                    if let Some(f) = c.child("f") {
                        if f.attrs.get("t").is_some_and(|t| t != "normal") {
                            return Err(invalid(
                                "shared/array formulas require native application (including dataTable formulas)",
                            ));
                        }
                        format!("={}", f.text())
                    } else if c.attrs.get("t").is_some_and(|t| t == "b") {
                        c.child("v").map(Element::text).unwrap_or_default()
                    } else {
                        self.editable_text(Some(c), true)?
                    }
                } else {
                    String::new()
                };
                if has_excel_escape(&old) {
                    return Err(invalid("OOXML escaped text requires native application"));
                }
                if &old != expected_text {
                    return Err(Error::Request(format!(
                        "expectedText does not match {name}!{cell}"
                    )));
                }
                let c = self.sheets.get_mut(name).unwrap().cell_mut(cell)?;
                c.attrs.remove("t");
                c.children
                    .retain(|n| !matches!(n, Node::Element(e) if e.local_name() == "f"));
                let mut f = styles::make(c, "f");
                f.children.push(Node::Text(formula.into()));
                replace_value(c, f);
                // Mark the workbook only after the cell write succeeds. A prior
                // insertRows may already have dirtied definedNames, so the dirty
                // flag alone cannot prove that recalculation was requested.
                request_recalculation(&mut self.workbook)?;
                1
            }
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
                let old = self.editable_text(sheet.cell(cell)?, false)?;
                if &old != expected_text {
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
                if has_excel_escape(value) || has_excel_escape(&old) {
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
            Operation::SetNumber {
                cell,
                expected_text,
                value,
                ..
            } => {
                address(cell)?;
                sheet.check_merge(&Area::parse(cell)?)?;
                if !value.is_finite() {
                    return Err(invalid("setNumber requires a finite number"));
                }
                let old = self.editable_text(sheet.cell(cell)?, true)?;
                if &old != expected_text {
                    return Err(Error::Request(format!(
                        "expectedText does not match {name}!{cell}"
                    )));
                }
                if has_excel_escape(&old) {
                    return Err(invalid("OOXML escaped text requires native application"));
                }
                let c = self.sheets.get_mut(name).unwrap().cell_mut(cell)?;
                c.attrs.remove("t");
                let mut v = styles::make(c, "v");
                v.children.push(Node::Text(value.to_string()));
                replace_value(c, v);
                1
            }
            Operation::AppendText {
                cell,
                expected_text,
                value,
                font_color,
                bold,
                strike,
                ..
            } => {
                address(cell)?;
                sheet.check_merge(&Area::parse(cell)?)?;
                let old = self.editable_text(sheet.cell(cell)?, false)?;
                if &old != expected_text {
                    return Err(Error::Request(format!(
                        "expectedText does not match {name}!{cell}"
                    )));
                }
                if value.is_empty() {
                    return Err(invalid("appendText value must not be empty"));
                }
                if old.encode_utf16().count() + value.encode_utf16().count() > 32767
                    || value.chars().any(|c| !xml_char(c))
                {
                    return Err(invalid(
                        "text exceeds Excel length or contains illegal XML characters",
                    ));
                }
                if has_excel_escape(value) || has_excel_escape(&old) {
                    return Err(invalid("OOXML escaped text requires native application"));
                }
                let id = sheet.style_id(cell)?;
                let parent = sheet.root()?;
                let mut inline = styles::make(parent, "is");
                for (text, overrides) in [
                    (&old, Style::default()),
                    (
                        value,
                        Style {
                            font_color: font_color.clone(),
                            bold: *bold,
                            strike: *strike,
                            ..Style::default()
                        },
                    ),
                ] {
                    if text.is_empty() {
                        continue;
                    }
                    let mut run = styles::make(parent, "r");
                    run.children.push(Node::Element(styles::run_properties(
                        &self.styles,
                        id,
                        parent,
                        &overrides,
                    )?));
                    let mut t = styles::make(parent, "t");
                    t.attrs.insert("xml:space".into(), "preserve".into());
                    t.children.push(Node::Text(text.clone()));
                    run.children.push(Node::Element(t));
                    inline.children.push(Node::Element(run));
                }
                let c = self.sheets.get_mut(name).unwrap().cell_mut(cell)?;
                c.attrs.insert("t".into(), "inlineStr".into());
                replace_value(c, inline);
                1
            }
            Operation::SetStyle { range, style, .. } => {
                let area = Area::parse(range)?;
                sheet.check_merge(&area)?;
                let cells = area.cells();
                let mut ids = Vec::new();
                for cell in &cells {
                    let c = sheet.cell(cell)?;
                    if (style.font_color.is_some()
                        || style.bold.is_some()
                        || style.strike.is_some())
                        && c.map(|c| self.string_item(c))
                            .transpose()?
                            .flatten()
                            .is_some_and(|i| i.child("r").is_some())
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
                    let c = sheet.cell(cell)?;
                    if components.iter().any(|x| x == "font")
                        && c.map(|c| self.string_item(c))
                            .transpose()?
                            .flatten()
                            .is_some_and(|i| i.child("r").is_some())
                    {
                        return Err(invalid(
                            "font copy onto rich-text requires native application",
                        ));
                    }
                    ids.push(sheet.style_id(cell)?);
                }
                let mut cache = BTreeMap::new();
                let mut assigned = Vec::new();
                for (cell, id) in cells.iter().zip(ids) {
                    let id = if let Some(new) = cache.get(&id) {
                        *new
                    } else {
                        let new = styles::copy(
                            &mut self.styles,
                            &mut self.append_cache,
                            id,
                            donor,
                            components,
                        )
                        .map_err(|e| {
                            invalid(format!(
                                "target {name}!{cell}, donor {name}!{from_cell}: {e}"
                            ))
                        })?;
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
        if self.append_cache.revision() != styles_revision {
            self.dirty.insert(self.style_path.clone());
        }
        if matches!(op, Operation::SetFormula { .. }) {
            self.dirty.insert("xl/workbook.xml".into());
        }
        Ok(count)
    }
    pub fn updates(&self, source: &Package) -> Result<BTreeMap<String, Vec<u8>>> {
        let mut result = BTreeMap::new();
        for part in &self.dirty {
            let doc = if part == &self.style_path {
                &self.styles
            } else if part == "xl/workbook.xml" {
                &self.workbook
            } else if let Some(doc) = self.ancillary_updates.get(part) {
                doc
            } else {
                &self
                    .sheets
                    .values()
                    .find(|s| &s.part == part)
                    .ok_or_else(|| invalid("dirty sheet missing"))?
                    .doc
            };
            let bytes = if part == &self.style_path
                || part == "xl/workbook.xml"
                || self.ancillary_updates.contains_key(part)
            {
                doc.serialize().into_bytes()
            } else {
                let sheet = self.sheets.values().find(|s| &s.part == part).unwrap();
                let original = source
                    .parts
                    .get(part)
                    .ok_or_else(|| invalid("source sheet missing"))?;
                let raw_rows = xml::raw_rows(
                    std::str::from_utf8(original).map_err(|e| invalid(e.to_string()))?,
                )
                .map_err(invalid)?;
                let mut preserved = doc.clone();
                let data = preserved
                    .root_mut()
                    .map_err(invalid)?
                    .child_mut("sheetData")
                    .unwrap();
                for (row, position) in &sheet.rows {
                    if !sheet.touched_rows.contains(row)
                        && let Some(raw) = raw_rows.get(row)
                    {
                        data.children[*position] = Node::Raw(raw.clone());
                    }
                }
                preserved.serialize().into_bytes()
            };
            xml::parse(std::str::from_utf8(&bytes).unwrap()).map_err(invalid)?;
            if source.parts.get(part) != Some(&bytes) {
                result.insert(part.clone(), bytes);
            }
        }
        Ok(result)
    }
}
fn plain_formula(formula: &str) -> Result<&str> {
    let expression = formula.strip_prefix('=').unwrap_or(formula);
    if expression.is_empty()
        || expression.encode_utf16().count() > 8192
        || expression.chars().any(|c| !xml_char(c))
    {
        return Err(invalid(
            "formula must contain 1-8192 UTF-16 units and legal XML characters",
        ));
    }
    if has_excel_escape(expression) {
        return Err(invalid("OOXML escaped text requires native application"));
    }
    // The protocol accepts expression text, never an OOXML formula record or
    // Excel's legacy CSE {=...} wrapper. Normal array constants remain text.
    let trimmed = expression.trim();
    if trimmed.starts_with("{=") || trimmed.starts_with('<') {
        return Err(invalid(
            "shared/array formulas require native application (including dataTable formulas); supply a plain formula expression",
        ));
    }
    Ok(expression)
}
fn request_recalculation(workbook: &mut Document) -> Result<()> {
    let root = workbook.root_mut().map_err(invalid)?;
    if root.child("calcPr").is_none() {
        let calc = styles::make(root, "calcPr");
        // CT_Workbook places calcPr after definedNames and before these
        // optional successors. Existing sheets and definedNames stay in place.
        let at = root.children.iter().position(|n| matches!(n, Node::Element(e)
            if ["oleSize", "customWorkbookViews", "pivotCaches", "smartTagPr", "smartTagTypes",
                "webPublishing", "fileRecoveryPr", "webPublishObjects", "extLst"].contains(&e.local_name())))
            .unwrap_or(root.children.len());
        root.children.insert(at, Node::Element(calc));
    }
    root.child_mut("calcPr")
        .unwrap()
        .attrs
        .insert("fullCalcOnLoad".into(), "1".into());
    Ok(())
}
fn replace_value(cell: &mut Element, value: Element) {
    cell.children
        .retain(|n| !matches!(n,Node::Element(e) if ["v","is"].contains(&e.local_name())));
    let at = cell
        .children
        .iter()
        .position(|n| matches!(n,Node::Element(e) if e.local_name()=="extLst"))
        .unwrap_or(cell.children.len());
    cell.children.insert(at, Node::Element(value));
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
            touched_rows: BTreeSet::new(),
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
        if !self.rows.contains_key(&n) {
            let mut row = styles::make(self.data()?, "row");
            row.attrs.insert("r".into(), n.to_string());
            let position = self
                .rows
                .range(n..)
                .next()
                .map_or(self.data()?.children.len(), |(_, p)| *p);
            self.doc
                .root_mut()
                .map_err(invalid)?
                .child_mut("sheetData")
                .unwrap()
                .children
                .insert(position, Node::Element(row));
            for p in self.rows.values_mut() {
                if *p >= position {
                    *p += 1;
                }
            }
            for (r, _) in self.cells.values_mut() {
                if *r >= position {
                    *r += 1;
                }
            }
            self.rows.insert(n, position);
        }
        self.touched_rows.insert(n);
        let pos = self.rows[&n];
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
        let (column, row) = address(cell)?;
        if !self.cells.contains_key(cell) {
            if self.cells.len() >= MAX_STORED_CELLS {
                return Err(invalid("worksheet exceeds 200000 stored cells"));
            }
            let inherited = self.inherited_style(column, row)?;
            let mut created = styles::make(self.root()?, "c");
            created.attrs.insert("r".into(), cell.into());
            if let Some(id) = inherited {
                created.attrs.insert("s".into(), id.to_string());
            }
            self.extend_dimension(column, row)?;
            let r = self.row_mut(row)?;
            let position = r
                .children
                .iter()
                .position(|node| match node {
                    Node::Element(e) if e.local_name() == "c" => e
                        .attrs
                        .get("r")
                        .and_then(|s| address(s).ok())
                        .is_some_and(|p| p.0 > column),
                    Node::Element(e) => e.local_name() == "extLst",
                    _ => false,
                })
                .unwrap_or(r.children.len());
            r.children.insert(position, Node::Element(created));
            let row_position = self.rows[&row];
            for (r, c) in self.cells.values_mut() {
                if *r == row_position && *c >= position {
                    *c += 1;
                }
            }
            self.cells.insert(cell.into(), (row_position, position));
        }
        self.touched_rows.insert(row);
        let (r, c) = self.cells[cell];
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
    fn inherited_style(&self, column: u32, row: u32) -> Result<Option<u32>> {
        if let Some(r) = self.row(row)?
            && r.attrs
                .get("customFormat")
                .is_some_and(|v| v == "1" || v == "true")
        {
            return Ok(Some(attr_u32(r, "s", 0)?));
        }
        self.column(column)?
            .filter(|c| c.attrs.contains_key("style"))
            .map(|c| attr_u32(c, "style", 0))
            .transpose()
    }
    fn extend_dimension(&mut self, column: u32, row: u32) -> Result<()> {
        let Some(dimension) = self.doc.root_mut().map_err(invalid)?.child_mut("dimension") else {
            return Ok(());
        };
        let reference = dimension
            .attrs
            .get("ref")
            .ok_or_else(|| invalid("dimension reference missing"))?;
        let mut area = Area::parse_unbounded(reference)?;
        if !area.contains((column, row)) {
            area.left = area.left.min(column);
            area.right = area.right.max(column);
            area.top = area.top.min(row);
            area.bottom = area.bottom.max(row);
            dimension.attrs.insert("ref".into(), area.reference());
        }
        Ok(())
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

#[cfg(test)]
#[path = "protocol_tests.rs"]
mod protocol_tests;
