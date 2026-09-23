use std::fs;
use std::io::Write as _;
use std::path::Path;

use assert_cmd::Command;
use serde_json::{Value, json};
use tempfile::TempDir;
use zip::{ZipWriter, write::SimpleFileOptions};

fn workbook(path: &Path) {
    workbook_with(path, None, None);
}

fn workbook_with(path: &Path, sheet: Option<&str>, styles: Option<&str>) {
    let mut zip = ZipWriter::new(fs::File::create(path).unwrap());
    for (name, data) in [
        (
            "[Content_Types].xml",
            r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>"#,
        ),
        (
            "_rels/.rels",
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#,
        ),
        (
            "xl/workbook.xml",
            r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="UseCase" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
        ),
        (
            "xl/_rels/workbook.xml.rels",
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/></Relationships>"#,
        ),
        (
            "xl/styles.xml",
            r#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><fonts count="1"><font><sz val="10"/><name val="Arial"/><color rgb="FF000000"/></font></fonts><fills count="2"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill></fills><borders count="1"><border><left/><right/><top/><bottom/></border></borders><cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs><cellXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"><alignment vertical="top"/></xf></cellXfs></styleSheet>"#,
        ),
        (
            "xl/sharedStrings.xml",
            r#"<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="2" uniqueCount="1"><si><t>hello</t></si></sst>"#,
        ),
        (
            "xl/worksheets/sheet1.xml",
            r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1:B2"/><sheetFormatPr defaultRowHeight="15"/><cols><col min="1" max="3" width="12" customWidth="1"/></cols><sheetData><row r="1" ht="15" customHeight="1"><c r="A1" s="0" t="s"><v>0</v></c><c r="B1" s="0" t="inlineStr"><is><t>donor</t></is></c></row><row r="2"><c r="A2" s="0" t="s"><v>0</v></c></row></sheetData></worksheet>"#,
        ),
        ("xl/media/reference.bin", "untouched-media"),
        ("custom/unknown.xml", "<private>keep exactly</private>"),
    ] {
        let data = match name {
            "xl/worksheets/sheet1.xml" => sheet.unwrap_or(data),
            "xl/styles.xml" => styles.unwrap_or(data),
            _ => data,
        };
        zip.start_file(name, SimpleFileOptions::default()).unwrap();
        zip.write_all(data.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
}

fn machine(request: Value, success: bool) -> Value {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("opsail"));
    command
        .args(["xlsx", "--machine"])
        .write_stdin(request.to_string());
    let assertion = if success {
        command.assert().success()
    } else {
        command.assert().code(2)
    };
    let response: Value = serde_json::from_slice(&assertion.get_output().stdout).unwrap();
    assert_eq!(response["schemaVersion"], 1);
    if success {
        assert_eq!(
            response["protocolFeatures"],
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
    }
    response
}

fn read_part(path: &Path, name: &str) -> String {
    use std::io::Read as _;
    let mut zip = zip::ZipArchive::new(fs::File::open(path).unwrap()).unwrap();
    let mut text = String::new();
    zip.by_name(name)
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    text
}

fn inspect_machine(source: &Path, range: &str, detail: &str) -> Value {
    machine(
        json!({"schemaVersion":1,"operation":"inspect","source":source,"ranges":[range],"detail":detail}),
        true,
    )
}

fn patch_request(source: &Path, output: &Path, operations: Value) -> Value {
    let sha = inspect_machine(source, "UseCase!A1", "compact")["sourceSha256"].clone();
    json!({"schemaVersion":1,"operation":"patch","source":source,"output":output,"expectedSha256":sha,"operations":operations})
}

#[test]
fn real_cli_inspect_patch_diff_preserves_source_and_shared_string_neighbour() {
    let dir = TempDir::new().unwrap();
    let source = dir.path().join("source.xlsx");
    let output = dir.path().join("candidate.xlsx");
    workbook(&source);
    let source_bytes = fs::read(&source).unwrap();
    let inspected = machine(
        json!({ "schemaVersion":1, "operation":"inspect", "source":source, "ranges":["UseCase!A1:B2"] }),
        true,
    );
    let request = json!({
        "schemaVersion":1, "operation":"patch", "source":source, "output":output,
        "expectedSha256":inspected["sourceSha256"],
        "operations":[
            {"op":"setText","sheet":"UseCase","cell":"A1","expectedText":"hello","value":"new <text> & content"},
            {"op":"setStyle","sheet":"UseCase","range":"A1","style":{"fontColor":"0000FF","bold":true,"wrapText":true}},
            {"op":"rowHeight","sheet":"UseCase","row":1,"height":42},
            {"op":"columnWidth","sheet":"UseCase","column":"A","width":30}
        ]
    });
    let patched = machine(request.clone(), true);
    assert!(output.exists());
    assert_eq!(fs::read(&source).unwrap(), source_bytes);
    assert_eq!(patched["visualVerification"], "pending");
    assert!(
        patched["candidateSha256"]
            .as_str()
            .is_some_and(|s| s.len() == 64)
    );
    let mut zip = zip::ZipArchive::new(fs::File::open(&output).unwrap()).unwrap();
    use std::io::Read as _;
    let mut shared = String::new();
    zip.by_name("xl/sharedStrings.xml")
        .unwrap()
        .read_to_string(&mut shared)
        .unwrap();
    assert!(shared.contains("<t>hello</t>"));
    let compared = machine(
        json!({ "schemaVersion":1, "operation":"diff", "before":source, "after":output }),
        true,
    );
    assert_eq!(compared["visualVerification"], "pending");
    assert_ne!(compared["beforeSha256"], compared["afterSha256"]);
    let before_retry = fs::read(&output).unwrap();
    let error = machine(request, false);
    assert!(error["error"]["message"].as_str().is_some());
    assert_eq!(fs::read(&output).unwrap(), before_retry);
}

#[test]
fn rejected_machine_requests_never_create_candidates() {
    let dir = TempDir::new().unwrap();
    let source = dir.path().join("source.xlsx");
    let output = dir.path().join("candidate.xlsx");
    workbook(&source);
    let base = json!({ "schemaVersion":1,"operation":"patch","source":source,"output":output,
        "expectedSha256":"0".repeat(64),"operations":[{"op":"rowHeight","sheet":"UseCase","row":1,"height":42}] });
    let rejected = machine(base.clone(), false);
    assert!(rejected["error"]["message"].as_str().is_some());
    assert!(!output.exists());
    machine(
        json!({"schemaVersion":1,"operation":"diff","before":source}),
        false,
    );
    machine(
        json!({"schemaVersion":1,"operation":"inspect","source":source,"ranges":["UseCase!A1"],"maxCells":2001}),
        false,
    );
    let mut wrong = base;
    wrong["unexpected"] = json!(true);
    machine(wrong, false);
    assert!(!output.exists());
}

#[test]
fn human_cli_accepts_plan_file_and_matches_machine_protocol() {
    let dir = TempDir::new().unwrap();
    let source = dir.path().join("source.xlsx");
    let output = dir.path().join("candidate.xlsx");
    let plan = dir.path().join("plan.json");
    workbook(&source);
    let result = Command::new(assert_cmd::cargo::cargo_bin!("opsail"))
        .args(["xlsx", "inspect"])
        .arg(&source)
        .args(["--range", "UseCase!A1"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let inspected: Value = serde_json::from_slice(&result).unwrap();
    fs::write(&plan, json!({"expectedSha256":inspected["sourceSha256"],"operations":[{"op":"rowHeight","sheet":"UseCase","row":1,"height":30}]}).to_string()).unwrap();
    Command::new(assert_cmd::cargo::cargo_bin!("opsail"))
        .args(["xlsx", "patch"])
        .arg(&source)
        .arg("--plan")
        .arg(&plan)
        .arg("--output")
        .arg(&output)
        .assert()
        .success();
    Command::new(assert_cmd::cargo::cargo_bin!("opsail"))
        .args(["xlsx", "diff"])
        .arg(&source)
        .arg(&output)
        .assert()
        .success();
}

#[test]
fn machine_creates_missing_cells_and_rows_then_reloads_the_candidate() {
    let dir = TempDir::new().unwrap();
    let source = dir.path().join("source.xlsx");
    let output = dir.path().join("candidate.xlsx");
    workbook(&source);
    let original = fs::read(&source).unwrap();
    let untouched = r#"<row r="2"><c r="A2" s="0" t="s"><v>0</v></c></row>"#;
    assert!(read_part(&source, "xl/worksheets/sheet1.xml").contains(untouched));
    let report = machine(
        patch_request(
            &source,
            &output,
            json!([
                {"op":"setText","sheet":"UseCase","cell":"C4","expectedText":"","value":"created"},
                {"op":"setNumber","sheet":"UseCase","cell":"A4","expectedText":"","value":42.0},
                {"op":"copyStyle","sheet":"UseCase","range":"B4","fromCell":"B1","components":["font","fill","border","alignment","numberFormat"]},
                {"op":"setStyle","sheet":"UseCase","range":"D3","style":{"bold":true}},
                {"op":"rowHeight","sheet":"UseCase","row":8,"height":30},
                {"op":"rowVisibility","sheet":"UseCase","row":7,"hidden":true},
                {"op":"setText","sheet":"UseCase","cell":"A1","expectedText":"hello","value":"after insertions"}
            ]),
        ),
        true,
    );
    assert_eq!(report["targetsProcessed"], 7);
    assert_eq!(report["operationsApplied"], 7);
    let sheet = read_part(&output, "xl/worksheets/sheet1.xml");
    assert!(sheet.contains(untouched));
    assert!(sheet.contains(r#"<dimension ref="A1:D4"/>"#));
    assert!(sheet.contains(r#"hidden="1" r="7""#));
    let inspected = inspect_machine(&output, "UseCase!A4:C4", "compact");
    assert_eq!(inspected["cells"][0]["kind"], "n");
    assert_eq!(inspected["cells"][0]["value"], "42");
    assert_eq!(inspected["cells"][1]["styleId"], 0);
    assert_eq!(inspected["cells"][2]["text"], "created");
    assert_eq!(
        inspect_machine(&output, "UseCase!A1", "compact")["cells"][0]["text"],
        "after insertions"
    );
    assert_eq!(
        inspect_machine(&output, "UseCase!D3", "compact")["cells"][0]["style"]["font"]["bold"],
        true
    );
    assert_eq!(
        inspect_machine(&output, "UseCase!A8", "compact")["cells"][0]["row"]["height"],
        30.0
    );
    assert_eq!(fs::read(&source).unwrap(), original);
}

#[test]
fn machine_append_text_preserves_stored_font_and_shared_string_neighbour() {
    let dir = TempDir::new().unwrap();
    let source = dir.path().join("source.xlsx");
    let output = dir.path().join("candidate.xlsx");
    workbook(&source);
    machine(
        patch_request(
            &source,
            &output,
            json!([
                {"op":"appendText","sheet":"UseCase","cell":"A1","expectedText":"hello","value":" world <&>","fontColor":"123456","bold":true,"strike":true},
                {"op":"appendText","sheet":"UseCase","cell":"B3","expectedText":"","value":"new","fontColor":"80abcdef","bold":false}
            ]),
        ),
        true,
    );
    let inspected = inspect_machine(&output, "UseCase!A1:A2", "compact");
    assert_eq!(inspected["cells"][0]["text"], "hello world <&>");
    assert_eq!(inspected["cells"][0]["richText"], true);
    assert_eq!(inspected["cells"][0]["styleId"], 0);
    assert_eq!(inspected["cells"][1]["text"], "hello");
    let sheet = read_part(&output, "xl/worksheets/sheet1.xml");
    assert_eq!(sheet.matches("<rPr>").count(), 3);
    assert_eq!(sheet.matches(r#"<rFont val="Arial"/>"#).count(), 3);
    assert_eq!(sheet.matches(r#"<sz val="10"/>"#).count(), 3);
    assert!(sheet.contains(r#"<color rgb="FF123456"/>"#));
    assert!(sheet.contains(r#"<color rgb="80ABCDEF"/>"#));
    assert_eq!(
        read_part(&source, "xl/styles.xml"),
        read_part(&output, "xl/styles.xml")
    );
    assert_eq!(
        read_part(&source, "xl/sharedStrings.xml"),
        read_part(&output, "xl/sharedStrings.xml")
    );
    let rejected = machine(
        patch_request(
            &output,
            &dir.path().join("rejected.xlsx"),
            json!([
                {"op":"appendText","sheet":"UseCase","cell":"A1","expectedText":"hello world <&>","value":"again"}
            ]),
        ),
        false,
    );
    assert_eq!(rejected["error"]["operationIndex"], 0);
    assert_eq!(rejected["error"]["op"], "appendText");
    assert!(
        rejected["error"]["message"]
            .as_str()
            .unwrap()
            .contains("rich text")
    );
}

#[test]
fn machine_validation_collects_errors_without_output_and_patch_errors_are_locatable() {
    let dir = TempDir::new().unwrap();
    let source = dir.path().join("source.xlsx");
    let output = dir.path().join("candidate.xlsx");
    workbook(&source);
    let original = fs::read(&source).unwrap();
    let mut request = patch_request(
        &source,
        &output,
        json!([
            {"op":"setText","sheet":"UseCase","cell":"A4","expectedText":"","value":"new"},
            {"op":"setNumber","sheet":"UseCase","cell":"A4","expectedText":"wrong","value":5},
            {"op":"appendText","sheet":"UseCase","cell":"A4","expectedText":"new","value":" appended"},
            {"op":"rowHeight","sheet":"UseCase","row":10,"height":500},
            {"op":"setStyle","sheet":"UseCase","range":"B4","style":{"fontColor":"zzzzzz"}},
            {"op":"rowVisibility","sheet":"UseCase","row":10,"hidden":true}
        ]),
    );
    request["validateOnly"] = json!(true);
    request.as_object_mut().unwrap().remove("output");
    let validated = machine(request.clone(), true);
    assert_eq!(validated["operationsChecked"], 6);
    assert_eq!(validated["targetsProcessed"], 3);
    assert_eq!(validated["violations"].as_array().unwrap().len(), 3);
    assert_eq!(validated["violations"][0]["operationIndex"], 1);
    assert_eq!(validated["violations"][1]["target"], "10");
    assert_eq!(validated["violations"][2]["target"], "B4");
    assert_eq!(
        validated["wouldChangeParts"],
        json!(["xl/worksheets/sheet1.xml"])
    );
    assert!(validated.get("output").is_none());
    assert!(!output.exists());
    request["output"] = json!(output);
    assert_eq!(machine(request.clone(), true), validated);
    request["validateOnly"] = json!(false);
    let rejected = machine(request.clone(), false);
    assert_eq!(rejected["error"]["operationIndex"], 1);
    assert_eq!(rejected["error"]["op"], "setNumber");
    assert_eq!(rejected["error"]["sheet"], "UseCase");
    assert_eq!(rejected["error"]["target"], "A4");
    assert!(
        rejected["error"]["message"]
            .as_str()
            .unwrap()
            .contains("operation 1 (setNumber UseCase!A4)")
    );
    assert!(!output.exists());
    request["validateOnly"] = json!(true);
    request["expectedSha256"] = json!("0".repeat(64));
    machine(request, false);
    assert_eq!(fs::read(&source).unwrap(), original);
}

#[test]
fn machine_copy_all_five_adopts_different_base_style_entirely() {
    let dir = TempDir::new().unwrap();
    let source = dir.path().join("source.xlsx");
    let output = dir.path().join("candidate.xlsx");
    let styles = r#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><fonts count="1"><font><name val="Arial"/><sz val="10"/></font></fonts><fills count="1"><fill><patternFill patternType="none"/></fill></fills><borders count="1"><border/></borders><cellStyleXfs count="2"><xf/><xf numFmtId="4"/></cellStyleXfs><cellXfs count="2"><xf xfId="0"/><xf xfId="1" numFmtId="4" applyNumberFormat="1" applyProtection="1" quotePrefix="1"><alignment horizontal="right"/><protection locked="0"/><extLst><ext uri="keep"/></extLst></xf></cellXfs></styleSheet>"#;
    let sheet = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1"/><c r="B1" s="1"/></row></sheetData></worksheet>"#;
    workbook_with(&source, Some(sheet), Some(styles));
    let partial = machine(
        patch_request(
            &source,
            &output,
            json!([
                {"op":"copyStyle","sheet":"UseCase","range":"A1","fromCell":"B1","components":["numberFormat"]}
            ]),
        ),
        false,
    );
    let message = partial["error"]["message"].as_str().unwrap();
    for text in [
        "target UseCase!A1",
        "donor UseCase!B1",
        "all five components",
        "adopt the donor cell style entirely",
    ] {
        assert!(message.contains(text));
    }
    machine(
        patch_request(
            &source,
            &output,
            json!([
                {"op":"copyStyle","sheet":"UseCase","range":"A1","fromCell":"B1","components":["font","fill","border","alignment","numberFormat"]}
            ]),
        ),
        true,
    );
    let inspected = inspect_machine(&output, "UseCase!A1:B1", "full");
    assert_eq!(
        inspected["cells"][0]["style"],
        inspected["cells"][1]["style"]
    );
    assert_eq!(
        inspect_machine(&output, "UseCase!A1", "compact")["cells"][0]["baseStyleId"],
        1
    );
    assert_eq!(
        read_part(&source, "xl/styles.xml"),
        read_part(&output, "xl/styles.xml")
    );
}

#[test]
fn machine_compact_shape_parts_defaults_and_53_cell_byte_measurement() {
    let dir = TempDir::new().unwrap();
    let source = dir.path().join("source.xlsx");
    let mut sheet = String::from(
        r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>"#,
    );
    for row in 1..=53 {
        sheet.push_str(&format!(r#"<row r="{row}"><c r="A{row}" s="0" t="inlineStr"><is><t>styled cell {row}</t></is></c></row>"#));
    }
    sheet.push_str("</sheetData></worksheet>");
    workbook_with(&source, Some(&sheet), None);
    let full = inspect_machine(&source, "UseCase!A1:A53", "full");
    let compact = inspect_machine(&source, "UseCase!A1:A53", "compact");
    let full_bytes = serde_json::to_vec(&full).unwrap().len();
    let compact_bytes = serde_json::to_vec(&compact).unwrap().len();
    println!(
        "CLI 53 styled cells: full={full_bytes} bytes compact={compact_bytes} bytes saved={} bytes ({:.2}%), {:.2}x",
        full_bytes - compact_bytes,
        100.0 * (full_bytes - compact_bytes) as f64 / full_bytes as f64,
        full_bytes as f64 / compact_bytes as f64
    );
    assert!(compact_bytes < full_bytes);
    assert_eq!(compact["totalCells"], 53);
    assert_eq!(compact["truncated"], false);
    assert!(full.get("parts").is_some());
    assert!(full.get("styleContext").is_some());
    assert!(compact.get("parts").is_none());
    assert!(compact.get("styleContext").is_none());
    assert_eq!(compact["sheets"], full["sheets"]);
    assert_eq!(compact["sourceSha256"], full["sourceSha256"]);
    for (index, cell) in compact["cells"].as_array().unwrap().iter().enumerate() {
        assert_eq!(cell["sheet"], "UseCase");
        assert_eq!(cell["cell"], format!("A{}", index + 1));
        assert_eq!(cell["kind"], "string");
        assert_eq!(cell["styleId"], 0);
        assert_eq!(cell["text"], format!("styled cell {}", index + 1));
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
            assert!(cell.get(key).is_none());
        }
        assert_eq!(
            cell["style"]["font"],
            json!({"name":"Arial","size":10.0,"color":{"rgb":"FF000000"}})
        );
        assert!(cell["style"].get("numberFormat").is_none());
    }
    for (detail, include_parts) in [("compact", true), ("full", false)] {
        let report = machine(
            json!({"schemaVersion":1,"operation":"inspect","source":source,"ranges":["UseCase!A1"],"detail":detail,"includeParts":include_parts}),
            true,
        );
        assert_eq!(report.get("parts").is_some(), include_parts);
    }
}

#[test]
fn machine_operation_errors_keep_index_op_sheet_and_target() {
    let dir = TempDir::new().unwrap();
    let source = dir.path().join("source.xlsx");
    let output = dir.path().join("never.xlsx");
    workbook(&source);
    let before = machine(
        json!({"schemaVersion":1,"operation":"inspect","source":source,"ranges":["UseCase!A1"]}),
        true,
    );
    let mut operations: Vec<_> = (0..12)
        .map(|_| json!({"op":"rowHeight","sheet":"UseCase","row":1,"height":20}))
        .collect();
    operations.push(
        json!({"op":"setText","sheet":"UseCase","cell":"A23","expectedText":"wrong","value":"x"}),
    );
    let response = machine(
        json!({"schemaVersion":1,"operation":"patch","source":source,"output":output,
        "expectedSha256":before["sourceSha256"],"operations":operations}),
        false,
    );
    let error = &response["error"];
    assert_eq!(error["operationIndex"], 12);
    assert_eq!(error["op"], "setText");
    assert_eq!(error["sheet"], "UseCase");
    assert_eq!(error["target"], "A23");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .starts_with("operation 12 (setText UseCase!A23):")
    );
    assert!(!output.exists());
}

#[test]
fn machine_formula_creation_replacement_recalculation_and_semantic_diff() {
    let dir = TempDir::new().unwrap();
    let source = dir.path().join("source.xlsx");
    let output = dir.path().join("formula.xlsx");
    workbook(&source);
    let original = fs::read(&source).unwrap();
    let report = machine(
        patch_request(
            &source,
            &output,
            json!([
                {"op":"setFormula","sheet":"UseCase","cell":"A1","expectedText":"hello","formula":"=Summary!D6"},
                {"op":"setFormula","sheet":"UseCase","cell":"C4","expectedText":"","formula":"SUM(A1:A2)"},
                {"op":"setFormula","sheet":"UseCase","cell":"A1","expectedText":"=Summary!D6","formula":"=Summary!D7"},
                {"op":"setNumber","sheet":"UseCase","cell":"D4","expectedText":"","value":42},
                {"op":"setFormula","sheet":"UseCase","cell":"D4","expectedText":"42","formula":"=IF(1<2,3,4)"}
            ]),
        ),
        true,
    );
    assert_eq!(report["operationsApplied"], 5);
    assert_eq!(report["targetsProcessed"], 5);
    assert_eq!(
        report["changedParts"],
        json!(["xl/workbook.xml", "xl/worksheets/sheet1.xml"])
    );
    let full = inspect_machine(&output, "UseCase!A1", "full");
    assert_eq!(
        full["cells"][0]["formula"]["children"],
        json!(["Summary!D7"])
    );
    assert!(full["cells"][0]["value"].is_null());
    let compact = inspect_machine(&output, "UseCase!C4:D4", "compact");
    for cell in compact["cells"].as_array().unwrap() {
        assert_eq!(cell["formula"], true);
        assert_eq!(cell["kind"], "n");
        assert_eq!(cell["styleId"], 0);
        assert!(cell.get("value").is_none());
        assert!(cell.get("text").is_none());
    }
    let wb = read_part(&output, "xl/workbook.xml");
    assert_eq!(wb.matches("<calcPr ").count(), 1);
    assert!(wb.contains(r#"<calcPr fullCalcOnLoad="1"/>"#));
    assert!(wb.find("</sheets>").unwrap() < wb.find("<calcPr ").unwrap());
    let sheet = read_part(&output, "xl/worksheets/sheet1.xml");
    assert!(sheet.contains(r#"<c r="A1" s="0"><f>Summary!D7</f></c>"#));
    assert!(sheet.contains(r#"<f>IF(1&lt;2,3,4)</f>"#));
    assert!(sheet.contains(r#"<row r="2"><c r="A2" s="0" t="s"><v>0</v></c></row>"#));
    let diff = machine(
        json!({"schemaVersion":1,"operation":"diff","before":source,"after":output}),
        true,
    );
    assert_eq!(diff["changedParts"], report["changedParts"]);
    assert_eq!(diff["cellChanges"]["total"], 3);
    assert_eq!(diff["workbookStructureChanged"], true);
    assert_eq!(
        diff["cellChanges"]["details"][0]["after"]["formula"]["children"],
        json!(["Summary!D7"])
    );
    assert_eq!(
        read_part(&source, "xl/sharedStrings.xml"),
        read_part(&output, "xl/sharedStrings.xml")
    );
    assert_eq!(fs::read(&source).unwrap(), original);
}

#[test]
fn machine_formula_validation_rolls_back_failures_and_reports_special_formula_targets() {
    let dir = TempDir::new().unwrap();
    let source = dir.path().join("source.xlsx");
    let output = dir.path().join("never.xlsx");
    let sheet = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1"><f t="shared" si="0">1</f><v>1</v></c><c r="B1"><f t="array" ref="B1:B2">2</f><v>2</v></c><c r="C1"><f t="dataTable"/><v>3</v></c><c r="D1" t="b"><v>1</v></c></row></sheetData></worksheet>"#;
    workbook_with(&source, Some(sheet), None);
    let original = fs::read(&source).unwrap();
    let mut request = patch_request(
        &source,
        &output,
        json!([
            {"op":"setFormula","sheet":"UseCase","cell":"A1","expectedText":"=1","formula":"4"},
            {"op":"setFormula","sheet":"UseCase","cell":"B1","expectedText":"=2","formula":"4"},
            {"op":"setFormula","sheet":"UseCase","cell":"C1","expectedText":"=","formula":"4"},
            {"op":"setFormula","sheet":"UseCase","cell":"D1","expectedText":"1","formula":"=TRUE"},
            {"op":"setFormula","sheet":"UseCase","cell":"E3","expectedText":"","formula":"=1"},
            {"op":"setFormula","sheet":"UseCase","cell":"E3","expectedText":"1","formula":"=2"},
            {"op":"setFormula","sheet":"UseCase","cell":"E3","expectedText":"=1","formula":"=3"},
            {"op":"setFormula","sheet":"UseCase","cell":"F3","expectedText":"","formula":"{=SUM(A1:A2)}"},
            {"op":"setFormula","sheet":"UseCase","cell":"F3","expectedText":"","formula":""}
        ]),
    );
    request["validateOnly"] = json!(true);
    request.as_object_mut().unwrap().remove("output");
    let report = machine(request.clone(), true);
    assert_eq!(report["operationsChecked"], 9);
    assert_eq!(report["targetsProcessed"], 3);
    assert_eq!(
        report["wouldChangeParts"],
        json!(["xl/workbook.xml", "xl/worksheets/sheet1.xml"])
    );
    let violations = report["violations"].as_array().unwrap();
    assert_eq!(
        violations
            .iter()
            .map(|v| v["operationIndex"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [0, 1, 2, 5, 7, 8]
    );
    for violation in violations.iter().take(3) {
        assert!(
            violation["message"]
                .as_str()
                .unwrap()
                .contains("shared/array formulas require native application")
        );
    }
    request["output"] = json!(output);
    request["validateOnly"] = json!(false);
    let rejected = machine(request, false);
    assert_eq!(rejected["error"]["operationIndex"], 0);
    assert_eq!(rejected["error"]["op"], "setFormula");
    assert_eq!(rejected["error"]["sheet"], "UseCase");
    assert_eq!(rejected["error"]["target"], "A1");
    assert!(!output.exists());
    assert_eq!(fs::read(&source).unwrap(), original);
}

/// Seven synthetic packages mirror the enterprise feature mix without using
/// enterprise inputs: unrelated shared formulas, print area on another sheet,
/// one row-zero drawing, one VML-only workbook, and one calculation chain.
#[test]
fn machine_insert_rows_seven_enterprise_feature_fixtures() {
    use std::collections::BTreeMap;
    use std::io::Read as _;
    const MAIN: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
    const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    let dir = TempDir::new().unwrap();
    for (index, shared_count) in [24, 48, 72, 96, 120, 144, 188].into_iter().enumerate() {
        let source = dir.path().join(format!("enterprise-{index}.xlsx"));
        let candidate = dir.path().join(format!("candidate-{index}.xlsx"));
        workbook(&source);
        let mut parts = BTreeMap::new();
        {
            let mut archive = zip::ZipArchive::new(fs::File::open(&source).unwrap()).unwrap();
            for i in 0..archive.len() {
                let mut entry = archive.by_index(i).unwrap();
                let mut data = Vec::new();
                entry.read_to_end(&mut data).unwrap();
                parts.insert(entry.name().to_string(), data);
            }
        }
        let styles = String::from_utf8(parts["xl/styles.xml"].clone()).unwrap()
            .replace("<cellXfs count=\"1\">", "<cellXfs count=\"2\">")
            .replace("</cellXfs>", "<xf fontId=\"0\" fillId=\"0\" borderId=\"0\" numFmtId=\"0\" xfId=\"0\"><alignment horizontal=\"center\"/></xf></cellXfs>");
        parts.insert("xl/styles.xml".into(), styles.into_bytes());
        let workbook_xml = format!(
            r#"<workbook xmlns="{MAIN}" xmlns:r="{REL}"><sheets><sheet name="UseCase" sheetId="4" r:id="rId1"/><sheet name="Report Image" sheetId="8" r:id="report"/></sheets><definedNames><definedName name="_xlnm.Print_Area" localSheetId="1">'Report Image'!$A$1:$AY$142</definedName></definedNames></workbook>"#
        );
        parts.insert("xl/workbook.xml".into(), workbook_xml.as_bytes().into());
        let rels = String::from_utf8(parts["xl/_rels/workbook.xml.rels"].clone()).unwrap()
            .replace("</Relationships>", &format!(r#"<Relationship Id="report" Type="{REL}/worksheet" Target="worksheets/sheet2.xml"/></Relationships>"#));
        parts.insert("xl/_rels/workbook.xml.rels".into(), rels.into_bytes());
        let shared = (1..=shared_count)
            .map(|row| {
                let formula = if row == 1 {
                    format!(r#"<f t="shared" si="0" ref="B1:B{shared_count}">A1+1</f>"#)
                } else {
                    r#"<f t="shared" si="0"/>"#.into()
                };
                let references = if row == 1 {
                    r#"<c r="A1"><f>UseCase!C7+UseCase!$C$7+UseCase!U7</f></c>"#
                } else {
                    ""
                };
                format!(r#"<row r="{row}">{references}<c r="B{row}">{formula}<v>1</v></c></row>"#)
            })
            .collect::<String>();
        let other =
            format!(r#"<worksheet xmlns="{MAIN}"><sheetData>{shared}</sheetData></worksheet>"#);
        parts.insert("xl/worksheets/sheet2.xml".into(), other.as_bytes().into());
        let object = match index {
            0 => r#"<drawing r:id="drawing"/>"#,
            1 => r#"<legacyDrawing r:id="drawing"/>"#,
            _ => "",
        };
        let raw = "<row r='7'> <c r='C7'><v>7</v></c><c r='U7'><v>8</v></c> </row>";
        parts.insert("xl/worksheets/sheet1.xml".into(), format!(r#"<worksheet xmlns="{MAIN}" xmlns:r="{REL}"><dimension ref="A7:U15"/><sheetData>{raw}<row r="9" ht="24" customHeight="1"><c r="A9" s="1"/></row><row r="15" spans="3:21"><c r="C15"/><c r="U15"/></row></sheetData>{object}</worksheet>"#).into_bytes());
        if index <= 1 {
            let (kind, target, data) = if index == 0 {
                (
                    "drawing",
                    "drawing1.xml",
                    r#"<xdr:wsDr xmlns:xdr="http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing"><xdr:twoCellAnchor editAs="oneCell"><xdr:from><xdr:col>0</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>0</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from><xdr:to><xdr:col>4</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>5</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:to><xdr:clientData/></xdr:twoCellAnchor></xdr:wsDr>"#,
                )
            } else {
                (
                    "vmlDrawing",
                    "vmlDrawing1.vml",
                    r#"<xml xmlns:v="urn:schemas-microsoft-com:vml" xmlns:x="urn:schemas-microsoft-com:office:excel"><v:shape id="shape1"><x:ClientData ObjectType="Button"><x:Anchor>0, 0, 11, 0, 4, 0, 14, 0</x:Anchor><x:Row>11</x:Row></x:ClientData></v:shape></xml>"#,
                )
            };
            parts.insert("xl/worksheets/_rels/sheet1.xml.rels".into(), format!(r#"<Relationships><Relationship Id="drawing" Type="{REL}/{kind}" Target="../drawings/{target}"/></Relationships>"#).into_bytes());
            parts.insert(format!("xl/drawings/{target}"), data.as_bytes().into());
        }
        if index == 2 {
            parts.insert("xl/calcChain.xml".into(), format!(r#"<calcChain xmlns="{MAIN}"><c r="C15" i="1"/><c r="U15"/><c r="A15" i="2"/></calcChain>"#).into_bytes());
        }
        let mut writer = ZipWriter::new(fs::File::create(&source).unwrap());
        for (name, data) in &parts {
            writer
                .start_file(name, SimpleFileOptions::default())
                .unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap();
        let source_bytes = fs::read(&source).unwrap();
        let request = patch_request(
            &source,
            &candidate,
            json!([
                {"op":"insertRows","sheet":"UseCase","before":10,"count":2},
                {"op":"setText","sheet":"UseCase","cell":"C10","expectedText":"","value":"inserted"},
                {"op":"setStyle","sheet":"UseCase","range":"C10","style":{"bold":true}},
                {"op":"copyStyle","sheet":"UseCase","range":"U11","fromCell":"A10","components":["font","fill","border","alignment","numberFormat"]}
            ]),
        );
        let mut validate = request.clone();
        validate["validateOnly"] = json!(true);
        let checked = machine(validate, true);
        assert_eq!(checked["violations"], json!([]));
        assert!(!candidate.exists());
        let result = machine(request, true);
        assert_eq!(
            result["rowsInserted"],
            json!([{"sheet":"UseCase","before":10,"count":2}])
        );
        assert_eq!(result["targetsProcessed"], 5);
        assert_eq!(result["changedParts"], checked["wouldChangeParts"]);
        assert_eq!(result["rowsInserted"], checked["rowsInserted"]);
        assert_eq!(fs::read(&source).unwrap(), source_bytes);
        assert_eq!(read_part(&candidate, "xl/worksheets/sheet2.xml"), other);
        assert_eq!(read_part(&candidate, "xl/workbook.xml"), workbook_xml);
        let target = read_part(&candidate, "xl/worksheets/sheet1.xml");
        assert!(target.contains(raw));
        assert!(target.contains("r=\"C17\""));
        assert!(target.contains("ref=\"A7:U17\""));
        assert_eq!(
            inspect_machine(&candidate, "UseCase!C10", "compact")["cells"][0]["text"],
            "inserted"
        );
        assert_eq!(
            inspect_machine(&candidate, "UseCase!U11", "compact")["cells"][0]["styleId"],
            1
        );
        if index == 0 {
            assert_eq!(
                read_part(&candidate, "xl/drawings/drawing1.xml").as_bytes(),
                parts["xl/drawings/drawing1.xml"]
            );
        }
        if index == 1 {
            let vml = read_part(&candidate, "xl/drawings/vmlDrawing1.vml");
            assert!(vml.contains("0, 0, 13, 0, 4, 0, 16, 0"));
            assert!(vml.contains("<x:Row>13</x:Row>"));
        }
        if index == 2 {
            let calc = read_part(&candidate, "xl/calcChain.xml");
            assert!(calc.contains("r=\"C17\""));
            assert!(calc.contains("r=\"U17\""));
            assert!(calc.contains("r=\"A15\""));
        }
    }
}
