use std::fs;
use std::io::Write as _;
use std::path::Path;

use assert_cmd::Command;
use serde_json::{Value, json};
use tempfile::TempDir;
use zip::{ZipWriter, write::SimpleFileOptions};

fn workbook(path: &Path) {
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
    serde_json::from_slice(&assertion.get_output().stdout).unwrap()
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
