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
        .stderr(predicate::str::contains("targeted worksheet scan stopped"));
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
