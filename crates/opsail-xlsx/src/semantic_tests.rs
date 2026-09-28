//! Public JSON contract tests; this file also compiles against 1b9174a.
use crate::execute;
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use tempfile::TempDir;
use zip::{ZipWriter, write::SimpleFileOptions};
const MAIN: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

fn styles(user: bool) -> String {
    let fonts = if user {
        r#"<font><name val="Unused"/></font><font><sz val="11"/><color rgb="FF000000"/><name val="Arial"/></font><font><sz val="11"/><color rgb="FF0000FF"/><name val="Arial"/></font><font><strike/><sz val="11"/><color rgb="FF000000"/><name val="Arial"/></font><font><sz val="11"/><color rgb="FF052CFF"/><name val="Arial"/></font>"#
    } else {
        r#"<font><name val="Arial"/><sz val="11"/><color rgb="FF000000"/></font><font><name val="Arial"/><sz val="11"/><color rgb="FF0000FF"/></font><font><name val="Arial"/><sz val="11"/><color rgb="FF000000"/><strike val="1"/></font>"#
    };
    // WPS's new index zero resolves to the inherited F-column strike font.
    // It is unused until F8 acquires an explicit s="0" during the save.
    let xfs = if user {
        r#"<xf fontId="3"/><xf fontId="1"/><xf fontId="2"/><xf fontId="3"/><xf fontId="4"/>"#
    } else {
        r#"<xf/><xf fontId="1"/><xf fontId="2"/>"#
    };
    let border = if user {
        "<border/>"
    } else {
        "<border><left/><right/><top/><bottom/><diagonal/></border>"
    };
    format!(
        r#"<styleSheet xmlns="{MAIN}"><fonts>{fonts}</fonts><fills><fill><patternFill patternType="none"/></fill></fills><borders>{border}</borders><cellXfs>{xfs}</cellXfs></styleSheet>"#
    )
}
fn fixture(path: &Path, user: bool, extra: &str) {
    let mut shared = Vec::new();
    let mut string = |cell: &str, text: &str, style: Option<u32>| {
        let s = style.map(|n| format!(r#" s="{n}""#)).unwrap_or_default();
        if user {
            let i = shared.len();
            shared.push(format!("<si><t>{text}</t></si>"));
            format!(r#"<c r="{cell}"{s} t="s"><v>{i}</v></c>"#)
        } else {
            format!(r#"<c r="{cell}"{s} t="inlineStr"><is><t>{text}</t></is></c>"#)
        }
    };
    let mut history = String::new();
    history.push_str("<row r=\"8\">");
    for (c, t) in [("A8", "Version"), ("B8", "Ticket"), ("C8", "Date")] {
        history.push_str(&string(c, t, Some(if user { 1 } else { 0 })));
    }
    history.push_str("</row>");
    for (row, number, date) in [
        (9, "1", "2026.09.08"),
        (10, "1.1", "2026.09.22"),
        (11, "1.2", if user { "2026.09.28" } else { "2026.09.24" }),
    ] {
        let style = if user {
            4
        } else if row == 11 {
            1
        } else {
            0
        };
        history.push_str(&format!("<row r=\"{row}\">"));
        if row == 11 && !user {
            history.push_str(&string("A11", number, Some(style)));
        } else {
            history.push_str(&format!(r#"<c r="A{row}" s="{style}"><v>{number}</v></c>"#));
        }
        history.push_str(&string(&format!("B{row}"), "#100", Some(style)));
        history.push_str(&string(&format!("C{row}"), date, Some(style)));
        history.push_str("</row>");
    }
    let mut usecase = String::new();
    for r in 4..=if user { 9 } else { 8 } {
        let text = if user && r == 6 {
            "Inserted step".into()
        } else if user && r == 8 {
            "Step 4 revised".into()
        } else {
            format!("Step {}", if user && r > 6 { r - 4 } else { r - 3 })
        };
        usecase.push_str(&format!(
            "<row r=\"{r}\">{}",
            string(&format!("B{r}"), &text, Some(if user { 1 } else { 0 }))
        ));
        if r == 5 {
            usecase.push_str(if user {
                "RICH_J5"
            } else {
                r#"<c r="J5" s="2" t="inlineStr"><is><t>Old rule</t></is></c>"#
            });
        }
        usecase.push_str("</row>");
    }
    let f8 = string("F8", "ERR1", if user { Some(0) } else { None });
    let h = if user { 34 } else { 17 };
    let f7 = if user {
        "RICH_F7"
    } else {
        r#"<c r="F7" s="2" t="inlineStr"><is><t>Error Condition</t></is></c>"#
    };
    let error = format!(r#"<row r="7" ht="{h}">{f7}</row><row r="8" ht="{h}">{f8}</row>"#);
    let mut error = error;
    if user {
        for (marker, cell, old, new) in [
            ("RICH_J5", "J5", "Old rule", "New rule"),
            ("RICH_F7", "F7", "Error Condition", "MSG text"),
        ] {
            let i = shared.len();
            shared.push(format!(r#"<si><r><rPr><strike/><sz val="11"/><color rgb="FF000000"/><rFont val="Arial"/></rPr><t>{old}</t></r><r><rPr><sz val="11"/><color rgb="FF0000FF"/><rFont val="Arial"/></rPr><t xml:space="preserve">&#10;{new}</t></r></si>"#));
            let c = format!(r#"<c r="{cell}" s="3" t="s"><v>{i}</v></c>"#);
            usecase = usecase.replace(marker, &c);
            error = error.replace(marker, &c);
        }
    }
    let selection = if user { "C11" } else { "A1" };
    let cols = if user {
        r#"<cols><col width="12" max="1" min="1" style="1"/><col width="22" max="3" min="3" style="1"/></cols>"#
    } else {
        r#"<cols><col min="1" max="1" width="12"/><col min="3" max="3" width="22"/></cols>"#
    };
    let history = format!(
        r#"<sheetViews><sheetView workbookViewId="0"><selection activeCell="{selection}"/></sheetView></sheetViews><sheetFormatPr defaultRowHeight="15"/>{cols}<sheetData>{history}</sheetData>{}"#,
        if user {
            r#"<mergeCells><mergeCell ref="E12:F12"/></mergeCells>"#
        } else {
            ""
        }
    );
    let usecase = format!(
        r#"{}<sheetData>{usecase}{extra}</sheetData><mergeCells><mergeCell ref="B{r}:C{r}"/></mergeCells>"#,
        if user {
            r#"<sheetViews><sheetView workbookViewId="0" topLeftCell="B4"/></sheetViews>"#
        } else {
            ""
        },
        r = if user { 9 } else { 8 }
    );
    let error = format!(
        r#"<sheetFormatPr defaultRowHeight="15"/>{}<sheetData>{error}</sheetData>"#,
        if user {
            r#"<cols><col style="3" width="30" max="6" min="6"/></cols>"#
        } else {
            r#"<cols><col min="6" max="6" width="30" style="2"/></cols>"#
        }
    );
    let defined = if user {
        r#"<definedName localSheetId="0" name="_xlnm.Print_Area">History!$A$1:$K$30</definedName>"#
    } else {
        r#"<definedName name="_xlnm.Print_Area" localSheetId="0">History!$A$1:$K$29</definedName>"#
    };
    let workbook = format!(
        r#"<workbook xmlns="{MAIN}" xmlns:r="{REL}"><bookViews><workbookView activeTab="{}"/></bookViews><sheets><sheet name="History" sheetId="1" r:id="s1"/><sheet name="UseCase" sheetId="2" r:id="s2"/><sheet name="ERROR MSG" sheetId="3" r:id="s3"/></sheets><definedNames>{defined}</definedNames></workbook>"#,
        u8::from(user)
    );
    let mut rels = format!(
        r#"<Relationships><Relationship Id="style" Type="{REL}/styles" Target="styles.xml"/><Relationship Id="shared" Type="{REL}/sharedStrings" Target="sharedStrings.xml"/>"#
    );
    for i in 1..=3 {
        rels.push_str(&format!(
            r#"<Relationship Id="s{i}" Type="{REL}/worksheet" Target="worksheets/sheet{i}.xml"/>"#
        ));
    }
    rels.push_str("</Relationships>");
    let mut parts = vec![
        ("xl/workbook.xml".into(), workbook),
        ("xl/_rels/workbook.xml.rels".into(), rels),
        ("xl/styles.xml".into(), styles(user)),
        (
            "xl/sharedStrings.xml".into(),
            format!(r#"<sst xmlns="{MAIN}">{}</sst>"#, shared.join("")),
        ),
    ];
    for (i, s) in [history, usecase, error].iter().enumerate() {
        parts.push((
            format!("xl/worksheets/sheet{}.xml", i + 1),
            format!(r#"<worksheet xmlns="{MAIN}">{s}</worksheet>"#),
        ));
    }
    write_parts(path, parts);
}
fn write_parts(path: &Path, parts: Vec<(String, String)>) {
    let mut zip = ZipWriter::new(fs::File::create(path).unwrap());
    for (name, data) in parts {
        zip.start_file(name, SimpleFileOptions::default()).unwrap();
        zip.write_all(data.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
}
fn patch(source: &Path, output: &Path, operations: Value, dry: bool) -> crate::Result<Value> {
    let inspected = execute(
        json!({"schemaVersion":1,"operation":"inspect","source":source,"ranges":["History!A11"]}),
    )
    .unwrap();
    execute(
        json!({"schemaVersion":1,"operation":"patch","source":source,"output":output,"expectedSha256":inspected["sourceSha256"],"operations":operations,"validateOnly":dry}),
    )
}
fn trio() -> (TempDir, PathBuf, PathBuf) {
    let dir = TempDir::new().unwrap();
    let base = dir.path().join("baseline.xlsx");
    let output = dir.path().join("output.xlsx");
    let user = dir.path().join("user.xlsx");
    fixture(&base, false, "");
    fixture(&user, true, "");
    patch(&base,&output,json!([
        {"op":"appendText","sheet":"UseCase","cell":"J5","expectedText":"Old rule","value":"\nNew rule","fontColor":"0000FF","strike":false},
        {"op":"appendText","sheet":"ERROR MSG","cell":"F7","expectedText":"Error Condition","value":"\nMSG text","fontColor":"0000FF","strike":false}
    ]),false).unwrap();
    (dir, output, user)
}
fn diff(before: &Path, after: &Path, align: bool) -> Value {
    execute(json!({"schemaVersion":1,"operation":"diff","before":before,"after":after,"semantic":true,"alignRows":align,"maxCells":2000})).unwrap()
}
fn details(v: &Value, sheet: &str) -> Vec<Value> {
    v["semantic"]["cellChanges"]["details"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["sheet"] == sheet)
        .cloned()
        .collect()
}
fn has(v: &Value, tag: &str) -> bool {
    v["changes"].as_array().unwrap().contains(&json!(tag))
}
fn check_history(v: &Value) {
    let cells = details(v, "History");
    assert_eq!(cells.len(), 9, "{cells:#?}");
    let a = cells.iter().find(|c| c["cell"] == "A11").unwrap();
    assert!(has(a, "type") && has(a, "font"));
    assert_eq!(a["before"]["kind"], "string");
    assert_eq!(a["after"]["kind"], "number");
    let c = cells.iter().find(|c| c["cell"] == "C11").unwrap();
    assert!(has(c, "text"));
    assert_eq!(c["before"]["text"], "2026.09.24");
    assert_eq!(c["after"]["text"], "2026.09.28");
}
#[test]
fn semantic_position_removes_wps_noise_but_keeps_user_edits() {
    let (_dir, output, user) = trio();
    let v = diff(&output, &user, false);
    check_history(&v);
    assert!(details(&v, "ERROR MSG").is_empty());
    assert!(!details(&v, "UseCase").iter().any(|c| c["cell"] == "J5"));
    let samples = v["semantic"]["equivalentOnly"]["sample"]
        .as_array()
        .unwrap();
    for s in ["ERROR MSG!F7", "UseCase!J5"] {
        assert!(samples.contains(&json!(s)));
    }
    assert!(
        v["cellChanges"]["details"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["sheet"] == "ERROR MSG" && c["cell"] == "F7")
    );
    let layout = &v["semantic"]["layoutChanges"];
    assert_eq!(layout["rows"]["total"], 2);
    for (c, row) in layout["rows"]["details"]
        .as_array()
        .unwrap()
        .iter()
        .zip([7, 8])
    {
        assert_eq!(c["sheet"], "ERROR MSG");
        assert_eq!(c["row"], row);
        assert_eq!(c["before"]["height"], 17.0);
        assert_eq!(c["after"]["height"], 34.0);
    }
    assert_eq!(layout["columns"]["total"], 0);
    assert_eq!(
        layout["merges"],
        json!([{"sheet":"History","added":["E12:F12"],"removed":[]},{"sheet":"UseCase","added":["B9:C9"],"removed":["B8:C8"]}])
    );
    assert_eq!(
        layout["printAreas"],
        json!([{"sheet":"History","name":"_xlnm.Print_Area","before":"History!$A$1:$K$29","after":"History!$A$1:$K$30"}])
    );
    assert_eq!(
        v["semantic"]["viewOnly"],
        json!({"sheets":["History","UseCase"],"workbook":true})
    );
}
#[test]
fn semantic_align_rows_exposes_insert_and_edit_without_shift_noise() {
    let (_dir, output, user) = trio();
    let v = diff(&output, &user, true);
    check_history(&v);
    let rows = v["semantic"]["rowAlignment"].as_array().unwrap();
    let uc = rows.iter().find(|r| r["sheet"] == "UseCase").unwrap();
    assert_eq!(uc["inserted"], json!([{"afterRow":6,"count":1}]));
    let cells = details(&v, "UseCase");
    assert_eq!(cells.len(), 2, "{cells:#?}");
    assert_eq!(cells[0]["cell"], "B6");
    assert!(has(&cells[0], "inserted"));
    assert_eq!(cells[1]["cell"], "B8");
    assert_eq!(cells[1]["beforeCell"], "B7");
    assert!(has(&cells[1], "text"));
    assert!(
        !v["semantic"]["layoutChanges"]["merges"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["sheet"] == "UseCase")
    );
}
#[test]
fn semantic_replace_rich_text_and_dry_run() {
    let (dir, output, _user) = trio();
    let target = dir.path().join("replaced.xlsx");
    let ops = json!([
        {"op":"setRichText","sheet":"UseCase","cell":"J5","expectedText":"Old rule\nNew rule","runs":[{"text":"Old rule","strike":true}]},
        {"op":"setText","sheet":"ERROR MSG","cell":"F7","expectedText":"Error Condition\nMSG text","value":"Replacement","replaceRichText":true}
    ]);
    let dry = patch(&output, &target, ops.clone(), true).unwrap();
    assert_eq!(dry["violations"], json!([]));
    assert!(!target.exists());
    patch(&output, &target, ops, false).unwrap();
    let v = diff(&output, &target, false);
    let j = details(&v, "UseCase").remove(0);
    assert_eq!(j["after"]["text"], "Old rule");
    assert!(j["after"]["runs"].is_null());
    assert_eq!(j["after"]["style"]["font"]["strike"], true);
    let f = details(&v, "ERROR MSG").remove(0);
    assert_eq!(f["after"]["text"], "Replacement");
    assert!(f["after"]["runs"].is_null());
    assert_eq!(f["after"]["style"]["font"]["strike"], true);
}
#[test]
fn semantic_rich_text_refusals_are_located_and_do_not_write() {
    let (dir, output, _) = trio();
    let target = dir.path().join("no.xlsx");
    let refused=patch(&output,&target,json!([{"op":"setText","sheet":"UseCase","cell":"J5","expectedText":"Old rule\nNew rule","value":"x"}]),false).unwrap_err().to_string();
    assert!(refused.contains("rich text or annotated string requires native application; use setText replaceRichText:true or setRichText"),"{refused}");
    for op in [
        json!({"op":"setRichText","sheet":"UseCase","cell":"J5","expectedText":"wrong","runs":[{"text":"x"}]}),
        json!({"op":"setText","sheet":"UseCase","cell":"J5","expectedText":"wrong","value":"x","replaceRichText":true}),
        json!({"op":"setRichText","sheet":"UseCase","cell":"J5","expectedText":"Old rule\nNew rule","runs":[]}),
        json!({"op":"setRichText","sheet":"UseCase","cell":"J5","expectedText":"Old rule\nNew rule","runs":[{"text":""}]}),
        json!({"op":"setRichText","sheet":"History","cell":"A9","expectedText":"1","runs":[{"text":"x"}]}),
    ] {
        let r = patch(&output, &target, json!([op]), true).unwrap();
        assert_eq!(r["violations"].as_array().unwrap().len(), 1);
        assert_eq!(r["targetsProcessed"], 0);
        assert!(!target.exists());
    }
    let odd = dir.path().join("odd.xlsx");
    fixture(
        &odd,
        false,
        r#"<row r="20"><c r="A20"><f>1+1</f><v>2</v></c><c r="B20" t="inlineStr"><is><r><t>x</t></r><rPh sb="0" eb="1"><t>p</t></rPh></is></c><c r="C20" t="inlineStr"><is><r xmlns:z="urn:foreign"><z:t>x</z:t></r></is></c></row>"#,
    );
    for c in ["A20", "B20", "C20"] {
        for op in ["setRichText", "setText"] {
            let mut edit = json!({"op":op,"sheet":"UseCase","cell":c,"expectedText":"x"});
            if op == "setText" {
                edit["value"] = json!("y");
                edit["replaceRichText"] = json!(true);
            } else {
                edit["runs"] = json!([{"text":"y"}]);
            }
            assert!(patch(&odd, &target, json!([edit]), false).is_err());
        }
    }
}
#[test]
fn semantic_parameters_are_diff_only_and_alignment_is_opt_in() {
    for request in [
        json!({"schemaVersion":1,"operation":"diff","before":"/a","after":"/b","alignRows":true}),
        json!({"schemaVersion":1,"operation":"inspect","semantic":true}),
        json!({"schemaVersion":1,"operation":"patch","semantic":true}),
    ] {
        let e = execute(request).unwrap_err().to_string();
        assert!(e.contains("semantic"), "{e}");
    }
}

fn simple(path: &Path, body: &str, style: &str) {
    write_parts(
        path,
        vec![
            (
                "xl/workbook.xml".into(),
                format!(
                    r#"<workbook xmlns="{MAIN}" xmlns:r="{REL}"><sheets><sheet name="Data" sheetId="1" r:id="s"/></sheets></workbook>"#
                ),
            ),
            (
                "xl/_rels/workbook.xml.rels".into(),
                format!(
                    r#"<Relationships><Relationship Id="s" Type="{REL}/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="style" Type="{REL}/styles" Target="styles.xml"/></Relationships>"#
                ),
            ),
            ("xl/styles.xml".into(), style.into()),
            (
                "xl/worksheets/sheet1.xml".into(),
                format!(r#"<worksheet xmlns="{MAIN}">{body}</worksheet>"#),
            ),
        ],
    );
}
#[test]
fn semantic_value_kinds_formula_cache_and_missing_cells() {
    let dir = TempDir::new().unwrap();
    let b = dir.path().join("b.xlsx");
    let a = dir.path().join("a.xlsx");
    let before = r#"<sheetData><row r="1"><c r="A1"><v>1.10</v></c><c r="B1" t="b"><v>0</v></c><c r="C1"><f>1+1</f><v>2</v></c><c r="D1"><f t="shared" si="7"/></c><c r="E1" t="d"><v>2026-09-28</v></c><c r="F1" t="e"><v>#N/A</v></c></row></sheetData>"#;
    let after = r#"<sheetData><row r="1"><c r="A1" s="0"><v>1.1</v></c><c r="B1" t="b"><v>false</v></c><c r="C1"><f>1+1</f><v>3</v></c><c r="D1"><f t="shared" si="8"/></c><c r="E1" t="d"><v>2026-09-29</v></c><c r="F1" t="e"><v>#DIV/0!</v></c><c r="G1" s="0"/></row></sheetData>"#;
    simple(&b, before, &styles(false));
    simple(&a, after, &styles(false));
    let v = diff(&b, &a, false);
    let changes = details(&v, "Data");
    assert_eq!(
        changes
            .iter()
            .map(|c| c["cell"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["C1", "D1", "E1", "F1"]
    );
    assert_eq!(changes[0]["changes"], json!(["cachedValue"]));
    assert_eq!(
        changes[0]["after"]["cachedValue"],
        json!({"kind":"number","text":"3"})
    );
    assert_eq!(changes[1]["before"]["formula"], "shared:7");
    assert!(has(&changes[1], "formula"));
    assert_eq!(changes[2]["after"]["kind"], "date");
    assert_eq!(changes[3]["after"]["kind"], "error");
    assert_eq!(v["semantic"]["equivalentOnly"]["total"], 3);
}
#[test]
fn semantic_runs_are_self_contained_and_equivalent_segments_collapse() {
    let dir = TempDir::new().unwrap();
    let b = dir.path().join("b.xlsx");
    let a = dir.path().join("a.xlsx");
    let before = r#"<sheetData><row r="1"><c r="A1" s="2" t="inlineStr"><is><t>same</t></is></c><c r="B1" s="2" t="inlineStr"><is><r><rPr><rFont val="Arial"/><sz val="11"/><color rgb="ff000000"/><strike val="false"/></rPr><t>same</t></r></is></c><c r="C1" s="2" t="inlineStr"><is><r><rPr><rFont val="Arial"/><sz val="11"/><color rgb="FF000000"/><strike/></rPr><t>same</t></r></is></c></row></sheetData>"#;
    let after = r#"<sheetData><row r="1"><c r="A1" s="2" t="inlineStr"><is><t>sa</t><r><t>me</t></r><r><t/></r></is></c><c r="B1" s="2" t="inlineStr"><is><r><rPr><color rgb="000000"/><sz val="11.0"/><rFont val="Arial"/><charset val="134"/><family val="2"/></rPr><t>same</t></r></is></c><c r="C1" s="2" t="inlineStr"><is><r><rPr><rFont val="Arial"/><sz val="11"/><color rgb="FF000000"/></rPr><t>same</t></r></is></c></row></sheetData>"#;
    simple(&b, before, &styles(false));
    simple(&a, after, &styles(false));
    let v = diff(&b, &a, false);
    let changes = details(&v, "Data");
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0]["cell"], "C1");
    assert_eq!(changes[0]["changes"], json!(["runs"]));
    assert!(changes[0]["before"]["runs"].is_null());
    assert!(changes[0]["after"]["runs"][0].get("strike").is_none());
    assert_eq!(v["semantic"]["equivalentOnly"]["total"], 2);
}
#[test]
fn semantic_style_defaults_colors_layout_and_detail_caps() {
    let dir = TempDir::new().unwrap();
    let b = dir.path().join("b.xlsx");
    let a = dir.path().join("a.xlsx");
    let before = r#"<sheetFormatPr defaultRowHeight="15" defaultColWidth="10"/><cols><col min="1" max="3" width="10"/></cols><sheetData><row r="1" ht="15" spans="1:3"><c r="A1" t="inlineStr"><is><t>x</t></is></c></row></sheetData>"#;
    let after = r#"<sheetFormatPr defaultColWidth="10" defaultRowHeight="15"/><cols><col max="1" min="1" width="10.0" bestFit="1" customWidth="1"/><col min="2" max="3"/></cols><sheetData><row r="1"><c r="A1" s="0" t="inlineStr"><is><t>x</t></is></c></row></sheetData>"#;
    let s = styles(false);
    let equivalent=s.replace("<name val=\"Arial\"/><sz val=\"11\"/><color rgb=\"FF000000\"/>","<sz val=\"11.0\"/><color rgb=\"000000\"/><b val=\"0\"/><i val=\"false\"/><strike val=\"0\"/><name val=\"Arial\"/>").replace("<xf/>",r#"<xf xfId="0" applyFont="0" quotePrefix="false"><alignment horizontal="general" vertical="bottom" wrapText="0" indent="0" textRotation="0" shrinkToFit="false" readingOrder="0"/><protection locked="1" hidden="0"/></xf>"#);
    simple(&b, before, &s);
    simple(&a, after, &equivalent);
    let v = diff(&b, &a, false);
    assert_eq!(v["semantic"]["summary"]["semanticTotal"], 0);
    let changed = after
        .replace("<row r=\"1\">", "<row r=\"1\" ht=\"30\">")
        .replace("</sheetData>", "<row r=\"2\" ht=\"31\"/></sheetData>")
        .replace("width=\"10.0\"", "width=\"20\"")
        .replace(
            "<col min=\"2\" max=\"3\"/>",
            "<col min=\"2\" max=\"3\" width=\"20\"/>",
        );
    simple(
        &a,
        &changed,
        &s.replace("rgb=\"FF000000\"", "theme=\"1\" tint=\"0\""),
    );
    let v=execute(json!({"schemaVersion":1,"operation":"diff","before":b,"after":a,"semantic":true,"maxCells":1})).unwrap();
    assert!(has(&details(&v, "Data")[0], "font"));
    assert_eq!(
        v["semantic"]["layoutChanges"]["columns"]["details"][0]["columns"],
        "A:C"
    );
    assert_eq!(v["semantic"]["layoutChanges"]["rows"]["total"], 2);
    assert_eq!(v["semantic"]["layoutChanges"]["rows"]["truncated"], true);
    assert_eq!(v["semantic"]["truncated"], true);
}
#[test]
fn semantic_alignment_handles_deletions_repeated_rows_and_large_unmatched_blocks() {
    let dir = TempDir::new().unwrap();
    let b = dir.path().join("b.xlsx");
    let a = dir.path().join("a.xlsx");
    let body = |values: &[u32]| {
        let mut out = String::from("<sheetData>");
        for (i, v) in values.iter().enumerate() {
            let r = i + 1;
            out.push_str(&format!(r#"<row r="{r}"><c r="A{r}"><v>{v}</v></c></row>"#));
        }
        out.push_str("</sheetData>");
        out
    };
    let mut seed = 7u32;
    for case in 0..60 {
        let mut values = Vec::new();
        for _ in 0..(case % 19 + 1) {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            values.push(seed % 7);
        }
        let mut changed = values.clone();
        let at = case % changed.len();
        changed.remove(at);
        changed.insert(case % changed.len().max(1), 100 + case as u32);
        simple(&b, &body(&values), &styles(false));
        simple(&a, &body(&changed), &styles(false));
        let v = diff(&b, &a, true);
        assert!(v["semantic"]["summary"]["semanticTotal"].as_u64().unwrap() > 0);
        assert!(
            details(&v, "Data")
                .iter()
                .any(|c| c["after"]["text"] == json!((100 + case).to_string()))
        );
    }
    let values: Vec<_> = (1..=205).collect();
    let changed: Vec<_> = (301..=506).collect();
    simple(&b, &body(&values), &styles(false));
    simple(&a, &body(&changed), &styles(false));
    let v = diff(&b, &a, true);
    assert_eq!(v["semantic"]["cellChanges"]["total"], 206);
    simple(&b, &body(&[1, 2, 3, 4]), &styles(false));
    simple(&a, &body(&[1, 3, 4]), &styles(false));
    let v = diff(&b, &a, true);
    let changes = details(&v, "Data");
    assert_eq!(changes.len(), 1);
    assert!(has(&changes[0], "deleted"));
    assert_eq!(
        v["semantic"]["rowAlignment"][0]["deleted"],
        json!([{"beforeRow":2,"count":1}])
    );
}
#[test]
fn semantic_rich_replacement_bounds_merge_rollback_and_shared_identity() {
    let (dir, _output, user) = trio();
    let target = dir.path().join("candidate.xlsx");
    let source = fs::read(&user).unwrap();
    for runs in [
        json!([{"text":"😀".repeat(16384)}]),
        json!([{"text":"a\u{0001}"}]),
        json!([{"text":"_x00"},{"text":"41_"}]),
        json!((0..257).map(|_| json!({"text":"x"})).collect::<Vec<_>>()),
    ] {
        let e = patch(
            &user,
            &target,
            json!([{"op":"setRichText","sheet":"UseCase","cell":"J5","expectedText":"Old rule\nNew rule","runs":runs}]),
            false,
        );
        assert!(e.is_err());
        assert!(!target.exists());
    }
    for op in [
        json!({"op":"setRichText","sheet":"UseCase","cell":"C9","expectedText":"","runs":[{"text":"x"}]}),
        json!({"op":"setText","sheet":"UseCase","cell":"C9","expectedText":"","value":"x","replaceRichText":true}),
    ] {
        assert!(
            patch(&user, &target, json!([op]), false)
                .unwrap_err()
                .to_string()
                .contains("covered merged")
        );
    }
    let report=patch(&user,&target,json!([
        {"op":"setRichText","sheet":"UseCase","cell":"J5","expectedText":"Old rule\nNew rule","runs":[{"text":"bad","fontColor":"invalid"}]},
        {"op":"setRichText","sheet":"UseCase","cell":"J5","expectedText":"Old rule\nNew rule","runs":[{"text":"replacement","strike":false}]}
    ]),true).unwrap();
    assert_eq!(report["violations"].as_array().unwrap().len(), 1);
    assert_eq!(report["targetsProcessed"], 1);
    assert!(!target.exists());
    assert_eq!(fs::read(&user).unwrap(), source);
    let report=patch(&user,&target,json!([
        {"op":"setRichText","sheet":"UseCase","cell":"J5","expectedText":"Old rule\nNew rule","runs":[{"text":"replacement","strike":false}]},
        {"op":"setRichText","sheet":"UseCase","cell":"A30","expectedText":"","runs":[{"text":"created","bold":true}]}
    ]),false).unwrap();
    assert_eq!(report["changedParts"], json!(["xl/worksheets/sheet2.xml"]));
    assert_eq!(fs::read(&user).unwrap(), source);
    let v = diff(&user, &target, false);
    let j = details(&v, "UseCase")
        .into_iter()
        .find(|c| c["cell"] == "J5")
        .unwrap();
    assert!(j["after"]["runs"][0].get("strike").is_none());
    assert_eq!(j["after"]["style"]["font"]["strike"], true);
}
#[test]
fn semantic_output_budget_retains_semantics_before_legacy_details() {
    let dir = TempDir::new().unwrap();
    let b = dir.path().join("b.xlsx");
    let a = dir.path().join("a.xlsx");
    let body = |suffix: &str| {
        let mut out = String::from("<sheetData>");
        for r in 1..=100 {
            out.push_str(&format!(
                r#"<row r="{r}"><c r="A{r}" t="inlineStr"><is><t>{}{suffix}</t></is></c></row>"#,
                "x".repeat(30000)
            ));
        }
        out.push_str("</sheetData>");
        out
    };
    simple(&b, &body("a"), &styles(false));
    simple(&a, &body("b"), &styles(false));
    let v = diff(&b, &a, false);
    assert_eq!(v["outputTruncated"], true);
    assert_eq!(v["semantic"]["cellChanges"]["total"], 100);
    assert_eq!(
        v["semantic"]["cellChanges"]["details"]
            .as_array()
            .unwrap()
            .len(),
        100
    );
    assert_eq!(v["semantic"]["cellChanges"]["truncated"], false);
    assert_eq!(v["cellChanges"]["total"], 100);
    assert_eq!(v["cellChanges"]["truncated"], true);
    assert!(serde_json::to_vec(&v).unwrap().len() <= 8 * 1024 * 1024);
}

#[test]
fn semantic_style_components_compare_properties_not_serialization() {
    let dir = TempDir::new().unwrap();
    let b = dir.path().join("b.xlsx");
    let a = dir.path().join("a.xlsx");
    let body = r#"<sheetData><row r="1"><c r="A1"><v>1</v></c></row></sheetData>"#;
    let before = format!(
        r#"<styleSheet xmlns="{MAIN}"><numFmts><numFmt numFmtId="164" formatCode="0.00"/></numFmts><fonts><font><name val="Arial"/><sz val="11"/><color theme="2" tint="0"/></font></fonts><fills><fill><patternFill patternType="solid"><fgColor rgb="abcDEF"/><bgColor indexed="64"/></patternFill></fill></fills><borders><border><left style="thin"><color rgb="000000"/></left><right style="none"/><bottom/></border></borders><cellXfs><xf numFmtId="164" quotePrefix="0"><alignment horizontal="general" vertical="bottom"/><protection locked="true" hidden="false"/></xf></cellXfs></styleSheet>"#
    );
    let after = format!(
        r#"<styleSheet xmlns="{MAIN}"><numFmts><numFmt formatCode="0.00" numFmtId="165"/></numFmts><fonts><font><color theme="2"/><sz val="11.0"/><name val="Arial"/><b val="false"/></font></fonts><fills><fill><patternFill patternType="solid"><bgColor rgb="FFFFFFFF"/><fgColor rgb="FFABCDEF"/></patternFill></fill></fills><borders><border diagonalUp="false" diagonalDown="0"><left style="thin"><color rgb="FF000000"/></left></border></borders><cellXfs><xf numFmtId="165" applyFont="0" applyFill="false" xfId="0"><protection/></xf></cellXfs></styleSheet>"#
    );
    simple(&b, body, &before);
    simple(&a, body, &after);
    let v = diff(&b, &a, false);
    assert_eq!(v["semantic"]["cellChanges"]["total"], 0);
    assert_eq!(v["semantic"]["equivalentOnly"]["total"], 1);
    let changed = after
        .replace("theme=\"2\"", "theme=\"3\"")
        .replace("FFABCDEF", "FFABCDED")
        .replace("style=\"thin\"", "style=\"medium\"")
        .replace("formatCode=\"0.00\"", "formatCode=\"0.000\"")
        .replace(
            "<protection/>",
            "<alignment vertical=\"top\"/><protection locked=\"false\"/>",
        )
        .replace("<xf numFmtId", "<xf quotePrefix=\"1\" numFmtId");
    simple(&a, body, &changed);
    let v = diff(&b, &a, false);
    assert_eq!(
        details(&v, "Data")[0]["changes"],
        json!([
            "font",
            "fill",
            "border",
            "alignment",
            "numberFormat",
            "protection",
            "quotePrefix"
        ])
    );
}
fn rewrite(path: &Path, change: impl Fn(&str, String) -> String) {
    use std::io::Read;
    let mut archive = zip::ZipArchive::new(fs::File::open(path).unwrap()).unwrap();
    let mut parts = Vec::new();
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).unwrap();
        let name = file.name().to_owned();
        let mut value = String::new();
        file.read_to_string(&mut value).unwrap();
        parts.push((name.clone(), change(&name, value)));
    }
    drop(archive);
    write_parts(path, parts);
}
#[test]
fn semantic_workbook_names_theme_defaults_other_structure_and_nested_caps() {
    let (dir, output, user) = trio();
    rewrite(&user, |name, s| {
        match name {
        "xl/workbook.xml"=>s.replace("</definedNames>",r#"<definedName name="_xlnm.Print_Titles" localSheetId="1">UseCase!$1:$3</definedName><definedName name="Rate">0.1</definedName></definedNames>"#),
        "xl/worksheets/sheet1.xml"=>s.replace("defaultRowHeight=\"15\"","defaultRowHeight=\"16\"").replace("<mergeCells>","<mergeCells><mergeCell ref=\"G12:H12\"/>").replace("</worksheet>","<pageMargins left=\"0.7\"/></worksheet>"),_=>s
    }
    });
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&user)
        .unwrap();
    let mut zip = ZipWriter::new_append(file).unwrap();
    zip.start_file("xl/theme/theme1.xml", SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"<theme/>").unwrap();
    zip.finish().unwrap();
    let v = diff(&output, &user, false);
    assert_eq!(
        v["semantic"]["workbookChanges"]["definedNames"],
        json!([{"name":"Rate","scope":null,"before":null,"after":"0.1"}])
    );
    assert_eq!(v["semantic"]["workbookChanges"]["themeChanged"], true);
    assert!(
        v["semantic"]["layoutChanges"]["printAreas"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["sheet"] == "UseCase" && c["name"] == "_xlnm.Print_Titles")
    );
    assert_eq!(
        v["semantic"]["layoutChanges"]["sheetDefaults"][0]["after"]["defaultRowHeight"],
        16.0
    );
    assert_eq!(
        v["semantic"]["layoutChanges"]["otherStructure"][0]["elements"],
        json!(["pageMargins"])
    );
    let limited=execute(json!({"schemaVersion":1,"operation":"diff","before":output,"after":user,"semantic":true,"maxCells":1})).unwrap();
    assert_eq!(
        limited["semantic"]["layoutChanges"]["merges"][0]["addedTotal"],
        2
    );
    assert_eq!(limited["semantic"]["layoutChanges"]["truncated"], true);
    let same = dir.path().join("same.xlsx");
    fs::copy(&user, &same).unwrap();
    let v = diff(&user, &same, true);
    assert_eq!(v["semantic"]["layoutChanges"]["merges"], json!([]));
}
#[test]
fn semantic_byte_limit_marks_both_cell_lists_without_losing_totals() {
    let dir = TempDir::new().unwrap();
    let b = dir.path().join("b.xlsx");
    let a = dir.path().join("a.xlsx");
    let body = |suffix: &str| {
        let mut out = String::from("<sheetData>");
        for r in 1..=150 {
            out.push_str(&format!(
                r#"<row r="{r}"><c r="A{r}" t="inlineStr"><is><t>{}{suffix}</t></is></c></row>"#,
                "x".repeat(30000)
            ));
        }
        out.push_str("</sheetData>");
        out
    };
    simple(&b, &body("a"), &styles(false));
    simple(&a, &body("b"), &styles(false));
    let v = diff(&b, &a, false);
    assert_eq!(v["outputTruncated"], true);
    assert_eq!(v["semantic"]["truncated"], true);
    for cells in [&v["cellChanges"], &v["semantic"]["cellChanges"]] {
        assert_eq!(cells["total"], 150);
        assert_eq!(cells["truncated"], true);
    }
    let cells = v["semantic"]["cellChanges"]["details"].as_array().unwrap();
    assert!(cells.len() > 100);
    for (i, c) in cells.iter().enumerate() {
        assert_eq!(c["cell"], format!("A{}", i + 1));
    }
    assert!(serde_json::to_vec(&v).unwrap().len() <= 8 * 1024 * 1024);
}
