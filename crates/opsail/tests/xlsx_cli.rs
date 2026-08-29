use std::io::Write as _;
use std::path::Path;

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
            br#"<xdr:wsDr xmlns:xdr="http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><xdr:twoCellAnchor><xdr:from><xdr:col>0</xdr:col><xdr:row>0</xdr:row></xdr:from><xdr:to><xdr:col>3</xdr:col><xdr:row>9</xdr:row></xdr:to><xdr:pic><xdr:blipFill><a:blip r:embed="rIdImage"/></xdr:blipFill></xdr:pic><xdr:clientData/></xdr:twoCellAnchor></xdr:wsDr>"#,
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
