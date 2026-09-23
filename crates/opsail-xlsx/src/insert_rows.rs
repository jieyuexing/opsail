//! Transactional structural edits: all affected documents are staged before
//! replacing Book state, including when validateOnly collects a refusal.
use super::*;
use crate::{
    RowStyle,
    references::{MAX_ROW, Shift},
};
const DRAWING: &str = "http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing";
const CHART: &str = "http://schemas.openxmlformats.org/drawingml/2006/chart";
const VML_EXCEL: &str = "urn:schemas-microsoft-com:office:excel";
const THREADED: &str = "http://schemas.microsoft.com/office/spreadsheetml/2018/threadedcomments";

fn native(location: &str, message: impl std::fmt::Display) -> Error {
    invalid(format!(
        "{location}: {message}; requires native application"
    ))
}
fn walk(
    element: &mut Element,
    inherited: &BTreeMap<String, String>,
    parent: &str,
    action: &mut impl FnMut(&mut Element, &str, &str) -> Result<()>,
) -> Result<()> {
    let mut namespaces = inherited.clone();
    for (key, value) in &element.attrs {
        if key == "xmlns" {
            namespaces.insert(String::new(), value.clone());
        } else if let Some(prefix) = key.strip_prefix("xmlns:") {
            namespaces.insert(prefix.into(), value.clone());
        }
    }
    let prefix = element
        .name
        .rsplit_once(':')
        .map_or("", |(prefix, _)| prefix);
    let uri = namespaces.get(prefix).map_or("", String::as_str);
    action(element, uri, parent)?;
    let name = element.local_name().to_owned();
    for child in element.elements_mut() {
        walk(child, &namespaces, &name, action)?;
    }
    Ok(())
}
fn visit(
    doc: &mut Document,
    mut action: impl FnMut(&mut Element, &str, &str) -> Result<()>,
) -> Result<()> {
    walk(
        doc.root_mut().map_err(invalid)?,
        &BTreeMap::new(),
        "",
        &mut action,
    )
}
fn text_shift(e: &mut Element, shift: &Shift<'_>, local: bool) -> Result<()> {
    let old = e.text();
    let new = shift.formula(&old, local)?.text;
    if old != new {
        e.children = vec![Node::Text(new)];
    }
    Ok(())
}
fn attr_shift(e: &mut Element, key: &str, shift: &Shift<'_>) -> Result<()> {
    if let Some(value) = e.attrs.get_mut(key) {
        *value = shift.formula(value, true)?.text;
    }
    Ok(())
}
fn number(text: &str) -> Result<u32> {
    text.trim()
        .parse()
        .map_err(|_| invalid("invalid row index"))
}
fn zero_text(e: &mut Element, shift: &Shift<'_>) -> Result<()> {
    let old = e.text();
    let row = number(&old)?;
    let new = shift.zero_row(row)?;
    if row != new {
        e.children = vec![Node::Text(old.replacen(old.trim(), &new.to_string(), 1))];
    }
    Ok(())
}
fn relationship_path(part: &str) -> String {
    let (dir, file) = part.rsplit_once('/').unwrap_or(("", part));
    format!("{dir}/_rels/{file}.rels")
}
fn resolve(part: &str, target: &str) -> Result<String> {
    if target.contains(['\\', '%', '#', '?', ':']) {
        return Err(native(part, "unsupported relationship target"));
    }
    let mut segments: Vec<&str> = if target.starts_with('/') {
        vec![]
    } else {
        part.rsplit_once('/')
            .map_or("", |(dir, _)| dir)
            .split('/')
            .filter(|s| !s.is_empty())
            .collect()
    };
    for segment in target.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if segments.pop().is_none() {
                    return Err(native(part, "relationship escapes package"));
                }
            }
            s => segments.push(s),
        }
    }
    Ok(segments.join("/"))
}
impl Book {
    fn structural_part(&self, path: &str) -> Result<Document> {
        if let Some(doc) = self.ancillary_updates.get(path) {
            return Ok(doc.clone());
        }
        let bytes = self
            .ancillary
            .get(path)
            .ok_or_else(|| native(path, "related part missing"))?;
        xml::parse(std::str::from_utf8(bytes).map_err(|e| native(path, e))?)
            .map_err(|e| native(path, e))
    }
    fn related(&self, part: &str) -> Result<BTreeMap<String, (String, String)>> {
        let path = relationship_path(part);
        if !self.ancillary.contains_key(&path) {
            return Ok(BTreeMap::new());
        }
        let doc = self.structural_part(&path)?;
        let mut result = BTreeMap::new();
        for r in doc
            .root()
            .map_err(invalid)?
            .elements()
            .filter(|e| e.local_name() == "Relationship")
        {
            let get = |key: &str| {
                r.attrs
                    .get(key)
                    .ok_or_else(|| native(&path, format!("missing {key}")))
            };
            let id = get("Id")?;
            let kind = get("Type")?;
            if [
                "drawing",
                "vmlDrawing",
                "comments",
                "threadedComment",
                "table",
            ]
            .iter()
            .any(|suffix| kind.ends_with(&format!("/{suffix}")))
            {
                if r.attrs.get("TargetMode").is_some_and(|m| m == "External") {
                    return Err(native(&path, "external structural relationship"));
                }
                if result
                    .insert(id.clone(), (kind.clone(), resolve(part, get("Target")?)?))
                    .is_some()
                {
                    return Err(native(&path, "duplicate relationship Id"));
                }
            }
        }
        Ok(result)
    }
    // Content type overrides identify renamed chart/cache parts. Conventional
    // names remain supported for the small manifests accepted by this loader.
    fn structural_paths(&self, prefix: &str, suffix: &str) -> Result<BTreeSet<String>> {
        let mut paths: BTreeSet<_> = self
            .ancillary
            .keys()
            .filter(|p| p.starts_with(prefix) && p.ends_with(".xml"))
            .cloned()
            .collect();
        if self.ancillary.contains_key("[Content_Types].xml") {
            let types = self.structural_part("[Content_Types].xml")?;
            for e in types.root().map_err(invalid)?.elements() {
                if e.local_name() == "Override"
                    && e.attrs
                        .get("ContentType")
                        .is_some_and(|t| t.ends_with(suffix))
                {
                    let name = e
                        .attrs
                        .get("PartName")
                        .ok_or_else(|| native("[Content_Types].xml", "missing PartName"))?;
                    paths.insert(resolve("", name)?);
                }
            }
        }
        Ok(paths)
    }
    fn check_pivot_sources(&self, shift: &Shift<'_>, sheet_index: u32) -> Result<()> {
        for path in self.structural_paths(
            "xl/pivotCache/pivotCacheDefinition",
            ".pivotCacheDefinition+xml",
        )? {
            let mut doc = self.structural_part(&path)?;
            visit(&mut doc, |e, ns, _| {
                if ns != MAIN || e.local_name() != "worksheetSource" {
                    return Ok(());
                }
                if let Some(source_name) = e.attrs.get("name") {
                    let definitions = self.workbook.root().map_err(invalid)?.child("definedNames");
                    let mut found = false;
                    if let Some(definitions) = definitions {
                        for d in definitions
                            .elements()
                            .filter(|d| d.attrs.get("name") == Some(source_name))
                        {
                            found = true;
                            let text = d.text();
                            let reference = text.rsplit_once('!').map_or(text.as_str(), |(_, r)| r);
                            // Dynamic or chained names (e.g. OFFSET) do not expose
                            // a fixed source rectangle: never guess their extent.
                            if Area::parse_unbounded(&reference.replace('$', "")).is_err() {
                                return Err(native(
                                    &path,
                                    format!("unresolved pivot source name {source_name}"),
                                ));
                            }
                            let local = d
                                .attrs
                                .get("localSheetId")
                                .is_some_and(|id| id.parse::<u32>().ok() == Some(sheet_index - 1));
                            if shift.formula(&text, local)?.touches {
                                return Err(native(
                                    &path,
                                    format!(
                                        "pivot source name {source_name} touches inserted rows"
                                    ),
                                ));
                            }
                            if !text.contains('!') && !d.attrs.contains_key("localSheetId") {
                                return Err(native(
                                    &path,
                                    format!("unresolved pivot source sheet for {source_name}"),
                                ));
                            }
                        }
                    }
                    if !found {
                        return Err(native(
                            &path,
                            format!("unresolved pivot source name {source_name}"),
                        ));
                    }
                } else if e
                    .attrs
                    .get("sheet")
                    .is_some_and(|s| s.eq_ignore_ascii_case(shift.sheet))
                {
                    let reference = e
                        .attrs
                        .get("ref")
                        .ok_or_else(|| native(&path, "pivot source ref missing"))?;
                    let area = Area::parse_unbounded(&reference.replace('$', ""))
                        .map_err(|e| native(&path, e))?;
                    if area.bottom >= shift.before {
                        return Err(native(
                            &path,
                            format!("pivot source range {reference} touches inserted rows"),
                        ));
                    }
                }
                Ok(())
            })
            .map_err(|e| native(&path, e))?;
        }
        Ok(())
    }
    pub(super) fn insert_rows(
        &mut self,
        name: &str,
        before: u32,
        count: u32,
        style: &RowStyle,
    ) -> Result<usize> {
        if !(1..=MAX_ROW).contains(&before) || !(1..=500).contains(&count) {
            return Err(invalid(
                "insertRows before must be 1-1048576 and count must be 1-500",
            ));
        }
        if before + count - 1 > MAX_ROW {
            return Err(native(name, "inserted row exceeds 1048576"));
        }
        let sheet_names: Vec<_> = self
            .workbook
            .root()
            .map_err(invalid)?
            .child("sheets")
            .unwrap()
            .elements()
            .filter_map(|s| s.attrs.get("name").cloned())
            .collect();
        let shift = Shift {
            sheet: name,
            sheets: &sheet_names,
            before,
            count,
        };
        let sheet_index = sheet_names.iter().position(|s| s == name).unwrap() as u32 + 1;
        let target = self.sheet(name)?;
        let relations = self.related(&target.part)?;
        // A table entirely above the insertion remains valid. Table intersections
        // and pivot sources cannot be safely repaired without the native engine.
        if let Some(tables) = target.root()?.child("tableParts") {
            for table in tables.elements() {
                let id = table
                    .attrs
                    .iter()
                    .find(|(k, _)| k.ends_with(":id"))
                    .map(|(_, v)| v)
                    .ok_or_else(|| native(&target.part, "tablePart relationship missing"))?;
                let (_, path) = relations
                    .get(id)
                    .filter(|(kind, _)| kind.ends_with("/table"))
                    .ok_or_else(|| native(&target.part, "table relationship missing"))?;
                let doc = self.structural_part(path)?;
                let reference = doc
                    .root()
                    .map_err(invalid)?
                    .attrs
                    .get("ref")
                    .ok_or_else(|| native(path, "table ref missing"))?;
                let area = Area::parse_unbounded(&reference.replace('$', ""))
                    .map_err(|e| native(path, e))?;
                if area.bottom >= before {
                    return Err(native(
                        path,
                        format!("table range {reference} touches inserted rows"),
                    ));
                }
            }
        }
        let mut workbook = self.workbook.clone();
        let mut parts = BTreeMap::new();
        self.check_pivot_sources(&shift, sheet_index)?;
        // Shared formula references must be checked even when their referenced
        // rows are above the insertion, including masters on other worksheets.
        let mut sheets = BTreeMap::new();
        for (sheet_name, old) in &self.sheets {
            let local = sheet_name == name;
            let mut new = old.clone();
            for (reference, (rp, cp)) in &old.cells {
                let cell = element_at_mut(
                    element_at_mut(
                        new.doc
                            .root_mut()
                            .map_err(invalid)?
                            .child_mut("sheetData")
                            .unwrap(),
                        *rp,
                    )?,
                    *cp,
                )?;
                if let Some(f) = cell.child_mut("f") {
                    let location = format!("{} {sheet_name}!{reference}", old.part);
                    let formula = shift
                        .formula(&f.text(), local)
                        .map_err(|e| native(&location, e))?;
                    let kind = f.attrs.get("t").map_or("normal", String::as_str);
                    if kind == "shared" && formula.references_target {
                        return Err(native(&location, "shared formula references target sheet"));
                    }
                    if local && ["shared", "array", "dataTable"].contains(&kind) {
                        let below = address(reference)?.1 >= before;
                        let range_below = f
                            .attrs
                            .get("ref")
                            .map(|r| shift.formula(r, true))
                            .transpose()?
                            .is_some_and(|r| r.touches);
                        if below || range_below {
                            return Err(native(
                                &location,
                                format!("{kind} formula cell/ref touches inserted rows"),
                            ));
                        }
                    }
                    if formula.text != f.text() {
                        f.children = vec![Node::Text(formula.text)];
                        new.touched_rows.insert(address(reference)?.1);
                    }
                }
            }
            // Exclude sheetData: cell formulas have already been handled with
            // locations and shared/array/dataTable checks above.
            let root = new.doc.root_mut().map_err(invalid)?;
            let namespaces = root
                .attrs
                .iter()
                .filter_map(|(k, v)| {
                    if k == "xmlns" {
                        Some((String::new(), v.clone()))
                    } else {
                        k.strip_prefix("xmlns:").map(|p| (p.into(), v.clone()))
                    }
                })
                .collect();
            for child in root
                .elements_mut()
                .filter(|e| e.local_name() != "sheetData")
            {
                walk(child, &namespaces, "worksheet", &mut |e, ns, parent| {
                    if ns != MAIN {
                        return Ok(());
                    }
                    if local {
                        let keys: &[&str] = match e.local_name() {
                            "dimension" | "mergeCell" | "hyperlink" | "autoFilter"
                            | "sortState" | "sortCondition" => &["ref"],
                            "conditionalFormatting" | "dataValidation" | "protectedRange" => {
                                &["sqref"]
                            }
                            "selection" => &["activeCell", "sqref"],
                            "pane" => &["topLeftCell"],
                            _ => &[],
                        };
                        for key in keys {
                            attr_shift(e, key, &shift)?;
                        }
                        if e.local_name() == "brk" && parent == "rowBreaks" {
                            let id = e
                                .attrs
                                .get_mut("id")
                                .ok_or_else(|| invalid("row break id missing"))?;
                            *id = shift.zero_row(number(id)?)?.to_string();
                        }
                    }
                    if (parent == "cfRule" && e.local_name() == "formula")
                        || (parent == "dataValidation"
                            && ["formula1", "formula2"].contains(&e.local_name()))
                    {
                        text_shift(e, &shift, local)?;
                    }
                    Ok(())
                })
                .map_err(|e| native(&old.part, e))?;
            }
            if local {
                new.insert_physical_rows(&shift, style)
                    .map_err(|e| native(&old.part, e))?;
            }
            if new.doc != old.doc {
                sheets.insert(sheet_name.clone(), new);
            }
        }
        let stored: usize = self
            .sheets
            .iter()
            .map(|(n, s)| sheets.get(n).unwrap_or(s).cells.len())
            .sum();
        if stored > MAX_STORED_CELLS {
            return Err(invalid(
                "workbook exceeds 200000 stored cells for edit/diff capability",
            ));
        }
        visit(&mut workbook, |e, ns, _| {
            if ns == MAIN && e.local_name() == "definedName" {
                let local = e
                    .attrs
                    .get("localSheetId")
                    .is_some_and(|id| id.parse::<u32>().ok() == Some(sheet_index - 1));
                text_shift(e, &shift, local).map_err(|err| {
                    native(
                        &format!(
                            "xl/workbook.xml definedName {}",
                            e.attrs.get("name").map_or("?", String::as_str)
                        ),
                        err,
                    )
                })?;
            }
            Ok(())
        })?;
        for path in self.structural_paths("xl/charts/", ".chart+xml")? {
            let old = self.structural_part(&path)?;
            let mut doc = old.clone();
            visit(&mut doc, |e, ns, _| {
                if ns == CHART && e.local_name() == "f" {
                    text_shift(e, &shift, false)?;
                }
                Ok(())
            })
            .map_err(|e| native(&path, e))?;
            if old != doc {
                parts.insert(path, doc);
            }
        }
        if self.ancillary.contains_key("xl/calcChain.xml") {
            let old = self.structural_part("xl/calcChain.xml")?;
            let mut doc = old.clone();
            let mut index = 1;
            visit(&mut doc, |e, ns, _| {
                if ns == MAIN && e.local_name() == "c" {
                    if let Some(id) = e.attrs.get("i") {
                        index = number(id)?;
                    }
                    if index == sheet_index {
                        attr_shift(e, "r", &shift)?;
                    }
                }
                Ok(())
            })
            .map_err(|e| native("xl/calcChain.xml", e))?;
            if old != doc {
                parts.insert("xl/calcChain.xml".into(), doc);
            }
        }
        // Validate referenced drawing/table IDs, rather than silently ignoring
        // an object for which the package has no relationship.
        for e in target
            .root()?
            .elements()
            .filter(|e| ["drawing", "legacyDrawing", "legacyDrawingHF"].contains(&e.local_name()))
        {
            let id = e
                .attrs
                .iter()
                .find(|(k, _)| k.ends_with(":id"))
                .map(|(_, v)| v)
                .ok_or_else(|| native(&target.part, "drawing relationship id missing"))?;
            if !relations.contains_key(id) {
                return Err(native(&target.part, "drawing relationship missing"));
            }
        }
        for (kind, path) in relations.values() {
            if kind.ends_with("/table") || parts.contains_key(path) {
                continue;
            }
            let old = self.structural_part(path)?;
            let mut doc = old.clone();
            if kind.ends_with("/drawing") {
                // absoluteAnchor contains no from/to markers and stays intact.
                visit(&mut doc, |e, ns, _| {
                    if ns == DRAWING && ["twoCellAnchor", "oneCellAnchor"].contains(&e.local_name())
                    {
                        for marker in ["from", "to"] {
                            if let Some(marker) = e.child_mut(marker) {
                                let row = marker
                                    .child_mut("row")
                                    .ok_or_else(|| invalid("drawing marker row missing"))?;
                                zero_text(row, &shift)?;
                            } else if marker == "from" || e.local_name() == "twoCellAnchor" {
                                return Err(invalid("drawing anchor marker missing"));
                            }
                        }
                    }
                    Ok(())
                })
                .map_err(|e| native(path, e))?;
            } else if kind.ends_with("/vmlDrawing") {
                visit(&mut doc, |e, ns, _| {
                    if ns == VML_EXCEL {
                        if e.local_name() == "Row" {
                            zero_text(e, &shift)?;
                        } else if e.local_name() == "Anchor" {
                            let old = e.text();
                            let mut values: Vec<String> =
                                old.split(',').map(str::to_owned).collect();
                            if values.len() != 8 {
                                return Err(invalid("VML Anchor needs eight components"));
                            }
                            for index in [2, 6] {
                                let n = number(&values[index])?;
                                let new = shift.zero_row(n)?;
                                if new != n {
                                    values[index] = values[index].replacen(
                                        values[index].trim(),
                                        &new.to_string(),
                                        1,
                                    );
                                }
                            }
                            let new = values.join(",");
                            if old != new {
                                e.children = vec![Node::Text(new)];
                            }
                        }
                    }
                    Ok(())
                })
                .map_err(|e| native(path, e))?;
            } else {
                visit(&mut doc, |e, ns, _| {
                    if (ns == MAIN && e.local_name() == "comment")
                        || (ns == THREADED && e.local_name() == "threadedComment")
                    {
                        attr_shift(e, "ref", &shift)?;
                    }
                    Ok(())
                })
                .map_err(|e| native(path, e))?;
            }
            if old != doc {
                parts.insert(path.clone(), doc);
            }
        }
        // Every changed XML part is parsed again before any state is committed.
        // Publication additionally reloads the complete candidate package.
        for doc in parts
            .values()
            .chain(sheets.values().map(|s| &s.doc))
            .chain(std::iter::once(&workbook))
        {
            xml::parse(&doc.serialize()).map_err(invalid)?;
        }
        for (name, sheet) in sheets {
            self.dirty.insert(sheet.part.clone());
            self.sheets.insert(name, sheet);
        }
        if workbook != self.workbook {
            self.dirty.insert("xl/workbook.xml".into());
            self.workbook = workbook;
        }
        for (path, doc) in parts {
            self.dirty.insert(path.clone());
            self.ancillary_updates.insert(path, doc);
        }
        Ok(count as usize)
    }
}
impl Sheet {
    fn insert_physical_rows(&mut self, shift: &Shift<'_>, style: &RowStyle) -> Result<()> {
        let donor = if *style == RowStyle::Above && shift.before > 1 {
            self.row(shift.before - 1)?.cloned()
        } else {
            None
        };
        let mut template = styles::make(self.data()?, "row");
        if let Some(donor) = donor {
            template.attrs = donor.attrs.clone();
            template.attrs.remove("r");
            template.attrs.remove("hidden");
            for cell in donor.elements().filter(|e| e.local_name() == "c") {
                let id = attr_u32(cell, "s", 0)?;
                if id != 0 {
                    let mut empty = styles::make(&template, "c");
                    empty.attrs.insert("s".into(), id.to_string());
                    empty.attrs.insert("r".into(), cell.attrs["r"].clone());
                    template.children.push(Node::Element(empty));
                }
            }
        }
        let styled = template.elements().count() * shift.count as usize;
        if self.cells.len() + styled > MAX_STORED_CELLS {
            return Err(invalid("worksheet exceeds 200000 stored cells"));
        }
        let position = self
            .rows
            .range(shift.before..)
            .next()
            .map_or(self.data()?.children.len(), |(_, p)| *p);
        let data = self
            .doc
            .root_mut()
            .map_err(invalid)?
            .child_mut("sheetData")
            .unwrap();
        for row in data.elements_mut() {
            let number = attr_u32(row, "r", 0)?;
            if number >= shift.before {
                let new = shift.row(number)?;
                row.attrs.insert("r".into(), new.to_string());
                for cell in row.elements_mut().filter(|c| c.local_name() == "c") {
                    let (col, _) = address(&cell.attrs["r"])?;
                    cell.attrs
                        .insert("r".into(), format!("{}{new}", column_name(col)));
                }
            }
        }
        let mut inserted = Vec::new();
        for number in shift.before..shift.before + shift.count {
            let mut row = template.clone();
            row.attrs.insert("r".into(), number.to_string());
            for cell in row.elements_mut() {
                let (col, _) = address(&cell.attrs["r"])?;
                cell.attrs
                    .insert("r".into(), format!("{}{number}", column_name(col)));
            }
            inserted.push(Node::Element(row));
        }
        data.children.splice(position..position, inserted);
        let touched = self.touched_rows.clone();
        let mut rebuilt = Sheet::new(self.part.clone(), self.doc.clone())?;
        rebuilt.touched_rows = touched.into_iter().filter(|r| *r < shift.before).collect();
        rebuilt
            .touched_rows
            .extend(rebuilt.rows.range(shift.before..).map(|(r, _)| *r));
        // Dimension includes empty inserted rows as well as the displaced extent.
        if rebuilt.root()?.child("dimension").is_some() {
            let right = template
                .elements()
                .filter_map(|c| address(&c.attrs["r"]).ok())
                .map(|(c, _)| c)
                .max();
            let column = right.unwrap_or_else(|| {
                rebuilt
                    .root()
                    .ok()
                    .and_then(|r| r.child("dimension"))
                    .and_then(|d| d.attrs.get("ref"))
                    .and_then(|r| Area::parse_unbounded(r).ok())
                    .map_or(1, |a| a.left)
            });
            rebuilt.extend_dimension(column, shift.before)?;
            rebuilt.extend_dimension(column, shift.before + shift.count - 1)?;
        }
        *self = rebuilt;
        Ok(())
    }
}
