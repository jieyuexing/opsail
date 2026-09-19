use std::io::Write as _;
use std::path::Path;
use std::process::Command as ProcessCommand;

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::json;
use tempfile::tempdir;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

fn write_xlsx(path: &Path) {
    write_xlsx_with_sheet(
        path,
        r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1:XFD99"/><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>CLI value</t></is></c></row><row r="99"><c r="XFD99" s="1"/></row></sheetData></worksheet>"#,
    );
}

fn write_xlsx_with_sheet(path: &Path, sheet: &str) {
    let parts = [
        (
            "[Content_Types].xml",
            r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>"#,
        ),
        (
            "xl/workbook.xml",
            r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Data" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
        ),
        (
            "xl/_rels/workbook.xml.rels",
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
        ),
        ("xl/worksheets/sheet1.xml", sheet),
    ];
    let file = std::fs::File::create(path).unwrap();
    let mut archive = ZipWriter::new(file);
    for (name, xml) in parts {
        archive
            .start_file(name, SimpleFileOptions::default())
            .unwrap();
        archive.write_all(xml.as_bytes()).unwrap();
    }
    archive.finish().unwrap();
}

fn write_xlsx_with_missing_drawing(path: &Path) {
    let parts = [
        ("[Content_Types].xml", r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>"#),
        ("xl/workbook.xml", r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Data" sheetId="1" r:id="rId1"/></sheets></workbook>"#),
        ("xl/_rels/workbook.xml.rels", r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#),
        ("xl/worksheets/sheet1.xml", r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheetData/><drawing r:id="missing"/></worksheet>"#),
        ("xl/worksheets/_rels/sheet1.xml.rels", r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="missing" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing" Target="../drawings/not-there.xml"/></Relationships>"#),
    ];
    let file = std::fs::File::create(path).unwrap(); let mut archive = ZipWriter::new(file);
    for (name, xml) in parts { archive.start_file(name, SimpleFileOptions::default()).unwrap(); archive.write_all(xml.as_bytes()).unwrap(); }
    archive.finish().unwrap();
}

fn write_xlsx_with_shared_string(path: &Path, shared: &str) {
    let parts = [
        (
            "[Content_Types].xml",
            r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>"#,
        ),
        (
            "xl/workbook.xml",
            r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Data" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
        ),
        (
            "xl/_rels/workbook.xml.rels",
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
        ),
        ("xl/sharedStrings.xml", shared),
        (
            "xl/worksheets/sheet1.xml",
            r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c></row></sheetData></worksheet>"#,
        ),
    ];
    let file = std::fs::File::create(path).unwrap();
    let mut archive = ZipWriter::new(file);
    for (name, xml) in parts {
        archive
            .start_file(name, SimpleFileOptions::default())
            .unwrap();
        archive.write_all(xml.as_bytes()).unwrap();
    }
    archive.finish().unwrap();
}

fn write_xlsx_with_workbook_xml(path: &Path, workbook: &str, sheet: &str) {
    let parts = [
        (
            "[Content_Types].xml",
            r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>"#,
        ),
        ("xl/workbook.xml", workbook),
        (
            "xl/_rels/workbook.xml.rels",
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
        ),
        ("xl/worksheets/sheet1.xml", sheet),
    ];
    let file = std::fs::File::create(path).unwrap();
    let mut archive = ZipWriter::new(file);
    for (name, xml) in parts {
        archive
            .start_file(name, SimpleFileOptions::default())
            .unwrap();
        archive.write_all(xml.as_bytes()).unwrap();
    }
    archive.finish().unwrap();
}

fn write_full_export_fixture(path: &Path) {
    let parts = [
        (
            "[Content_Types].xml",
            r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>"#,
        ),
        (
            "xl/workbook.xml",
            r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Visible" sheetId="1" r:id="rId1"/><sheet name="Hidden" sheetId="2" state="hidden" r:id="rId2"/></sheets></workbook>"#,
        ),
        (
            "xl/_rels/workbook.xml.rels",
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml"/></Relationships>"#,
        ),
        (
            "xl/worksheets/sheet1.xml",
            r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1:XFD1048576"/><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>head</t></is></c></row><row r="1048576"><c r="XFD1048576" t="inlineStr"><is><t>far tail</t></is></c></row></sheetData></worksheet>"#,
        ),
        (
            "xl/worksheets/sheet2.xml",
            r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="7"><c r="C7" t="inlineStr"><is><t>hidden value</t></is></c></row></sheetData></worksheet>"#,
        ),
    ];
    let file = std::fs::File::create(path).unwrap();
    let mut archive = ZipWriter::new(file);
    for (name, xml) in parts {
        archive
            .start_file(name, SimpleFileOptions::default())
            .unwrap();
        archive.write_all(xml.as_bytes()).unwrap();
    }
    archive.finish().unwrap();
}

fn generate_v2_text_fixture(directory: &Path) -> (std::path::PathBuf, Vec<String>) {
    let fixture = directory.join("source-defined-v2.xlsx");
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/support/view_fixture.py");
    let output = ProcessCommand::new("python3")
        .args([script.to_str().unwrap(), "--output", fixture.to_str().unwrap()])
        .output()
        .expect("fixture generator must run");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let probes: serde_json::Value = serde_json::from_slice(
        &std::fs::read(fixture.with_extension("probes.json")).unwrap(),
    ).unwrap();
    let tokens = probes["probes"].as_array().unwrap().iter()
        .map(|probe| probe["text"].as_str().unwrap().to_owned()).collect();
    (fixture, tokens)
}

#[test]
fn view_extract_v2_source_fixture_preserves_non_cell_text_anchors_and_terminal_counts() {
    let directory = tempdir().unwrap();
    let (path, tokens) = generate_v2_text_fixture(directory.path());
    assert!(tokens.len() >= 24, "fixture must define at least 24 non-cell probes");
    let assert = Command::cargo_bin("opsail").unwrap()
        .args(["view", "extract", path.to_str().unwrap(), "--format", "jsonl"])
        .assert().success();
    let lines = assert.get_output().stdout.split(|byte| *byte == b'\n').filter(|line| !line.is_empty())
        .map(serde_json::from_slice::<serde_json::Value>).collect::<Result<Vec<_>, _>>().unwrap();
    let non_cell_text = lines.iter().filter(|line| line["type"] == "shape" || line["type"] == "comment")
        .filter_map(|line| line["text"].as_str()).collect::<Vec<_>>().join("\n");
    for token in &tokens { assert!(non_cell_text.contains(token), "missing source-defined probe {token}"); }
    assert!(lines.iter().any(|line| line["type"] == "comment" && line["kind"] == "comment"));
    assert!(lines.iter().any(|line| line["type"] == "comment" && line["kind"] == "threaded"));
    assert!(lines.iter().any(|line| line["type"] == "shape" && line["kind"] == "vml-textbox"));
    let absolute = lines.iter().find(|line| line["type"] == "shape" && line["sourceId"] == "xl/drawings/drawing1.xml#shape:88").unwrap();
    assert_eq!(absolute["anchor"]["absoluteEmu"], json!({"x":914400,"y":1828800,"cx":2743200,"cy":3657600}));
    assert!(absolute["anchor"].get("from").is_none());
    assert_eq!(absolute["text"], "ABSOLUTE_ONE\nABSOLUTE_TWO\tABSOLUTE_THREE ABSOLUTE_四");
    for source_id in ["xl/drawings/drawing1.xml#shape:91", "xl/drawings/drawing1.xml#shape:92"] {
        let grouped = lines.iter().find(|line| line["sourceId"] == source_id).unwrap();
        assert_eq!(grouped["anchor"]["from"]["columnZeroBased"], 6);
        assert_eq!(grouped["anchor"]["from"]["rowZeroBased"], 7);
    }
    let terminal = lines.last().unwrap();
    assert_eq!(terminal["type"], "summary"); assert_eq!(terminal["complete"], true);
    let comments = lines.iter().filter(|line| line["type"] == "comment").count() as u64;
    let shapes = lines.iter().filter(|line| line["type"] == "shape").count() as u64;
    let assets = lines.iter().filter(|line| line["type"] == "asset").count() as u64;
    assert_eq!(terminal["stats"]["comments"], comments);
    assert_eq!(terminal["stats"]["shapes"], shapes);
    assert_eq!(terminal["stats"]["assets"], assets);
}

#[test]
fn view_extract_v2_text_budget_and_missing_referenced_part_fail_without_summary() {
    let directory = tempdir().unwrap(); let (path, _) = generate_v2_text_fixture(directory.path());
    let limited = Command::cargo_bin("opsail").unwrap().args(["view", "extract", path.to_str().unwrap(), "--max-text-bytes", "1"])
        .assert().failure().stderr(predicate::str::contains("max-text-bytes"));
    assert!(!String::from_utf8_lossy(&limited.get_output().stdout).contains("\"summary\""));
    let missing = directory.path().join("missing-drawing.xlsx");
    write_xlsx_with_missing_drawing(&missing);
    let failed = Command::cargo_bin("opsail").unwrap().args(["view", "extract", missing.to_str().unwrap()])
        .assert().failure();
    assert!(!String::from_utf8_lossy(&failed.get_output().stdout).contains("\"summary\""));
}

#[test]
fn view_extract_streams_hidden_and_far_cells_without_preview_caps() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("complete.xlsx");
    write_full_export_fixture(&path);
    let assert = Command::cargo_bin("opsail")
        .unwrap()
        .args([
            "view",
            "extract",
            path.to_str().unwrap(),
            "--format",
            "jsonl",
        ])
        .assert()
        .success();
    let lines = assert
        .get_output()
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(serde_json::from_slice::<serde_json::Value>)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(lines.first().unwrap()["type"], "protocol");
    assert_eq!(lines.first().unwrap()["version"], 2);
    assert!(lines.iter().any(|value| value["type"] == "sheet"
        && value["name"] == "Hidden"
        && value["hidden"] == true));
    assert!(lines.iter().any(|value| value["type"] == "cell"
        && value["cell"] == "XFD1048576"
        && value["text"] == "far tail"));
    assert!(lines.iter().any(|value| value["type"] == "cell"
        && value["sheet"] == "Hidden"
        && value["cell"] == "C7"));
    assert_eq!(lines.last().unwrap()["type"], "summary");
    assert_eq!(lines.last().unwrap()["complete"], true);
    assert_eq!(lines.last().unwrap()["version"], 2);
    assert_eq!(lines.last().unwrap()["stats"]["cells"], 3);
    assert_eq!(lines.last().unwrap()["cellCount"], 3);
}

#[test]
fn view_extract_limit_fails_without_completion_summary() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("limited.xlsx");
    write_full_export_fixture(&path);
    let assert = Command::cargo_bin("opsail")
        .unwrap()
        .args([
            "view",
            "extract",
            path.to_str().unwrap(),
            "--max-cells",
            "1",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "full export exceeds max-cells limit",
        ));
    assert!(!String::from_utf8_lossy(&assert.get_output().stdout).contains("\"summary\""));
}

#[test]
fn view_extract_covers_the_tail_that_read_preview_intentionally_omits() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("preview-known-bad.xlsx");
    write_full_export_fixture(&path);
    let preview = Command::cargo_bin("opsail")
        .unwrap()
        .args(["read", path.to_str().unwrap(), "--format", "json"])
        .output()
        .unwrap();
    assert!(preview.status.success());
    assert!(!String::from_utf8_lossy(&preview.stdout).contains("far tail"));
    let complete = Command::cargo_bin("opsail")
        .unwrap()
        .args(["view", "extract", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(complete.status.success());
    assert!(String::from_utf8_lossy(&complete.stdout).contains("far tail"));
}

#[test]
fn view_extract_rejects_corrupt_workbook_without_completion_summary() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("broken.xlsx");
    std::fs::write(&path, b"not a zip").unwrap();
    let assert = Command::cargo_bin("opsail")
        .unwrap()
        .args(["view", "extract", path.to_str().unwrap()])
        .assert()
        .failure();
    assert!(assert.get_output().stdout.is_empty());
}

#[test]
fn view_extract_rejects_truncated_worksheet_without_completion_summary() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("truncated-sheet.xlsx");
    write_xlsx_with_sheet(
        &path,
        r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>published before truncation</t></is></c></row>"#,
    );
    let assert = Command::cargo_bin("opsail")
        .unwrap()
        .args(["view", "extract", path.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("truncated or unclosed"));
    assert!(!String::from_utf8_lossy(&assert.get_output().stdout).contains("\"summary\""));
}

#[test]
fn view_extract_rejects_truncated_workbook_and_cdata_without_summary() {
    let directory = tempdir().unwrap();
    let workbook = directory.path().join("truncated-workbook.xlsx");
    write_xlsx_with_workbook_xml(
        &workbook,
        r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Data" sheetId="1" r:id="rId1"/>"#,
        r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData/></worksheet>"#,
    );
    let cdata = directory.path().join("cdata.xlsx");
    write_xlsx_with_sheet(
        &cdata,
        r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t><![CDATA[never silently omitted]]></t></is></c></row></sheetData></worksheet>"#,
    );
    for path in [&workbook, &cdata] {
        let assert = Command::cargo_bin("opsail")
            .unwrap()
            .args(["view", "extract", path.to_str().unwrap()])
            .assert()
            .failure();
        assert!(!String::from_utf8_lossy(&assert.get_output().stdout).contains("\"summary\""));
    }
}

#[test]
fn view_extract_rejects_overlong_inline_and_shared_text_without_summary() {
    let directory = tempdir().unwrap();
    let too_long = "x".repeat(32_767 * 4 + 1);
    let inline = directory.path().join("overlong-inline.xlsx");
    write_xlsx_with_sheet(
        &inline,
        &format!(
            r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>{too_long}</t></is></c></row></sheetData></worksheet>"#
        ),
    );
    let shared = directory.path().join("overlong-shared.xlsx");
    write_xlsx_with_shared_string(
        &shared,
        &format!(
            r#"<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="1" uniqueCount="1"><si><t>{too_long}</t></si></sst>"#
        ),
    );
    for path in [&inline, &shared] {
        let assert = Command::cargo_bin("opsail")
            .unwrap()
            .args(["view", "extract", path.to_str().unwrap()])
            .assert()
            .failure()
            .stderr(predicate::str::contains("text exceeds"));
        assert!(!String::from_utf8_lossy(&assert.get_output().stdout).contains("\"summary\""));
    }
}

fn write_picture_xlsx(path: &Path) {
    const PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x04, 0x00, 0x00, 0x00, 0xb5,
        0x1c, 0x0c, 0x02, 0x00, 0x00, 0x00, 0x0b, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x64,
        0xf8, 0x0f, 0x00, 0x01, 0x05, 0x01, 0x01, 0x27, 0x18, 0xe3, 0x66, 0x00, 0x00, 0x00, 0x00,
        0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];
    let parts: [(&str, &[u8]); 7] = [
        (
            "[Content_Types].xml",
            br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="png" ContentType="image/png"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>"#,
        ),
        (
            "xl/workbook.xml",
            br#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Data" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
        ),
        (
            "xl/_rels/workbook.xml.rels",
            br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
        ),
        (
            "xl/worksheets/sheet1.xml",
            br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><dimension ref="A1:D10"/><sheetData/><drawing r:id="rIdDrawing"/></worksheet>"#,
        ),
        (
            "xl/worksheets/_rels/sheet1.xml.rels",
            br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdDrawing" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing" Target="../drawings/drawing1.xml"/></Relationships>"#,
        ),
        (
            "xl/drawings/drawing1.xml",
            br#"<xdr:wsDr xmlns:xdr="http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><xdr:twoCellAnchor><xdr:from><xdr:col>0</xdr:col><xdr:row>0</xdr:row></xdr:from><xdr:to><xdr:col>3</xdr:col><xdr:row>9</xdr:row></xdr:to><xdr:pic><xdr:blipFill><a:blip r:embed="rIdImage"/></xdr:blipFill></xdr:pic><xdr:sp><xdr:nvSpPr><xdr:cNvPr id="7" name="Box"/></xdr:nvSpPr><xdr:txBody><a:p><a:r><a:t>BOX A</a:t></a:r></a:p><a:p><a:r><a:t>BOX B</a:t></a:r></a:p></xdr:txBody></xdr:sp><xdr:clientData/></xdr:twoCellAnchor></xdr:wsDr>"#,
        ),
        (
            "xl/drawings/_rels/drawing1.xml.rels",
            br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdImage" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.png"/></Relationships>"#,
        ),
    ];
    let file = std::fs::File::create(path).unwrap();
    let mut archive = ZipWriter::new(file);
    for (name, bytes) in parts {
        archive
            .start_file(name, SimpleFileOptions::default())
            .unwrap();
        archive.write_all(bytes).unwrap();
    }
    archive
        .start_file("xl/media/image1.png", SimpleFileOptions::default())
        .unwrap();
    archive.write_all(PNG).unwrap();
    archive.finish().unwrap();
}

#[test]
fn read_cli_returns_bounded_picture_pixels_for_an_intersecting_range() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("picture.xlsx");
    write_picture_xlsx(&path);
    let assert = Command::cargo_bin("opsail")
        .unwrap()
        .args([
            "read",
            path.to_str().unwrap(),
            "--range",
            "Data!A1:D10",
            "--format",
            "json",
        ])
        .assert()
        .success();
    let output = assert.get_output();
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();

    assert!(output.stdout.len() < 16 * 1024 * 1024);
    let inventory = &result["workbook"]["sheets"][0]["pictures"][0];
    assert_eq!(inventory["fromCell"], "A1");
    assert_eq!(inventory["toCell"], "D10");
    assert!(inventory.get("dataUri").is_none());
    let selected = &result["workbook"]["selections"][0]["images"][0];
    assert_eq!(selected["contentType"], "image/png");
    assert!(
        selected["dataUri"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,iVBORw0KGgo")
    );
    assert_eq!(selected["payloadTruncated"], false);
}

#[test]
fn view_extract_exports_only_internal_drawing_assets_to_controlled_directory() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("picture.xlsx");
    let assets = std::fs::canonicalize(directory.path()).unwrap().join("assets");
    write_picture_xlsx(&path);
    let assert = Command::cargo_bin("opsail")
        .unwrap()
        .args(["view", "extract", path.to_str().unwrap(), "--assets-dir", assets.to_str().unwrap()])
        .assert()
        .success();
    let lines = assert.get_output().stdout.split(|byte| *byte == b'\n').filter(|line| !line.is_empty())
        .map(serde_json::from_slice::<serde_json::Value>).collect::<Result<Vec<_>, _>>().unwrap();
    let asset = lines.iter().find(|value| value["type"] == "asset").expect("internal image asset");
    assert_eq!(asset["packagePart"], "xl/media/image1.png");
    assert_eq!(asset["mimeType"], "image/png");
    assert_eq!(asset["anchor"]["from"]["rowZeroBased"], 0);
    let exported = std::path::PathBuf::from(asset["exportedPath"].as_str().unwrap());
    assert!(exported.starts_with(std::fs::canonicalize(&assets).unwrap()));
    assert!(exported.is_file());
    let shape = lines.iter().find(|value| value["type"] == "shape").expect("DrawingML textbox");
    assert_eq!(shape["text"], "BOX A\nBOX B");
    assert_eq!(shape["sourceId"], "xl/drawings/drawing1.xml#shape:7");
    assert_eq!(lines.last().unwrap()["stats"]["assets"], 1);
}

#[test]
fn read_cli_truncates_cells_to_the_serialized_output_budget() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("bounded-output.xlsx");
    let value = "x".repeat(1_024);
    let mut sheet = String::from(
        r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1:A20"/><sheetData>"#,
    );
    for row in 1..=20 {
        use std::fmt::Write as _;
        write!(
            sheet,
            r#"<row r="{row}"><c r="A{row}" t="inlineStr"><is><t>{value}</t></is></c></row>"#
        )
        .unwrap();
    }
    sheet.push_str("</sheetData></worksheet>");
    write_xlsx_with_sheet(&path, &sheet);
    let assert = Command::cargo_bin("opsail")
        .unwrap()
        .args([
            "read",
            path.to_str().unwrap(),
            "--range",
            "Data!A1:A20",
            "--max-output-bytes",
            "16384",
            "--format",
            "json",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("serialized-output limit"));
    let output = assert.get_output();
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();

    assert!(output.stdout.len() <= 16_384);
    assert!(
        result["workbook"]["statistics"]["returnedCells"]
            .as_u64()
            .unwrap()
            < 20
    );
    assert_eq!(result["workbook"]["selections"][0]["truncated"], true);
}

#[test]
fn read_cli_emits_a_sparse_workbook_artifact() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("cli.xlsx");
    write_xlsx(&path);
    let assert = Command::cargo_bin("opsail")
        .unwrap()
        .args([
            "read",
            path.to_str().unwrap(),
            "--range",
            "Data!A1:B2",
            "--format",
            "json",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("skipped cell bodies"));
    let result: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();

    assert_eq!(result["artifactKind"], "workbook");
    assert_eq!(result["extraction"]["method"], "ooxml-sparse");
    assert_eq!(result["workbook"]["statistics"]["cellElements"], 1);
    assert_eq!(result["workbook"]["statistics"]["returnedCells"], 1);
    assert_eq!(
        result["workbook"]["sheets"][0]["semanticBoundsComplete"],
        false
    );
    assert_eq!(
        result["workbook"]["sheets"][0]["features"]["cellDataComplete"],
        false
    );
    assert_eq!(
        result["workbook"]["sheets"][0]["features"]["tailFeaturesComplete"],
        true
    );
    assert_eq!(result["workbook"]["selections"][0]["usedBounds"], "A1:A1");
    assert_eq!(
        result["workbook"]["selections"][0]["mergedRanges"],
        json!([])
    );
    assert_eq!(
        result["workbook"]["selections"][0]["cells"][0]["display"],
        "CLI value"
    );
}

#[test]
fn read_cli_revision_only_avoids_worksheet_expansion() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("revision.xlsx");
    write_xlsx(&path);
    let assert = Command::cargo_bin("opsail")
        .unwrap()
        .args([
            "read",
            path.to_str().unwrap(),
            "--revision-only",
            "--format",
            "json",
        ])
        .assert()
        .success()
        .stderr("");
    let result: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();

    assert_eq!(result["revision"]["parts"].as_array().unwrap().len(), 4);
    assert_eq!(result["workbook"]["statistics"]["scannedSheets"], 0);
    assert_eq!(
        result["workbook"]["selections"].as_array().unwrap().len(),
        0
    );
    assert_eq!(
        result["workbook"]["sheets"][0]["features"]["scanned"],
        false
    );
}

#[test]
fn machine_protocol_accepts_batched_xlsx_ranges() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("machine.xlsx");
    write_xlsx(&path);
    let request = json!({
        "protocolVersion": 1,
        "source": {"kind": "file", "path": path},
        "options": {
            "ranges": ["Data!A1:A1", "Data!A1:B2"],
            "maxCells": 10,
            "maxExpandedBytes": 1048576,
            "includeFormulas": false
        }
    });
    let assert = Command::cargo_bin("opsail")
        .unwrap()
        .args(["read", "--machine"])
        .write_stdin(request.to_string())
        .assert()
        .success()
        .stderr("");
    let response: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();

    assert_eq!(response["protocolVersion"], 1);
    assert_eq!(response["ok"], true);
    assert_eq!(response["result"]["artifactKind"], "workbook");
    assert_eq!(
        response["result"]["workbook"]["selections"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        response["result"]["workbook"]["statistics"]["scannedSheets"],
        1
    );
}
