use super::*;
use std::{fs, io::Write, path::Path};
use tempfile::TempDir;
use xml::{Document, Element};
use zip::{ZipWriter, write::SimpleFileOptions};

const MAIN: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

fn styles_xml() -> String {
    format!(
        r#"<styleSheet xmlns="{MAIN}"><fonts count="2"><font><sz val="10"/><name val="Arial"/></font><font><name val="Yu Gothic"/><sz val="12"/><color theme="2" tint="0.25"/><b/><i/><strike/><u/><family val="2"/><charset val="128"/><scheme val="minor"/></font></fonts><fills count="2"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="solid"><fgColor rgb="FFAABBCC"/></patternFill></fill></fills><borders count="2"><border/><border><left style="thin"/><right style="double"/><top/><bottom style="dashed"/></border></borders><cellStyleXfs count="2"><xf/><xf fontId="1"/></cellStyleXfs><cellXfs count="3"><xf/><xf fontId="1" fillId="1" borderId="1" numFmtId="49" xfId="1" applyFont="1" applyFill="0" applyBorder="1" applyNumberFormat="0" applyAlignment="1" applyProtection="1" quotePrefix="1"><alignment horizontal="right" vertical="top" wrapText="1"/><protection locked="0"/><extLst><ext uri="test"><custom/></ext></extLst></xf><xf fontId="1" xfId="0"/></cellXfs></styleSheet>"#
    )
}
fn fixture(sheet: &str, styles: &str, shared: &str) -> (TempDir, std::path::PathBuf) {
    let dir = TempDir::new().unwrap();
    let source = dir.path().join("source.xlsx");
    let mut zip = ZipWriter::new(fs::File::create(&source).unwrap());
    let workbook = format!(
        r#"<workbook xmlns="{MAIN}" xmlns:r="{REL}"><sheets><sheet name="Data" sheetId="1" r:id="s"/></sheets></workbook>"#
    );
    let rels = format!(
        r#"<Relationships><Relationship Id="s" Type="{REL}/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="style" Type="{REL}/styles" Target="styles.xml"/><Relationship Id="shared" Type="{REL}/sharedStrings" Target="sharedStrings.xml"/></Relationships>"#
    );
    let sheet = format!(r#"<worksheet xmlns="{MAIN}">{sheet}</worksheet>"#);
    let shared = format!(r#"<sst xmlns="{MAIN}">{shared}</sst>"#);
    for (name, content) in [
        ("xl/workbook.xml", workbook.as_str()),
        ("xl/_rels/workbook.xml.rels", rels.as_str()),
        ("xl/worksheets/sheet1.xml", sheet.as_str()),
        ("xl/styles.xml", styles),
        ("xl/sharedStrings.xml", shared.as_str()),
    ] {
        zip.start_file(name, SimpleFileOptions::default()).unwrap();
        zip.write_all(content.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
    (dir, source)
}
fn package(path: &Path) -> package::Package {
    package::Package::read(path, package::Limits::new(None, None).unwrap()).unwrap()
}
fn patch(source: &Path, output: Option<&Path>, ops: Value, validate: bool) -> Result<Value> {
    execute(
        json!({"schemaVersion":1,"operation":"patch","source":source,"output":output,
        "expectedSha256":package(source).sha(),"operations":ops,"validateOnly":validate}),
    )
}
fn doc(path: &Path, part: &str) -> Document {
    package(path).xml(part).unwrap()
}
fn inspect(source: &Path, ranges: &[&str], compact: bool) -> Value {
    execute(
        json!({"schemaVersion":1,"operation":"inspect","source":source,"ranges":ranges,
        "detail":if compact {"compact"} else {"full"}}),
    )
    .unwrap()
}
fn cells(doc: &Document) -> Vec<&Element> {
    doc.root()
        .unwrap()
        .child("sheetData")
        .unwrap()
        .elements()
        .flat_map(Element::elements)
        .filter(|e| e.local_name() == "c")
        .collect()
}

#[test]
fn set_number_checks_raw_text_and_preserves_style_and_metadata() {
    let (dir, source) = fixture(
        r#"<sheetData><row r="1"><c r="A1" s="2" t="inlineStr" cm="7"><is><t>42</t></is><extLst/></c><c r="B1"><v>4.20E1</v></c></row></sheetData>"#,
        &styles_xml(),
        "",
    );
    let output = dir.path().join("number.xlsx");
    patch(&source, Some(&output), json!([
        {"op":"setNumber","sheet":"Data","cell":"A1","expectedText":"42","value":42.0},
        {"op":"setNumber","sheet":"Data","cell":"B1","expectedText":"4.20E1","value":1.2345678901234567},
        {"op":"setNumber","sheet":"Data","cell":"C2","expectedText":"","value":-9}
    ]), false).unwrap();
    let sheet = doc(&output, "xl/worksheets/sheet1.xml");
    let c = cells(&sheet)[0];
    assert_eq!(c.attrs["s"], "2");
    assert_eq!(c.attrs["cm"], "7");
    assert!(!c.attrs.contains_key("t"));
    assert!(c.child("is").is_none());
    assert_eq!(c.child("v").unwrap().text(), "42");
    assert_eq!(
        c.elements().map(Element::local_name).collect::<Vec<_>>(),
        ["v", "extLst"]
    );
    let view = inspect(&output, &["Data!A1:C2"], false);
    assert_eq!(view["cells"][0]["kind"], "n");
    assert_eq!(view["cells"][0]["value"], "42");
    assert_eq!(view["cells"][1]["value"], "1.2345678901234567");
    let mut book = workbook::Book::load(&package(&source)).unwrap();
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(
            book.apply_bounded(
                &Operation::SetNumber {
                    sheet: "Data".into(),
                    cell: "A1".into(),
                    expected_text: "42".into(),
                    value
                },
                10000,
                true
            )
            .is_err()
        );
    }
    assert!(
        patch(
            &source,
            None,
            json!([{"op":"setNumber","sheet":"Data","cell":"B1","expectedText":"42","value":3}]),
            true
        )
        .unwrap()["violations"]
            .as_array()
            .unwrap()
            .len()
            == 1
    );
}

#[test]
fn append_text_copies_font_with_run_order_overrides_and_prefixes() {
    let (dir, source) = fixture(
        r#"<sheetData><row r="1" s="2" customFormat="1"><c r="A1" s="1" t="s"><v>0</v></c></row></sheetData>"#,
        &styles_xml(),
        "<si><t>old</t></si>",
    );
    let output = dir.path().join("rich.xlsx");
    patch(&source, Some(&output), json!([
        {"op":"appendText","sheet":"Data","cell":"A1","expectedText":"old","value":" new ","fontColor":"abcdef","bold":false,"strike":false},
        {"op":"appendText","sheet":"Data","cell":"B1","expectedText":"","value":"only","fontColor":"80123456","bold":true}
    ]), false).unwrap();
    let sheet = doc(&output, "xl/worksheets/sheet1.xml");
    let c = cells(&sheet);
    assert_eq!(c[0].attrs["s"], "1");
    assert_eq!(c[1].attrs["s"], "2");
    assert!(c[0].child("v").is_none());
    let runs: Vec<_> = c[0].child("is").unwrap().elements().collect();
    assert_eq!(runs.len(), 2);
    for r in &runs {
        assert_eq!(r.child("t").unwrap().attrs["xml:space"], "preserve");
        let pr = r.child("rPr").unwrap();
        assert_eq!(
            pr.elements().map(Element::local_name).collect::<Vec<_>>(),
            [
                "rFont", "charset", "family", "b", "i", "strike", "color", "sz", "u", "scheme"
            ]
        );
        assert_eq!(pr.child("rFont").unwrap().attrs["val"], "Yu Gothic");
        assert_eq!(pr.child("sz").unwrap().attrs["val"], "12");
    }
    let old = runs[0].child("rPr").unwrap();
    assert_eq!(old.child("color").unwrap().attrs["theme"], "2");
    let new = runs[1].child("rPr").unwrap();
    assert_eq!(new.child("color").unwrap().attrs["rgb"], "FFABCDEF");
    assert_eq!(new.child("b").unwrap().attrs["val"], "0");
    assert_eq!(new.child("strike").unwrap().attrs["val"], "0");
    assert_eq!(c[1].child("is").unwrap().elements().count(), 1);
    let view = inspect(&output, &["Data!A1"], true);
    assert_eq!(view["cells"][0]["text"], "old new ");
    assert_eq!(view["cells"][0]["richText"], true);
    let prefixed = xml::parse(&styles_xml()).unwrap();
    // Worksheet and style parts can have different prefixes.
    let parent = Element::new("s:c");
    let pr = styles::run_properties(&prefixed, 1, &parent, &Style::default()).unwrap();
    assert_eq!(pr.name, "s:rPr");
    assert!(pr.elements().all(|e| e.name.starts_with("s:")));
}

#[test]
fn compact_inspect_shape_parts_defaults_and_capabilities() {
    let (_dir, source) = fixture(
        r#"<cols><col min="1" max="3" width="24"/></cols><sheetData><row r="1" ht="30" customHeight="1"><c r="A1" s="1" t="inlineStr"><is><t>hello</t></is></c><c r="B1"><f>2</f><v>2</v></c></row></sheetData>"#,
        &styles_xml(),
        "",
    );
    let compact = inspect(&source, &["Data!A1:C1"], true);
    assert!(compact.get("styleContext").is_none());
    assert!(compact.get("parts").is_none());
    let cell = &compact["cells"][0];
    assert_eq!(cell["styleId"], 1);
    assert_eq!(cell["baseStyleId"], 1);
    assert_eq!(
        cell["style"]["font"],
        json!({"name":"Yu Gothic","size":12.0,"color":{"theme":2,"tint":0.25},"bold":true,"italic":true,"strike":true})
    );
    assert_eq!(cell["style"]["fill"]["color"], json!({"rgb":"FFAABBCC"}));
    assert_eq!(
        cell["style"]["border"],
        json!({"left":"thin","right":"double","bottom":"dashed"})
    );
    assert_eq!(
        cell["style"]["alignment"],
        json!({"horizontal":"right","vertical":"top","wrapText":true})
    );
    assert_eq!(cell["style"]["numberFormat"], json!({"builtInId":49}));
    assert_eq!(cell["row"], json!({"height":30.0,"customHeight":true}));
    assert_eq!(cell["column"], json!({"width":24.0}));
    assert_eq!(compact["cells"][1]["formula"], true);
    assert_eq!(compact["cells"][2]["blank"], true);
    assert_eq!(
        compact["protocolFeatures"],
        json!([
            "createCells",
            "setNumber",
            "appendText",
            "copyStyleAdoptBase",
            "validateOnly",
            "compactInspect",
            "setFormula",
            "insertRows"
        ])
    );
    let full = inspect(&source, &["Data!A1:C1"], false);
    assert!(full.get("styleContext").is_some());
    assert!(full.get("parts").is_some());
    assert!(encoded_len(&compact).unwrap() < encoded_len(&full).unwrap());
    for (detail, include) in [("full", false), ("compact", true)] {
        let result = execute(json!({"schemaVersion":1,"operation":"inspect","source":source,"ranges":["Data!A1"],"detail":detail,"includeParts":include})).unwrap();
        assert_eq!(result.get("parts").is_some(), include);
    }
}

#[test]
fn validate_requires_sha_ignores_output_and_retains_envelope() {
    let (_dir, source) = fixture("<sheetData/>", &styles_xml(), "");
    let request = json!({"schemaVersion":1,"operation":"patch","source":source,"validateOnly":true,
        "expectedSha256":package(&source).sha(),"operations":[{"op":"rowVisibility","sheet":"Data","row":7,"hidden":true}]});
    let result = execute(request.clone()).unwrap();
    assert_eq!(result["operation"], "patch");
    assert_eq!(result["schemaVersion"], 1);
    assert_eq!(result["visualVerification"], "pending");
    assert!(result["proofBoundary"].as_str().is_some());
    assert_eq!(result["protocolFeatures"].as_array().unwrap().len(), 8);
    let mut existing = request.clone();
    existing["output"] = json!(source);
    assert_eq!(execute(existing).unwrap(), result);
    let mut no_sha = request;
    no_sha.as_object_mut().unwrap().remove("expectedSha256");
    assert!(
        execute(no_sha)
            .unwrap_err()
            .to_string()
            .contains("expectedSha256")
    );
}

#[test]
fn compact_colors_custom_number_format_and_full_default_contract() {
    let styles = styles_xml()
        .replace(
            "<fonts",
            "<numFmts count=\"1\"><numFmt numFmtId=\"165\" formatCode=\"0.000\"/></numFmts><fonts",
        )
        .replace("numFmtId=\"49\"", "numFmtId=\"165\"")
        .replace("theme=\"2\" tint=\"0.25\"", "indexed=\"0\"");
    let (_dir, source) = fixture(
        "<sheetData><row r=\"1\"><c r=\"A1\" s=\"1\"/></row></sheetData>",
        &styles,
        "",
    );
    let compact = inspect(&source, &["Data!A1"], true);
    assert_eq!(
        compact["cells"][0]["style"]["font"]["color"],
        json!({"indexed":0})
    );
    assert_eq!(
        compact["cells"][0]["style"]["numberFormat"],
        json!({"formatCode":"0.000"})
    );
    let default = execute(
        json!({"schemaVersion":1,"operation":"inspect","source":source,"ranges":["Data!A1"]}),
    )
    .unwrap();
    assert_eq!(default, inspect(&source, &["Data!A1"], false));
    let diff =
        execute(json!({"schemaVersion":1,"operation":"diff","before":source,"after":source}))
            .unwrap();
    assert_eq!(diff["protocolFeatures"], default["protocolFeatures"]);
}

#[test]
fn validation_target_budget_includes_creations_and_failure_does_not_consume_budget() {
    let (_dir, source) = fixture("<sheetData/>", &styles_xml(), "");
    let result = patch(
        &source,
        None,
        json!([
            {"op":"setStyle","sheet":"Data","range":"A1:A10000","style":{"wrapText":true}},
            {"op":"setText","sheet":"Data","cell":"B1","expectedText":"","value":"over budget"},
            {"op":"rowVisibility","sheet":"Data","row":10001,"hidden":true}
        ]),
        true,
    )
    .unwrap();
    assert_eq!(result["targetsProcessed"], 10000);
    assert_eq!(result["violations"].as_array().unwrap().len(), 2);
    assert!(
        result["violations"][0]["message"]
            .as_str()
            .unwrap()
            .contains("target budget")
    );
}

#[test]
fn signed_workbooks_still_refuse_new_operations() {
    let (_dir, source) = fixture("<sheetData/>", &styles_xml(), "");
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&source)
        .unwrap();
    let mut zip = ZipWriter::new_append(file).unwrap();
    zip.start_file("_xmlsignatures/sig1.xml", SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"<Signature/>").unwrap();
    zip.finish().unwrap();
    let report = patch(
        &source,
        None,
        json!([
            {"op":"setNumber","sheet":"Data","cell":"A1","expectedText":"","value":42},
            {"op":"appendText","sheet":"Data","cell":"A1","expectedText":"","value":"text"},
            {"op":"setFormula","sheet":"Data","cell":"A1","expectedText":"","formula":"1"}
        ]),
        true,
    )
    .unwrap();
    assert_eq!(report["violations"].as_array().unwrap().len(), 3);
    assert!(
        report["violations"][0]["message"]
            .as_str()
            .unwrap()
            .contains("digitally signed")
    );
    assert_eq!(report["wouldChangeParts"], json!([]));
}
