use super::*;
use crate::{execute, package::Limits};
use std::{fs, io::Write as _, path::PathBuf};
use tempfile::TempDir;
use zip::{ZipWriter, write::SimpleFileOptions};

const STYLE_XML: &str = r#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
<numFmts count="1"><numFmt numFmtId="164" formatCode="0.00 kg"/></numFmts>
<fonts count="2"><font><name val="Arial"/><sz val="10"/></font><font><sz val="11"/><color theme="4" tint="0.25"/><name val="Calibri"/><b/><i/><strike/><u val="double"/><family val="2"/><charset val="134"/><scheme val="minor"/></font></fonts>
<fills count="3"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill><fill><patternFill patternType="solid"><fgColor indexed="0"/></patternFill></fill></fills>
<borders count="2"><border><left/><right/><top/><bottom/></border><border><left style="thin"/><right style="double"/><top/><bottom style="dashed"/></border></borders>
<cellStyleXfs count="2"><xf fontId="0" fillId="0" borderId="0" numFmtId="0"/><xf fontId="1" fillId="2" borderId="1" numFmtId="164"/></cellStyleXfs>
<cellXfs count="3"><xf fontId="0" fillId="0" borderId="0" numFmtId="0" xfId="0"/><xf fontId="1" fillId="2" borderId="1" numFmtId="164" xfId="1" applyFont="1" applyFill="1" applyBorder="1" applyAlignment="1" applyNumberFormat="1" applyProtection="1" quotePrefix="1"><alignment horizontal="center" vertical="top" wrapText="1"/><protection locked="0"/><extLst><ext uri="test"><custom:record xmlns:custom="urn:test" value="preserve"/></ext></extLst></xf><xf fontId="0" fillId="0" borderId="0" numFmtId="0" xfId="0" applyFont="0"/></cellXfs></styleSheet>"#;
const ALL_COMPONENTS: [&str; 5] = ["font", "fill", "border", "alignment", "numberFormat"];

struct Fixture {
    _dir: TempDir,
    source: PathBuf,
    package: Package,
}
impl Fixture {
    fn new(body: &str) -> Self {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("source.xlsx");
        let mut zip = ZipWriter::new(fs::File::create(&source).unwrap());
        let sheet = format!(r#"<worksheet xmlns="{MAIN}">{body}</worksheet>"#);
        let workbook = format!(
            r#"<workbook xmlns="{MAIN}" xmlns:r="{REL}"><sheets><sheet name="UseCase" sheetId="1" r:id="r1"/></sheets></workbook>"#
        );
        for (name, content) in [
            ("[Content_Types].xml", "<Types/>"),
            ("xl/workbook.xml", workbook.as_str()),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="r1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="r2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/><Relationship Id="r3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/></Relationships>"#,
            ),
            ("xl/styles.xml", STYLE_XML),
            ("xl/worksheets/sheet1.xml", sheet.as_str()),
            (
                "xl/sharedStrings.xml",
                r#"<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><si><t>shared</t></si><si><r><t>rich</t></r></si></sst>"#,
            ),
        ] {
            zip.start_file(name, SimpleFileOptions::default()).unwrap();
            zip.write_all(content.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        let package = Package::read(&source, Limits::new(None, None).unwrap()).unwrap();
        Self {
            _dir: dir,
            source,
            package,
        }
    }
    fn book(&self) -> Book {
        Book::load(&self.package).unwrap()
    }
    fn inspect(&self, detail: &str, range: &str) -> Value {
        execute(
            json!({"schemaVersion":1,"operation":"inspect","source":self.source,
            "ranges":[format!("UseCase!{range}")],"detail":detail}),
        )
        .unwrap()
    }
    fn request(&self, operations: Value) -> Value {
        json!({"schemaVersion":1,"operation":"patch","source":self.source,
            "output":self.source.with_file_name("candidate.xlsx"),
            "expectedSha256":self.package.sha(),"operations":operations})
    }
}
fn operation(value: Value) -> Operation {
    serde_json::from_value(value).unwrap()
}
fn apply(book: &mut Book, value: Value) -> usize {
    book.apply_bounded(&operation(value), 10000, true).unwrap()
}
fn cell<'a>(book: &'a Book, reference: &str) -> &'a Element {
    book.sheet("UseCase")
        .unwrap()
        .cell(reference)
        .unwrap()
        .unwrap()
}
fn compact(book: &Book, reference: &str) -> Value {
    book.describe_compact("UseCase", reference, &mut BTreeMap::new())
        .unwrap()
}

#[test]
fn creation_orders_indexes_inherits_styles_extends_dimension_and_preserves_rows() {
    for newline in ["", "\n", "\r\n"] {
        let untouched = format!(
            "<row customHeight='1' ht='19' r='8'>{newline}  <c t='inlineStr' r='H8'><is><t>keep &amp; &#65;</t></is></c>{newline}</row>"
        );
        let fixture = Fixture::new(&format!(
            r#"<dimension ref="B2:H8"/><cols><col min="1" max="3" style="1"/></cols><sheetData>{newline}<row r="2" s="0" customFormat="1" spans="2:2"><c r="B2" t="inlineStr"><is><t>old</t></is></c></row>{newline}<row r="4" s="0" customFormat="0"/>{newline}{untouched}{newline}</sheetData>"#
        ));
        let mut book = fixture.book();
        for reference in ["C2", "A2", "B4", "Z1", "A6"] {
            apply(
                &mut book,
                json!({"op":"setText","sheet":"UseCase","cell":reference,"expectedText":"","value":reference}),
            );
        }
        apply(
            &mut book,
            json!({"op":"setStyle","sheet":"UseCase","range":"E3","style":{"bold":true}}),
        );
        apply(
            &mut book,
            json!({"op":"copyStyle","sheet":"UseCase","range":"D5","fromCell":"B2","components":ALL_COMPONENTS}),
        );
        apply(
            &mut book,
            json!({"op":"rowHeight","sheet":"UseCase","row":7,"height":24}),
        );
        apply(
            &mut book,
            json!({"op":"rowVisibility","sheet":"UseCase","row":9,"hidden":true}),
        );
        apply(
            &mut book,
            json!({"op":"setText","sheet":"UseCase","cell":"B2","expectedText":"old","value":"after insertions"}),
        );
        let sheet = book.sheet("UseCase").unwrap();
        assert_eq!(
            sheet.rows.keys().copied().collect::<Vec<_>>(),
            (1..=9).collect::<Vec<_>>()
        );
        for reference in sheet.cells.keys() {
            assert_eq!(
                sheet.cell(reference).unwrap().unwrap().attrs["r"],
                *reference
            );
        }
        assert_eq!(
            sheet
                .row(2)
                .unwrap()
                .unwrap()
                .elements()
                .map(|e| e.attrs["r"].as_str())
                .collect::<Vec<_>>(),
            ["A2", "B2", "C2"]
        );
        for reference in ["A2", "C2", "D5"] {
            assert_eq!(cell(&book, reference).attrs["s"], "0");
        }
        for reference in ["B4", "A6"] {
            assert_eq!(cell(&book, reference).attrs["s"], "1");
        }
        assert!(!cell(&book, "Z1").attrs.contains_key("s"));
        assert_eq!(
            sheet.root().unwrap().child("dimension").unwrap().attrs["ref"],
            "A1:Z8"
        );
        assert_eq!(sheet.row(2).unwrap().unwrap().attrs["spans"], "2:2");
        let updates = book.updates(&fixture.package).unwrap();
        let output = std::str::from_utf8(&updates["xl/worksheets/sheet1.xml"]).unwrap();
        assert!(
            output.contains(&untouched),
            "untouched row must remain byte-identical"
        );
        let mut package = fixture.package;
        package.parts.extend(updates);
        let reloaded = Book::load(&package).unwrap();
        assert_eq!(compact(&reloaded, "B2")["text"], "after insertions");
        assert_eq!(compact(&reloaded, "A6")["styleId"], 1);
        assert!(reloaded.updates(&package).unwrap().is_empty());
    }
}

#[test]
fn creation_refuses_covered_merges_and_obeys_target_budget() {
    let fixture = Fixture::new(r#"<sheetData/><mergeCells><mergeCell ref="A1:B1"/></mergeCells>"#);
    let mut book = fixture.book();
    for op in [
        json!({"op":"setText","sheet":"UseCase","cell":"B1","expectedText":"","value":"x"}),
        json!({"op":"setStyle","sheet":"UseCase","range":"A1:B1","style":{"bold":true}}),
    ] {
        assert!(
            book.apply_bounded(&operation(op), 10000, true)
                .unwrap_err()
                .to_string()
                .contains("merged")
        );
        assert!(book.sheet("UseCase").unwrap().cells.is_empty());
    }
    let op =
        operation(json!({"op":"setStyle","sheet":"UseCase","range":"A2:B2","style":{"bold":true}}));
    assert!(
        book.apply_bounded(&op, 1, true)
            .unwrap_err()
            .to_string()
            .contains("budget")
    );
    assert_eq!(book.apply_bounded(&op, 2, true).unwrap(), 2);
    let row = operation(json!({"op":"rowHeight","sheet":"UseCase","row":3,"height":20}));
    assert!(book.apply_bounded(&row, 0, true).is_err());
    assert!(book.sheet("UseCase").unwrap().row(3).unwrap().is_none());
}

#[test]
fn creation_enforces_stored_cell_limit_before_mutating() {
    // Exercise the exact boundary using stored cells across multiple rows.
    let mut data = String::from("<sheetData>");
    for row in 1..=20 {
        data.push_str(&format!("<row r=\"{row}\">"));
        for column in 1..=10000 {
            data.push_str(&format!("<c r=\"{}{row}\"/>", column_name(column)));
        }
        data.push_str("</row>");
    }
    data.push_str("</sheetData>");
    let fixture = Fixture::new(&data);
    let mut book = fixture.book();
    let op = operation(
        json!({"op":"setText","sheet":"UseCase","cell":"A21","expectedText":"","value":"x"}),
    );
    assert!(
        book.apply_bounded(&op, 10000, true)
            .unwrap_err()
            .to_string()
            .contains("200000")
    );
    assert_eq!(book.sheet("UseCase").unwrap().cells.len(), MAX_STORED_CELLS);
    assert!(book.sheet("UseCase").unwrap().row(21).unwrap().is_none());
}

#[test]
fn set_number_preserves_style_and_round_trips_numeric_types() {
    let fixture = Fixture::new(
        r#"<sheetData><row r="1"><c r="A1" s="1" t="inlineStr"><is><t>old</t></is></c><c r="B1"><v>1.00E+02</v></c><c r="C1" t="s"><v>0</v></c></row></sheetData>"#,
    );
    let mut book = fixture.book();
    for (reference, expected, number, raw) in [
        ("A1", "old", 42.0, "42"),
        ("B1", "1.00E+02", -0.125, "-0.125"),
        ("C1", "shared", 0.0, "0"),
        ("D2", "", 1.25, "1.25"),
    ] {
        apply(
            &mut book,
            json!({"op":"setNumber","sheet":"UseCase","cell":reference,"expectedText":expected,"value":number}),
        );
        let c = cell(&book, reference);
        assert!(!c.attrs.contains_key("t"));
        assert!(c.child("is").is_none());
        assert_eq!(c.child("v").unwrap().text(), raw);
        for compact_mode in [false, true] {
            let result = book
                .inspect(&[format!("UseCase!{reference}")], 1, compact_mode)
                .unwrap();
            assert_eq!(result["cells"][0]["kind"], "n");
            assert_eq!(result["cells"][0]["value"], raw);
        }
    }
    assert_eq!(cell(&book, "A1").attrs["s"], "1");
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let op = Operation::SetNumber {
            sheet: "UseCase".into(),
            cell: "E2".into(),
            expected_text: "".into(),
            value,
        };
        assert!(
            book.apply_bounded(&op, 1, true)
                .unwrap_err()
                .to_string()
                .contains("finite")
        );
    }
    assert!(book.sheet("UseCase").unwrap().cell("E2").unwrap().is_none());
}

#[test]
fn append_text_copies_font_into_ordered_runs_and_overrides_only_new_run() {
    let fixture = Fixture::new(
        r#"<cols><col min="2" max="2" style="1"/></cols><sheetData><row r="1"><c r="A1" s="1" t="s"><v>0</v></c></row></sheetData>"#,
    );
    let mut book = fixture.book();
    apply(
        &mut book,
        json!({"op":"appendText","sheet":"UseCase","cell":"A1","expectedText":"shared","value":" new <&> ","fontColor":"aabbcc","bold":false,"strike":false}),
    );
    let c = cell(&book, "A1");
    assert_eq!(c.attrs["s"], "1");
    assert_eq!(c.attrs["t"], "inlineStr");
    assert!(c.child("v").is_none());
    let runs = c.child("is").unwrap().elements().collect::<Vec<_>>();
    assert_eq!(runs.len(), 2);
    let names = [
        "rFont", "charset", "family", "b", "i", "strike", "color", "sz", "u", "scheme",
    ];
    for (i, run) in runs.iter().enumerate() {
        assert_eq!(run.child("t").unwrap().attrs["xml:space"], "preserve");
        let p = run.child("rPr").unwrap();
        assert_eq!(
            p.elements().map(Element::local_name).collect::<Vec<_>>(),
            names
        );
        assert_eq!(p.child("rFont").unwrap().attrs["val"], "Calibri");
        assert_eq!(p.child("sz").unwrap().attrs["val"], "11");
        assert_eq!(p.child("u").unwrap().attrs["val"], "double");
        if i == 0 {
            assert_eq!(p.child("color").unwrap().attrs["theme"], "4");
            assert!(p.child("b").unwrap().attrs.is_empty());
        } else {
            assert_eq!(p.child("color").unwrap().attrs["rgb"], "FFAABBCC");
            assert_eq!(p.child("color").unwrap().attrs.len(), 1);
            assert_eq!(p.child("b").unwrap().attrs["val"], "0");
            assert_eq!(p.child("strike").unwrap().attrs["val"], "0");
        }
    }
    assert_eq!(compact(&book, "A1")["text"], "shared new <&> ");
    assert_eq!(compact(&book, "A1")["richText"], true);
    apply(
        &mut book,
        json!({"op":"appendText","sheet":"UseCase","cell":"B2","expectedText":"","value":"only new","fontColor":"80ff0000","bold":true,"strike":true}),
    );
    let runs = cell(&book, "B2")
        .child("is")
        .unwrap()
        .elements()
        .collect::<Vec<_>>();
    assert_eq!(runs.len(), 1);
    assert_eq!(
        runs[0].child("rPr").unwrap().child("color").unwrap().attrs["rgb"],
        "80FF0000"
    );
    assert_eq!(cell(&book, "B2").attrs["s"], "1");
    assert!(
        book.updates(&fixture.package)
            .unwrap()
            .keys()
            .all(|p| p != "xl/styles.xml")
    );
}

#[test]
fn new_text_operations_preserve_existing_safety_refusals() {
    let fixture = Fixture::new(
        r#"<sheetData><row r="1"><c r="A1"><f>1+1</f><v>2</v></c><c r="B1" t="s"><v>1</v></c><c r="C1" t="inlineStr"><is><r><t>rich</t></r></is></c><c r="D1" t="inlineStr"><is><t>_x000A_</t></is></c><c r="E1" t="b"><v>1</v></c><c r="F1" s="2"/></row></sheetData>"#,
    );
    let mut book = fixture.book();
    for op in ["setNumber", "appendText"] {
        for (reference, text) in [
            ("A1", "2"),
            ("B1", "rich"),
            ("C1", "rich"),
            ("D1", "_x000A_"),
            ("E1", "1"),
        ] {
            let value = if op == "setNumber" {
                json!(42)
            } else {
                json!("new")
            };
            let op = operation(
                json!({"op":op,"sheet":"UseCase","cell":reference,"expectedText":text,"value":value}),
            );
            assert!(book.apply_bounded(&op, 1, true).is_err());
        }
    }
    for value in ["", "_x0041_", "\u{0}"] {
        let op = operation(
            json!({"op":"appendText","sheet":"UseCase","cell":"G1","expectedText":"","value":value}),
        );
        assert!(book.apply_bounded(&op, 1, true).is_err());
    }
    for extra in [
        json!({"fontColor":"red"}),
        json!({"value":"a".repeat(32768)}),
        json!({"expectedText":"wrong"}),
        json!({"cell":"F1"}),
    ] {
        let mut op = json!({"op":"appendText","sheet":"UseCase","cell":"G1","expectedText":"","value":"new"});
        op.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert!(book.apply_bounded(&operation(op), 1, true).is_err());
    }
    for op in [
        json!({"op":"setNumber","sheet":"UseCase","cell":"G1","expectedText":"wrong","value":3}),
        json!({"op":"setStyle","sheet":"UseCase","range":"C1","style":{"bold":true}}),
        json!({"op":"copyStyle","sheet":"UseCase","range":"C1","fromCell":"F1","components":ALL_COMPONENTS}),
    ] {
        assert!(book.apply_bounded(&operation(op), 1, true).is_err());
    }
    assert!(book.updates(&fixture.package).unwrap().is_empty());
    assert!(book.sheet("UseCase").unwrap().cell("G1").unwrap().is_none());
    for marker in ["protected", "signed"] {
        let mut book = fixture.book();
        if marker == "protected" {
            book.sheets
                .get_mut("UseCase")
                .unwrap()
                .doc
                .root_mut()
                .unwrap()
                .children
                .push(Node::Element(Element::new("sheetProtection")));
        } else {
            book.signed = true;
        }
        for op in ["setNumber", "appendText"] {
            let value = if op == "setNumber" {
                json!(1)
            } else {
                json!("text")
            };
            assert!(book.apply_bounded(&operation(json!({"op":op,"sheet":"UseCase","cell":"G1","expectedText":"","value":value})),1,true).unwrap_err().to_string().contains(marker));
        }
    }
}

#[test]
fn copy_all_components_adopts_donor_base_flags_protection_and_extensions() {
    let fixture =
        Fixture::new(r#"<sheetData><row r="1"><c r="A1"/><c r="B1" s="1"/></row></sheetData>"#);
    let mut book = fixture.book();
    for components in [
        json!(["font"]),
        json!(["font", "fill", "border", "alignment"]),
    ] {
        let op = operation(
            json!({"op":"copyStyle","sheet":"UseCase","range":"A1","fromCell":"B1","components":components}),
        );
        let error = book.apply_bounded(&op, 1, true).unwrap_err().to_string();
        for text in [
            "UseCase!A1",
            "UseCase!B1",
            "all five components",
            "adopt the donor cell style entirely",
        ] {
            assert!(error.contains(text), "{error}");
        }
    }
    let before = book.styles.clone();
    apply(
        &mut book,
        json!({"op":"copyStyle","sheet":"UseCase","range":"A1","fromCell":"B1","components":ALL_COMPONENTS}),
    );
    apply(
        &mut book,
        json!({"op":"copyStyle","sheet":"UseCase","range":"C2","fromCell":"A1","components":ALL_COMPONENTS}),
    );
    for reference in ["A1", "C2"] {
        assert_eq!(cell(&book, reference).attrs["s"], "1");
        let target = book
            .resolved_style(
                book.sheet("UseCase").unwrap().style_id(reference).unwrap(),
                &mut BTreeMap::new(),
            )
            .unwrap()
            .clone();
        assert_eq!(target, styles::resolved(&before, 1).unwrap());
        assert_eq!(target["flags"]["quotePrefix"], "1");
        assert_eq!(target["protection"]["attributes"]["locked"], "0");
        assert_eq!(target["extensions"].as_array().unwrap().len(), 1);
    }
    assert_eq!(
        book.styles, before,
        "AppendCache deduplicates the whole existing donor record"
    );
    for components in [
        json!(["font", "font"]),
        json!(["font", "fill", "border", "alignment", "unknown"]),
    ] {
        assert!(book.apply_bounded(&operation(json!({"op":"copyStyle","sheet":"UseCase","range":"A1","fromCell":"B1","components":components})),1,true).is_err());
    }
}

#[test]
fn failed_operation_restores_style_appends_cache_and_sheet_state() {
    let fixture = Fixture::new(
        r#"<dimension ref="A1:B1"/><sheetData><row r="1"><c r="A1"/><c r="B1" s="2"/></row></sheetData>"#,
    );
    let mut book = fixture.book();
    let original = book.styles.clone();
    let sheet = book.sheet("UseCase").unwrap().doc.clone();
    // A1 appends a font and xf before B1 refuses its disabled applyFont flag.
    let failed =
        operation(json!({"op":"setStyle","sheet":"UseCase","range":"A1:B1","style":{"bold":true}}));
    assert!(book.apply_bounded(&failed, 2, true).is_err());
    assert_eq!(book.styles, original);
    assert_eq!(book.sheet("UseCase").unwrap().doc, sheet);
    assert!(book.updates(&fixture.package).unwrap().is_empty());
    apply(
        &mut book,
        json!({"op":"setStyle","sheet":"UseCase","range":"A2","style":{"bold":true}}),
    );
    assert_eq!(compact(&book, "A2")["style"]["font"]["bold"], true);
    assert_eq!(
        book.styles
            .root()
            .unwrap()
            .child("fonts")
            .unwrap()
            .elements()
            .count(),
        3
    );
    assert_eq!(
        book.styles
            .root()
            .unwrap()
            .child("cellXfs")
            .unwrap()
            .elements()
            .count(),
        4
    );
}

#[test]
fn validation_collects_violations_and_continues_sequentially_without_writes() {
    let fixture =
        Fixture::new(r#"<sheetData><row r="1"><c r="A1"/><c r="B1" s="2"/></row></sheetData>"#);
    let operations = json!([
        {"op":"setStyle","sheet":"UseCase","range":"A1:B1","style":{"bold":true}},
        {"op":"setText","sheet":"UseCase","cell":"A2","expectedText":"","value":"created"},
        {"op":"setText","sheet":"UseCase","cell":"A2","expectedText":"wrong","value":"bad"},
        {"op":"appendText","sheet":"UseCase","cell":"A2","expectedText":"created","value":" appended"},
        {"op":"rowHeight","sheet":"UseCase","row":3,"height":410},
        {"op":"rowHeight","sheet":"UseCase","row":3,"height":25},
        {"op":"setStyle","sheet":"UseCase","range":"A1","style":{"bold":true}}
    ]);
    let source_before = fs::read(&fixture.source).unwrap();
    let mut request = fixture.request(operations);
    request["validateOnly"] = json!(true);
    request.as_object_mut().unwrap().remove("output");
    let report = execute(request.clone()).unwrap();
    assert_eq!(report["validateOnly"], true);
    assert_eq!(report["operationsChecked"], 7);
    assert_eq!(report["targetsProcessed"], 4);
    assert_eq!(report["sourceSha256"], fixture.package.sha());
    assert_eq!(
        report["wouldChangeParts"],
        json!(["xl/styles.xml", "xl/worksheets/sheet1.xml"])
    );
    let violations = report["violations"].as_array().unwrap();
    assert_eq!(
        violations
            .iter()
            .map(|v| v["operationIndex"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [0, 2, 4]
    );
    assert_eq!(violations[0]["target"], "A1:B1");
    assert_eq!(violations[1]["op"], "setText");
    assert_eq!(violations[2]["target"], "3");
    assert!(!fixture.source.with_file_name("candidate.xlsx").exists());
    // Even an existing output is ignored, not replaced, in validation mode.
    request["output"] = json!(fixture.source);
    assert_eq!(execute(request.clone()).unwrap(), report);
    assert_eq!(fs::read(&fixture.source).unwrap(), source_before);
    request["expectedSha256"] = json!("0".repeat(64));
    assert!(
        execute(request.clone())
            .unwrap_err()
            .to_string()
            .contains("expectedSha256")
    );
    request.as_object_mut().unwrap().remove("expectedSha256");
    assert!(execute(request).is_err());
    let mut all_failed = fixture.request(
        json!([{"op":"setStyle","sheet":"UseCase","range":"A1:B1","style":{"bold":true}}]),
    );
    all_failed["validateOnly"] = json!(true);
    let report = execute(all_failed).unwrap();
    assert_eq!(report["wouldChangeParts"], json!([]));
    assert_eq!(report["targetsProcessed"], 0);
}

#[test]
fn operation_errors_expose_index_op_sheet_and_all_target_forms() {
    let fixture = Fixture::new("<sheetData/>");
    for (op, target) in [
        (
            json!({"op":"setText","sheet":"Missing","cell":"A23","expectedText":"","value":"x"}),
            "A23",
        ),
        (
            json!({"op":"setFormula","sheet":"Missing","cell":"A23","expectedText":"","formula":"1"}),
            "A23",
        ),
        (
            json!({"op":"setNumber","sheet":"Missing","cell":"A23","expectedText":"","value":1}),
            "A23",
        ),
        (
            json!({"op":"appendText","sheet":"Missing","cell":"A23","expectedText":"","value":"x"}),
            "A23",
        ),
        (
            json!({"op":"setStyle","sheet":"Missing","range":"A1:B2","style":{"bold":true}}),
            "A1:B2",
        ),
        (
            json!({"op":"copyStyle","sheet":"Missing","range":"A1:B2","fromCell":"C1","components":["font"]}),
            "A1:B2",
        ),
        (
            json!({"op":"rowHeight","sheet":"Missing","row":12,"height":20}),
            "12",
        ),
        (
            json!({"op":"rowVisibility","sheet":"Missing","row":12,"hidden":true}),
            "12",
        ),
        (
            json!({"op":"columnWidth","sheet":"Missing","column":"AD","width":10}),
            "AD",
        ),
    ] {
        let operations = json!([{"op":"rowHeight","sheet":"UseCase","row":1,"height":20},op]);
        let error = execute(fixture.request(operations)).unwrap_err();
        assert!(matches!(error, Error::Operation { index: 1, .. }));
        let details = error.details();
        assert_eq!(details["operationIndex"], 1);
        assert_eq!(details["sheet"], "Missing");
        assert_eq!(details["target"], target);
        assert_eq!(details["op"], op["op"]);
        assert!(details["message"].as_str().unwrap().contains(&format!(
            "operation 1 ({} Missing!{target})",
            op["op"].as_str().unwrap()
        )));
        assert!(!fixture.source.with_file_name("candidate.xlsx").exists());
    }
}

#[test]
fn compact_inspect_omits_defaults_but_preserves_required_and_meaningful_fields() {
    let fixture = Fixture::new(
        r#"<sheetFormatPr defaultRowHeight="15" defaultColWidth="12"/><cols><col min="1" max="1" width="12"/><col min="2" max="2" width="24"/></cols><sheetData><row r="1" ht="15"><c r="A1" t="inlineStr"><is><t></t></is></c><c r="B1" s="1"><v>0</v></c><c r="C1"><f>1+1</f><v>2</v></c></row><row r="2" ht="30" customHeight="1"><c r="A2" t="s"><v>1</v></c></row></sheetData><mergeCells><mergeCell ref="A2:B2"/></mergeCells>"#,
    );
    let report = fixture.inspect("compact", "A1:C3");
    assert!(report.get("styleContext").is_none());
    assert!(report.get("parts").is_none());
    assert_eq!(report["totalCells"], 9);
    assert_eq!(report["truncated"], false);
    let cells = report["cells"].as_array().unwrap();
    for c in cells {
        for key in ["sheet", "cell", "kind", "styleId"] {
            assert!(c.get(key).is_some(), "missing {key}");
        }
    }
    let plain = &cells[0];
    assert_eq!(plain["text"], "");
    for key in [
        "blank",
        "value",
        "formula",
        "richText",
        "merge",
        "baseStyleId",
        "row",
        "column",
    ] {
        assert!(plain.get(key).is_none(), "default {key}");
    }
    assert_eq!(plain["style"], json!({"font":{"name":"Arial","size":10.0}}));
    let styled = &cells[1];
    assert_eq!(styled["value"], "0");
    assert!(styled.get("text").is_none());
    assert_eq!(styled["baseStyleId"], 1);
    assert_eq!(styled["column"], json!({"width":24.0}));
    assert_eq!(
        styled["style"]["font"]["color"],
        json!({"theme":4,"tint":0.25})
    );
    assert_eq!(styled["style"]["fill"]["color"], json!({"indexed":0}));
    assert_eq!(
        styled["style"]["border"],
        json!({"left":"thin","right":"double","bottom":"dashed"})
    );
    assert_eq!(
        styled["style"]["numberFormat"],
        json!({"formatCode":"0.00 kg"})
    );
    assert_eq!(cells[2]["formula"], true);
    assert_eq!(cells[3]["richText"], true);
    assert_eq!(cells[3]["merge"], "A2:B2");
    assert_eq!(cells[3]["row"], json!({"height":30.0,"customHeight":true}));
    assert_eq!(cells[8]["kind"], "blank");
    assert_eq!(cells[8]["blank"], true);
    for (detail, include) in [("compact", true), ("full", false)] {
        let report = execute(json!({"schemaVersion":1,"operation":"inspect","source":fixture.source,"ranges":["UseCase!A1"],"detail":detail,"includeParts":include})).unwrap();
        assert_eq!(report.get("parts").is_some(), include);
        assert_eq!(report.get("styleContext").is_some(), detail == "full");
    }
    let default = execute(json!({"schemaVersion":1,"operation":"inspect","source":fixture.source,"ranges":["UseCase!A1"]})).unwrap();
    assert_eq!(default, fixture.inspect("full", "A1"));
}

#[test]
fn compact_size_is_measured_on_53_styled_cells_without_a_tenfold_gate() {
    let mut data = String::from("<sheetData><row r=\"1\">");
    for column in 1..=53 {
        data.push_str(&format!(
            r#"<c r="{}1" s="1" t="inlineStr"><is><t>cell {column}</t></is></c>"#,
            column_name(column)
        ));
    }
    data.push_str("</row></sheetData>");
    let fixture = Fixture::new(&data);
    let full = fixture.inspect("full", "A1:BA1");
    let compact = fixture.inspect("compact", "A1:BA1");
    let full_bytes = serde_json::to_vec(&full).unwrap().len();
    let compact_bytes = serde_json::to_vec(&compact).unwrap().len();
    println!(
        "53 styled cells: full={full_bytes} bytes compact={compact_bytes} bytes saved={} bytes ({:.2}%), {:.2}x",
        full_bytes - compact_bytes,
        100.0 * (full_bytes - compact_bytes) as f64 / full_bytes as f64,
        full_bytes as f64 / compact_bytes as f64
    );
    assert!(compact_bytes < full_bytes);
    assert_eq!(compact["cells"].as_array().unwrap().len(), 53);
    assert_eq!(compact["totalCells"], full["totalCells"]);
    assert_eq!(compact["sheets"], full["sheets"]);
}

#[test]
fn every_successful_response_announces_protocol_features() {
    let fixture = Fixture::new("<sheetData/>");
    let patch = fixture.request(
        json!([{"op":"setNumber","sheet":"UseCase","cell":"A1","expectedText":"","value":42}]),
    );
    let mut validation = patch.clone();
    validation["validateOnly"] = json!(true);
    let expected = json!([
        "createCells",
        "setNumber",
        "appendText",
        "copyStyleAdoptBase",
        "validateOnly",
        "compactInspect",
        "setFormula",
        "insertRows"
    ]);
    let full = fixture.inspect("full", "A1");
    let compact_report = fixture.inspect("compact", "A1");
    let validated = execute(validation).unwrap();
    let patched = execute(patch).unwrap();
    let diff = execute(json!({"schemaVersion":1,"operation":"diff","before":fixture.source,"after":fixture.source.with_file_name("candidate.xlsx")})).unwrap();
    for response in [full, compact_report, validated, patched, diff] {
        assert_eq!(response["schemaVersion"], 1);
        assert_eq!(response["protocolFeatures"], expected);
        assert_eq!(response["visualVerification"], "pending");
        assert!(response["proofBoundary"].as_str().is_some());
    }
    let candidate = Package::read(
        &fixture.source.with_file_name("candidate.xlsx"),
        Limits::new(None, None).unwrap(),
    )
    .unwrap();
    assert_eq!(
        compact(&Book::load(&candidate).unwrap(), "A1")["value"],
        "42"
    );
}

#[test]
fn formula_writes_plain_expressions_preserves_styles_and_round_trips_without_cached_values() {
    let fixture = Fixture::new(
        r#"<dimension ref="A1:F2"/><cols><col min="7" max="8" style="1"/></cols><sheetData><row r="1"><c r="A1" s="1" t="s"><v>0</v></c><c r="B1"><v>1.00E+02</v></c><c r="C1" t="b"><v>1</v></c><c r="D1" s="1" t="str" cm="7"><f t="normal">OLD()</f><v>old cache</v><extLst/></c><c r="E1" t="inlineStr"><is><t>inline</t></is></c><c r="F1" t="str"><v>plain</v></c></row><row r="2" customFormat="1" s="0"/></sheetData>"#,
    );
    let mut book = fixture.book();
    for (reference, expected, formula) in [
        ("A1", "shared", "=Summary!D6"),
        ("B1", "1.00E+02", "SUM(A1:A2)"),
        ("C1", "1", "=IF(1<2,TRUE,FALSE)"),
        ("D1", "=OLD()", "=NEW()"),
        ("E1", "inline", "1"),
        ("F1", "plain", "2"),
        ("G2", "", "3"),
        ("H3", "", "4"),
    ] {
        apply(
            &mut book,
            json!({"op":"setFormula","sheet":"UseCase","cell":reference,"expectedText":expected,"formula":formula}),
        );
        let c = cell(&book, reference);
        assert!(!c.attrs.contains_key("t"));
        assert!(c.child("v").is_none());
        assert!(c.child("is").is_none());
        assert_eq!(
            c.child("f").unwrap().text(),
            formula.strip_prefix('=').unwrap_or(formula)
        );
        assert_eq!(compact(&book, reference)["formula"], true);
        assert!(compact(&book, reference).get("value").is_none());
    }
    assert_eq!(cell(&book, "A1").attrs["s"], "1");
    assert_eq!(cell(&book, "D1").attrs["s"], "1");
    assert_eq!(cell(&book, "D1").attrs["cm"], "7");
    assert_eq!(
        cell(&book, "D1")
            .elements()
            .map(Element::local_name)
            .collect::<Vec<_>>(),
        ["f", "extLst"]
    );
    assert_eq!(cell(&book, "G2").attrs["s"], "0");
    assert_eq!(cell(&book, "H3").attrs["s"], "1");
    assert_eq!(
        book.sheet("UseCase")
            .unwrap()
            .root()
            .unwrap()
            .child("dimension")
            .unwrap()
            .attrs["ref"],
        "A1:H3"
    );
    let updates = book.updates(&fixture.package).unwrap();
    assert_eq!(
        updates.keys().map(String::as_str).collect::<Vec<_>>(),
        ["xl/workbook.xml", "xl/worksheets/sheet1.xml"]
    );
    let wb = xml::parse(std::str::from_utf8(&updates["xl/workbook.xml"]).unwrap()).unwrap();
    assert_eq!(
        wb.root().unwrap().child("calcPr").unwrap().attrs["fullCalcOnLoad"],
        "1"
    );
    assert_eq!(
        wb.root()
            .unwrap()
            .elements()
            .filter(|e| e.local_name() == "calcPr")
            .count(),
        1
    );
    let diff = fixture.book().diff(&book, 20).unwrap();
    assert_eq!(diff["cellChanges"]["total"], 8);
    assert_eq!(diff["workbookStructureChanged"], true);
    assert_eq!(
        diff["cellChanges"]["details"][3]["before"]["formula"]["children"],
        json!(["OLD()"])
    );
    assert_eq!(
        diff["cellChanges"]["details"][3]["after"]["formula"]["children"],
        json!(["NEW()"])
    );
    let mut package = fixture.package;
    package.parts.extend(updates);
    let reloaded = Book::load(&package).unwrap();
    assert_eq!(
        reloaded.inspect(&["UseCase!D1".into()], 1, false).unwrap()["cells"][0]["formula"]["children"],
        json!(["NEW()"])
    );
    assert!(reloaded.updates(&package).unwrap().is_empty());
}

#[test]
fn formula_validation_uses_utf16_and_refuses_non_plain_formula_values() {
    for value in [
        "",
        "=",
        "=\u{1}",
        "=_x0041_",
        "{=SUM(A1:A2)}",
        "={=SUM(A1:A2)}",
        "<f t=\"shared\">SUM(A1)</f>",
        "<f t=\"dataTable\"/>",
    ] {
        assert!(plain_formula(value).is_err(), "{value:?}");
    }
    for expression in ["x".repeat(8192), "😀".repeat(4096)] {
        assert_eq!(plain_formula(&expression).unwrap(), expression);
        assert_eq!(
            plain_formula(&format!("={expression}")).unwrap(),
            expression
        );
        assert!(plain_formula(&format!("{expression}x")).is_err());
    }
    // Array constants inside a normal expression are not shared/CSE records.
    assert_eq!(plain_formula("=SUM({1,2})").unwrap(), "SUM({1,2})");
}

#[test]
fn formula_validation_collects_special_formula_and_safety_refusals_without_mutation() {
    let fixture = Fixture::new(
        r#"<sheetData><row r="1"><c r="A1"><f t="shared" si="0">1</f><v>1</v></c><c r="B1"><f t="array" ref="B1:B2">2</f><v>2</v></c><c r="C1"><f t="dataTable" ref="C1:C2"/><v>3</v></c><c r="D1" t="s"><v>1</v></c><c r="E1" t="inlineStr"><is><t>_x0041_</t></is></c><c r="F1"><f>_x0041_</f><v>6</v></c></row></sheetData><mergeCells><mergeCell ref="A3:B3"/></mergeCells>"#,
    );
    let mut ops = Vec::new();
    for (reference, expected) in [
        ("A1", "=1"),
        ("B1", "=2"),
        ("C1", "="),
        ("D1", "rich"),
        ("E1", "_x0041_"),
        ("F1", "=_x0041_"),
        ("B3", ""),
    ] {
        ops.push(json!({"op":"setFormula","sheet":"UseCase","cell":reference,"expectedText":expected,"formula":"1"}));
    }
    ops.push(json!({"op":"setFormula","sheet":"UseCase","cell":"G1","expectedText":"","formula":"{=SUM(A1:A2)}"}));
    let mut request = fixture.request(json!(ops));
    request["validateOnly"] = json!(true);
    let result = execute(request).unwrap();
    assert_eq!(result["operationsChecked"], 8);
    assert_eq!(result["targetsProcessed"], 0);
    assert_eq!(result["wouldChangeParts"], json!([]));
    assert_eq!(result["violations"].as_array().unwrap().len(), 8);
    for index in [0, 1, 2, 7] {
        assert!(
            result["violations"][index]["message"]
                .as_str()
                .unwrap()
                .contains("shared/array formulas require native application")
        );
        assert_eq!(result["violations"][index]["op"], "setFormula");
    }
    let mut book = fixture.book();
    book.sheets
        .get_mut("UseCase")
        .unwrap()
        .doc
        .root_mut()
        .unwrap()
        .children
        .push(Node::Element(Element::new("sheetProtection")));
    let op = operation(
        json!({"op":"setFormula","sheet":"UseCase","cell":"G2","expectedText":"","formula":"1"}),
    );
    assert!(
        book.apply_bounded(&op, 1, true)
            .unwrap_err()
            .to_string()
            .contains("protected")
    );
    assert!(book.updates(&fixture.package).unwrap().is_empty());
}

#[test]
fn formula_failed_preconditions_leave_previous_formula_and_recalculation_state_intact() {
    let fixture = Fixture::new("<sheetData/>");
    let mut book = fixture.book();
    let mut value = json!({"op":"setFormula","sheet":"UseCase","cell":"A1","expectedText":"wrong","formula":"1"});
    let original = book.workbook.clone();
    assert!(
        book.apply_bounded(&operation(value.clone()), 1, true)
            .is_err()
    );
    assert_eq!(book.workbook, original);
    assert!(book.updates(&fixture.package).unwrap().is_empty());
    value["expectedText"] = json!("");
    assert!(
        book.apply_bounded(&operation(value.clone()), 0, true)
            .is_err()
    );
    apply(&mut book, value.clone());
    let after = book.workbook.clone();
    value["expectedText"] = json!("1"); // Formula text, not its absent cached value.
    value["formula"] = json!("2");
    assert!(
        book.apply_bounded(&operation(value.clone()), 1, true)
            .is_err()
    );
    assert_eq!(cell(&book, "A1").child("f").unwrap().text(), "1");
    assert_eq!(book.workbook, after);
    value["expectedText"] = json!("=1");
    apply(&mut book, value);
    assert_eq!(cell(&book, "A1").child("f").unwrap().text(), "2");
    assert_eq!(book.workbook, after);
}

#[test]
fn recalculation_preserves_workbook_order_metadata_and_existing_calc_properties() {
    for successor in [
        "",
        "oleSize",
        "customWorkbookViews",
        "pivotCaches",
        "smartTagPr",
        "smartTagTypes",
        "webPublishing",
        "fileRecoveryPr",
        "webPublishObjects",
        "extLst",
    ] {
        let suffix = if successor.is_empty() {
            String::new()
        } else {
            format!("<x:{successor}/>")
        };
        let mut doc = xml::parse(&format!(r#"<x:workbook xmlns:x="{MAIN}"><x:workbookPr/><x:bookViews/><x:sheets><x:sheet name="Keep"/></x:sheets><x:definedNames><x:definedName name="Keep">Keep!$A$1</x:definedName></x:definedNames>{suffix}</x:workbook>"#)).unwrap();
        let sheets = doc.root().unwrap().child("sheets").unwrap().clone();
        let names = doc.root().unwrap().child("definedNames").unwrap().clone();
        request_recalculation(&mut doc).unwrap();
        let root = doc.root().unwrap();
        let mut expected = vec![
            "workbookPr",
            "bookViews",
            "sheets",
            "definedNames",
            "calcPr",
        ];
        if !successor.is_empty() {
            expected.push(successor);
        }
        assert_eq!(
            root.elements().map(Element::local_name).collect::<Vec<_>>(),
            expected
        );
        assert_eq!(root.child("sheets"), Some(&sheets));
        assert_eq!(root.child("definedNames"), Some(&names));
        assert_eq!(root.child("calcPr").unwrap().name, "x:calcPr");
        let once = doc.clone();
        request_recalculation(&mut doc).unwrap();
        assert_eq!(doc, once);
    }
    let mut doc = xml::parse("<workbook><sheets/><calcPr calcId=\"12\" calcMode=\"manual\" fullCalcOnLoad=\"0\" forceFullCalc=\"0\"/><extLst/></workbook>").unwrap();
    request_recalculation(&mut doc).unwrap();
    let calc = doc.root().unwrap().child("calcPr").unwrap();
    assert_eq!(calc.attrs["fullCalcOnLoad"], "1");
    assert_eq!(calc.attrs["calcId"], "12");
    assert_eq!(calc.attrs["calcMode"], "manual");
    assert_eq!(calc.attrs["forceFullCalc"], "0");
    assert_eq!(
        doc.root()
            .unwrap()
            .elements()
            .map(Element::local_name)
            .collect::<Vec<_>>(),
        ["sheets", "calcPr", "extLst"]
    );
}

#[path = "insert_rows_tests.rs"]
mod insert_rows_tests;
