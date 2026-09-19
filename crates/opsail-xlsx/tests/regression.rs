use std::{collections::BTreeMap, fs::{self, File}, io::{Read as _, Write as _}, path::Path, sync::{Arc, Barrier}, thread};

use opsail_xlsx::execute;
use quick_xml::{events::Event, Reader};
use serde_json::{json, Value};
use tempfile::TempDir;
use zip::{write::SimpleFileOptions, ZipArchive, ZipWriter};

const MAIN: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

fn styles(fonts: &str, borders: &str, xfs: &str) -> String {
    styles_with_base(fonts, borders, r#"<xf numFmtId="0" fontId="0" fillId="0" borderId="0"/>"#, xfs)
}

fn styles_with_base(fonts: &str, borders: &str, base_xf: &str, xfs: &str) -> String {
    format!(r#"<styleSheet xmlns="{MAIN}"><fonts count="2">{fonts}</fonts><fills count="2"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill></fills><borders count="2">{borders}</borders><cellStyleXfs count="1">{base_xf}</cellStyleXfs><cellXfs count="2">{xfs}</cellXfs></styleSheet>"#)
}

fn write_book(path: &Path, sheet: &str, styles: &str, shared: &str) {
    let mut zip = ZipWriter::new(File::create(path).unwrap());
    let workbook = format!(r#"<workbook xmlns="{MAIN}" xmlns:r="{REL}"><sheets><sheet name="UseCase" sheetId="1" r:id="rId1"/></sheets></workbook>"#);
    for (name, data) in [
        ("[Content_Types].xml", r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/></Types>"#),
        ("xl/workbook.xml", workbook.as_str()),
        ("xl/_rels/workbook.xml.rels", r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/></Relationships>"#),
        ("xl/worksheets/sheet1.xml", sheet),
        ("xl/styles.xml", styles),
        ("xl/sharedStrings.xml", shared),
        ("custom/unknown.bin", "untouched"),
    ] {
        zip.start_file(name, SimpleFileOptions::default()).unwrap();
        zip.write_all(data.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
}

fn part(path: &Path, name: &str) -> String {
    let mut zip = ZipArchive::new(File::open(path).unwrap()).unwrap();
    let mut value = String::new();
    zip.by_name(name).unwrap().read_to_string(&mut value).unwrap();
    value
}

fn well_formed(xml: &str) {
    let mut reader = Reader::from_str(xml);
    loop {
        match reader.read_event().unwrap() {
            Event::Start(event) | Event::Empty(event) => {
                for attr in event.attributes() { attr.unwrap(); }
            }
            Event::Eof => return,
            _ => (),
        }
    }
}

fn column_attributes(xml: &str, min: &str) -> BTreeMap<String, String> {
    let mut reader = Reader::from_str(xml);
    loop {
        match reader.read_event().unwrap() {
            Event::Start(event) | Event::Empty(event) if event.name().as_ref() == b"col" => {
                let attrs = event.attributes().map(|attr| {
                    let attr = attr.unwrap();
                    (String::from_utf8(attr.key.as_ref().to_vec()).unwrap(), attr.unescape_value().unwrap().into_owned())
                }).collect::<BTreeMap<_, _>>();
                if attrs.get("min").is_some_and(|value| value == min) { return attrs; }
            }
            Event::Eof => panic!("column {min} was not written"),
            _ => (),
        }
    }
}

fn inspect(path: &Path, range: &str) -> Value {
    execute(json!({"schemaVersion":1,"operation":"inspect","source":path,"ranges":[range]})).unwrap()
}

fn patch(source: &Path, output: &Path, operations: Value) -> Result<Value, opsail_xlsx::Error> {
    let source_sha = inspect(source, "UseCase!A1")["sourceSha256"].clone();
    execute(json!({"schemaVersion":1,"operation":"patch","source":source,"output":output,"expectedSha256":source_sha,"operations":operations}))
}

fn ordinary_styles() -> String {
    styles(
        r#"<font><name val="Arial"/><sz val="10"/></font><font><name val="Calibri"/><sz val="11"/></font>"#,
        r#"<border><left/><right/><top/><bottom/></border><border><left style="thin"/><right/><top/><bottom/></border>"#,
        r#"<xf numFmtId="0" fontId="0" fillId="0" borderId="0"/><xf numFmtId="0" fontId="1" fillId="0" borderId="1"/>"#,
    )
}

#[test]
fn diff_resolves_shared_formula_and_truncates_details() {
    let dir = TempDir::new().unwrap();
    let before = dir.path().join("before.xlsx");
    let after = dir.path().join("after.xlsx");
    let sheet_before = format!(r#"<worksheet xmlns="{MAIN}"><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1"><f>1+1</f><v>2</v></c></row></sheetData></worksheet>"#);
    let sheet_after = format!(r#"<worksheet xmlns="{MAIN}"><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1"><f>2</f><v>2</v></c></row></sheetData></worksheet>"#);
    write_book(&before, &sheet_before, &ordinary_styles(), &format!(r#"<sst xmlns="{MAIN}"><si><t>before shared</t></si></sst>"#));
    write_book(&after, &sheet_after, &ordinary_styles(), &format!(r#"<sst xmlns="{MAIN}"><si><t>after shared</t></si></sst>"#));

    let result = execute(json!({"schemaVersion":1,"operation":"diff","before":before,"after":after,"maxCells":1})).unwrap();
    assert_eq!(result["cellChanges"]["total"], 2);
    assert_eq!(result["cellChanges"]["details"].as_array().unwrap().len(), 1);
    assert_eq!(result["cellChanges"]["truncated"], true);
}

#[test]
fn diff_uses_resolved_style_components_instead_of_style_ids() {
    let dir = TempDir::new().unwrap();
    let before = dir.path().join("before.xlsx");
    let after = dir.path().join("after.xlsx");
    let sheet = format!(r#"<worksheet xmlns="{MAIN}"><sheetData><row r="1"><c r="A1" s="0" t="inlineStr"><is><t>same</t></is></c></row></sheetData></worksheet>"#);
    let left = styles(
        r#"<font><name val="Arial"/><sz val="10"/></font><font><name val="Calibri"/><sz val="11"/></font>"#,
        r#"<border><left/><right/><top/><bottom/></border><border><left style="thin"/><right/><top/><bottom/></border>"#,
        r#"<xf numFmtId="0" fontId="0" fillId="0" borderId="0"/><xf numFmtId="0" fontId="1" fillId="0" borderId="1"/>"#,
    );
    let right = styles_with_base(
        r#"<font><name val="Calibri"/><sz val="11"/></font><font><name val="Arial"/><sz val="10"/></font>"#,
        r#"<border><left style="thin"/><right/><top/><bottom/></border><border><left/><right/><top/><bottom/></border>"#,
        r#"<xf numFmtId="0" fontId="1" fillId="0" borderId="1"/>"#,
        r#"<xf numFmtId="0" fontId="1" fillId="0" borderId="1"/><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/>"#,
    );
    write_book(&before, &sheet, &left, &format!(r#"<sst xmlns="{MAIN}"/>"#));
    write_book(&after, &sheet, &right, &format!(r#"<sst xmlns="{MAIN}"/>"#));

    let result = execute(json!({"schemaVersion":1,"operation":"diff","before":before,"after":after})).unwrap();
    assert_eq!(result["cellChanges"]["total"], 0);
}

#[test]
fn text_style_copy_and_column_edits_preserve_unrequested_ooxml_state() {
    let dir = TempDir::new().unwrap();
    let source = dir.path().join("source.xlsx");
    let output = dir.path().join("candidate.xlsx");
    let copied = dir.path().join("copied.xlsx");
    let sheet = format!(r#"<worksheet xmlns="{MAIN}"><cols><col min="1" max="3" width="12" hidden="1" style="1" bestFit="1" outlineLevel="2"/></cols><sheetData><row r="1"><c r="A1" s="0" cm="7" vm="8" ph="1" t="inlineStr"><is><t xml:space="preserve"> old </t></is></c><c r="B1" s="1" t="inlineStr"><is><t>donor</t></is></c></row></sheetData></worksheet>"#);
    let styled = styles(
        r#"<font><name val="Arial"/><sz val="10"/></font><font><name val="Calibri"/><sz val="11"/></font>"#,
        r#"<border/><border/>"#,
        r#"<xf numFmtId="0" fontId="0" fillId="0" borderId="0"><alignment horizontal="right" textRotation="90" shrinkToFit="1"/></xf><xf numFmtId="0" fontId="1" fillId="0" borderId="1"/>"#,
    );
    write_book(&source, &sheet, &styled, &format!(r#"<sst xmlns="{MAIN}"/>"#));
    patch(&source, &output, json!([
        {"op":"setText","sheet":"UseCase","cell":"A1","expectedText":" old ","value":" new "},
        {"op":"setStyle","sheet":"UseCase","range":"A1","style":{"wrapText":true}},
        {"op":"columnWidth","sheet":"UseCase","column":"B","width":24}
    ])).unwrap();

    let sheet_out = part(&output, "xl/worksheets/sheet1.xml");
    let styles_out = part(&output, "xl/styles.xml");
    well_formed(&sheet_out);
    well_formed(&styles_out);
    assert!(sheet_out.contains(r#"cm="7""#));
    assert!(sheet_out.contains(r#"vm="8""#));
    assert!(sheet_out.contains(r#"ph="1""#));
    assert!(sheet_out.contains(r#"xml:space="preserve""#));
    let column_b = column_attributes(&sheet_out, "2");
    assert_eq!(column_b.get("max"), Some(&"2".to_owned()));
    for (key, value) in [("hidden", "1"), ("bestFit", "1"), ("outlineLevel", "2"), ("style", "1")] {
        assert_eq!(column_b.get(key), Some(&value.to_owned()), "B column lost {key}");
    }

    let inspected = inspect(&output, "UseCase!A1");
    assert_eq!(inspected["cells"][0]["text"], " new ");
    assert_eq!(inspected["cells"][0]["style"]["alignment"]["attributes"]["textRotation"], "90");
    assert_eq!(inspected["cells"][0]["style"]["alignment"]["attributes"]["shrinkToFit"], "1");

    patch(&source, &copied, json!([
        {"op":"copyStyle","sheet":"UseCase","range":"A1","fromCell":"B1","components":["alignment"]}
    ])).unwrap();
    let inspected = inspect(&copied, "UseCase!A1");
    assert!(inspected["cells"][0]["style"]["alignment"].is_null(), "copying donor absent alignment must clear target alignment");
}

#[test]
fn rejected_inputs_never_create_a_candidate() {
    let dir = TempDir::new().unwrap();
    let source = dir.path().join("source.xlsx");
    let output = dir.path().join("candidate.xlsx");
    let sheet = format!(r#"<worksheet xmlns="{MAIN}"><sheetData><row r="1"><c r="A1" s="0" t="inlineStr"><is><r><t>rich</t></r></is></c><c r="B1" s="0" t="inlineStr"><is><t>covered</t></is></c></row></sheetData><mergeCells count="1"><mergeCell ref="A1:B1"/></mergeCells></worksheet>"#);
    write_book(&source, &sheet, &ordinary_styles(), &format!(r#"<sst xmlns="{MAIN}"/>"#));

    for operations in [
        json!([]),
        json!([{"op":"setText","sheet":"UseCase","cell":"A1","expectedText":"rich","value":"no"}]),
        json!([{"op":"setStyle","sheet":"UseCase","range":"B1","style":{"bold":true}}]),
    ] {
        assert!(patch(&source, &output, operations).is_err());
        assert!(!output.exists());
    }
    File::create(&output).unwrap();
    assert!(patch(&source, &output, json!([{"op":"rowHeight","sheet":"UseCase","row":1,"height":20}])).is_err());
}

#[test]
fn duplicate_zip_parts_and_limits_are_rejected() {
    let dir = TempDir::new().unwrap();
    let duplicate = dir.path().join("duplicate.xlsx");
    let mut zip = ZipWriter::new(File::create(&duplicate).unwrap());
    zip.start_file("one.xml", SimpleFileOptions::default()).unwrap();
    zip.write_all(b"one").unwrap();
    zip.start_file("two.xml", SimpleFileOptions::default()).unwrap();
    zip.write_all(b"two").unwrap();
    zip.finish().unwrap();
    let mut bytes = fs::read(&duplicate).unwrap();
    for index in 0..=bytes.len() - b"one.xml".len() {
        if &bytes[index..index + b"one.xml".len()] == b"one.xml" { bytes[index..index + b"one.xml".len()].copy_from_slice(b"two.xml"); }
    }
    fs::write(&duplicate, bytes).unwrap();
    assert!(execute(json!({"schemaVersion":1,"operation":"inspect","source":duplicate,"ranges":["UseCase!A1"]})).is_err());

    let oversized = dir.path().join("oversized.xlsx");
    write_book(&oversized, &format!(r#"<worksheet xmlns="{MAIN}"><sheetData/></worksheet>"#), &ordinary_styles(), &format!(r#"<sst xmlns="{MAIN}"/>"#));
    assert!(execute(json!({"schemaVersion":1,"operation":"inspect","source":oversized,"ranges":["UseCase!A1"],"maxBytes":1})).is_err());
}

#[test]
fn concurrent_candidate_publish_is_noclobber_and_cleans_temporary_file() {
    let dir = TempDir::new().unwrap();
    let source = dir.path().join("source.xlsx");
    let output = dir.path().join("candidate.xlsx");
    let sheet = format!(r#"<worksheet xmlns="{MAIN}"><sheetData><row r="1"><c r="A1" s="0" t="inlineStr"><is><t>old</t></is></c></row></sheetData></worksheet>"#);
    write_book(&source, &sheet, &ordinary_styles(), &format!(r#"<sst xmlns="{MAIN}"/>"#));
    let hash = inspect(&source, "UseCase!A1")["sourceSha256"].clone();
    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for _ in 0..2 {
        let source = source.clone();
        let output = output.clone();
        let hash = hash.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            execute(json!({"schemaVersion":1,"operation":"patch","source":source,"output":output,"expectedSha256":hash,
                "operations":[{"op":"setText","sheet":"UseCase","cell":"A1","expectedText":"old","value":"new"}]})).is_ok()
        }));
    }
    assert_eq!(handles.into_iter().map(|handle| handle.join().unwrap()).filter(|success| *success).count(), 1);
    let candidate = inspect(&output, "UseCase!A1");
    assert_eq!(candidate["cells"][0]["text"], "new");
    let names = fs::read_dir(dir.path()).unwrap().map(|entry| entry.unwrap().file_name().into_string().unwrap()).collect::<Vec<_>>();
    assert_eq!(names.len(), 2, "failed candidate publication left a temporary file: {names:?}");
}

#[test]
fn expanded_limit_and_disabled_alignment_flag_reject_without_candidate() {
    let dir = TempDir::new().unwrap();
    let source = dir.path().join("source.xlsx");
    let output = dir.path().join("candidate.xlsx");
    let sheet = format!(r#"<worksheet xmlns="{MAIN}"><sheetData><row r="1"><c r="A1" s="0" t="inlineStr"><is><t>old</t></is></c></row></sheetData></worksheet>"#);
    let disabled = styles(
        r#"<font><name val="Arial"/><sz val="10"/></font><font><name val="Calibri"/><sz val="11"/></font>"#,
        r#"<border/><border/>"#,
        r#"<xf numFmtId="0" fontId="0" fillId="0" borderId="0" applyAlignment="0"/><xf numFmtId="0" fontId="1" fillId="0" borderId="1"/>"#,
    );
    write_book(&source, &sheet, &disabled, &format!(r#"<sst xmlns="{MAIN}"/>"#));
    let hash = inspect(&source, "UseCase!A1")["sourceSha256"].clone();
    assert!(execute(json!({"schemaVersion":1,"operation":"patch","source":source,"output":output,"expectedSha256":hash,
        "maxExpandedBytes":1,"operations":[{"op":"rowHeight","sheet":"UseCase","row":1,"height":20}]})).is_err());
    assert!(!output.exists());
    assert!(patch(&source, &output, json!([{"op":"setStyle","sheet":"UseCase","range":"A1","style":{"wrapText":true}}])).is_err());
    assert!(!output.exists());
}

#[test]
fn new_alignment_is_before_existing_protection() {
    let dir = TempDir::new().unwrap();
    let source = dir.path().join("source.xlsx");
    let output = dir.path().join("candidate.xlsx");
    let sheet = format!(r#"<worksheet xmlns="{MAIN}"><sheetData><row r="1"><c r="A1" s="0" t="inlineStr"><is><t>old</t></is></c></row></sheetData></worksheet>"#);
    let protected = styles(
        r#"<font><name val="Arial"/><sz val="10"/></font><font><name val="Calibri"/><sz val="11"/></font>"#,
        r#"<border/><border/>"#,
        r#"<xf numFmtId="0" fontId="0" fillId="0" borderId="0"><protection locked="0"/></xf><xf numFmtId="0" fontId="1" fillId="0" borderId="1"/>"#,
    );
    write_book(&source, &sheet, &protected, &format!(r#"<sst xmlns="{MAIN}"/>"#));
    patch(&source, &output, json!([{"op":"setStyle","sheet":"UseCase","range":"A1","style":{"wrapText":true}}])).unwrap();
    let xml = part(&output, "xl/styles.xml");
    well_formed(&xml);
    let alignment = xml.rfind("<alignment").unwrap();
    let protection = xml.rfind("<protection").unwrap();
    assert!(alignment < protection, "alignment must precede protection in xf child order");
}

#[test]
fn chained_edits_keep_physical_indexes_and_donor_styles_current() {
    let dir=TempDir::new().unwrap();
    let source=dir.path().join("source.xlsx");
    let output=dir.path().join("candidate.xlsx");
    let sheet=format!(r#"<worksheet xmlns="{MAIN}">
<!-- root whitespace and comments shift physical child indexes -->
<sheetData>
<!-- row comment -->
<row r="1">
  <c r="A1" s="0" t="inlineStr"><is><t>old</t></is></c>
  <!-- cell comment -->
  <c r="B1" s="1" t="inlineStr"><is><t>donor</t></is></c>
</row>
<row r="100"><c r="A100" s="0" t="inlineStr"><is><t>last</t></is></c></row>
</sheetData></worksheet>"#);
    write_book(&source,&sheet,&ordinary_styles(),&format!(r#"<sst xmlns="{MAIN}"/>"#));
    patch(&source,&output,json!([
        {"op":"columnWidth","sheet":"UseCase","column":"A","width":30},
        {"op":"setText","sheet":"UseCase","cell":"A1","expectedText":"old","value":"new"},
        {"op":"setStyle","sheet":"UseCase","range":"A1","style":{"fontColor":"0000FF","wrapText":true}},
        {"op":"copyStyle","sheet":"UseCase","range":"B1","fromCell":"A1","components":["font","alignment"]},
        {"op":"setStyle","sheet":"UseCase","range":"B1","style":{"bold":true}},
        {"op":"copyStyle","sheet":"UseCase","range":"A100","fromCell":"B1","components":["font","alignment"]},
        {"op":"rowHeight","sheet":"UseCase","row":100,"height":35},
        {"op":"rowVisibility","sheet":"UseCase","row":100,"hidden":true}
    ])).unwrap();
    let first=inspect(&output,"UseCase!A1:B1");
    let last=inspect(&output,"UseCase!A100");
    assert_eq!(first["cells"][0]["text"],"new");
    assert_eq!(last["cells"][0]["text"],"last");
    assert_eq!(last["cells"][0]["style"]["font"],first["cells"][1]["style"]["font"]);
    assert_ne!(last["cells"][0]["style"]["font"],first["cells"][0]["style"]["font"]);
    assert_eq!(last["cells"][0]["style"]["alignment"]["attributes"]["wrapText"],"1");
    assert_eq!(last["cells"][0]["row"]["ht"],"35");
    assert_eq!(last["cells"][0]["row"]["hidden"],"1");
    assert_eq!(last["cells"][0]["column"]["width"],"30");
    well_formed(&part(&output,"xl/worksheets/sheet1.xml"));
}

#[test]
fn inspect_caps_a_stable_prefix_across_overlapping_ranges() {
    let dir=TempDir::new().unwrap();let source=dir.path().join("source.xlsx");
    let sheet=format!(r#"<worksheet xmlns="{MAIN}"><sheetData><row r="1"><c r="A1" s="0"/></row></sheetData></worksheet>"#);
    write_book(&source,&sheet,&ordinary_styles(),&format!(r#"<sst xmlns="{MAIN}"/>"#));
    let result=execute(json!({"schemaVersion":1,"operation":"inspect","source":source,"ranges":["UseCase!A1:B1","UseCase!A1:A9000"],"maxCells":3})).unwrap();
    assert_eq!(result["totalCells"],9002);
    assert_eq!(result["truncated"],true);
    assert_eq!(result["cells"].as_array().unwrap().iter().map(|c|c["cell"].as_str().unwrap()).collect::<Vec<_>>(),["A1","B1","A1"]);
}
