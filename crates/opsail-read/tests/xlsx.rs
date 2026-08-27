use std::io::Write as _;
use std::path::Path;

use opsail_read::{
    CellValueType, FormulaKind, ReadArtifact, ReadError, ReadOptions, ReadSource, SheetState,
    WorkbookPartRevision, WorkbookRevision, WorkbookSession, merge_markdown_mirror, read_artifact,
};
use tempfile::tempdir;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
  <Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
  <Override PartName="/xl/worksheets/sheet2.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
  <Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/>
  <Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/>
</Types>"#;

const WORKBOOK: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
 xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <workbookPr date1904="0"/>
  <sheets>
    <sheet name="Main" sheetId="1" r:id="rId1"/>
    <sheet name="Hidden Data" sheetId="2" state="hidden" r:id="rId2"/>
  </sheets>
  <definedNames>
    <definedName name="BrokenName">#REF!</definedName>
  </definedNames>
</workbook>"#;

const RELATIONSHIPS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml"/>
  <Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
  <Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/>
</Relationships>"#;

const SHARED_STRINGS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="2" uniqueCount="2">
  <si><t>Name</t></si>
  <si><r><t>A &amp; </t></r><r><t>B</t></r></si>
</sst>"#;

const STYLES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <numFmts count="1"><numFmt numFmtId="164" formatCode="yyyy-mm-dd"/></numFmts>
  <fonts count="1"><font/></fonts><fills count="1"><fill/></fills>
  <borders count="1"><border/></borders>
  <cellStyleXfs count="1"><xf numFmtId="0"/></cellStyleXfs>
  <cellXfs count="2"><xf numFmtId="0"/><xf numFmtId="164" applyNumberFormat="1"/></cellXfs>
</styleSheet>"#;

const POSITIVE_SHEET: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
 xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"
 xmlns:x14="http://schemas.microsoft.com/office/spreadsheetml/2009/9/main">
  <dimension ref="A1:D4"/>
  <cols><col min="4" max="4" hidden="1" outlineLevel="2"/></cols>
  <sheetData>
    <row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c></row>
    <row r="2"><c r="A2"><v>42</v></c><c r="B2" t="b"><v>1</v></c><c r="C2"><f t="shared" si="0" ref="C2:D2">SUM(A2,8)</f><v>50</v></c><c r="D2"><f t="shared" si="0"/><v>51</v></c></row>
    <row r="3" hidden="1" outlineLevel="1"><c r="A3" s="1"><v>45000</v></c></row>
    <row r="4"><c r="A4" t="inlineStr"><is><r><t>Merged</t></r></is></c></row>
  </sheetData>
  <mergeCells count="1"><mergeCell ref="A4:B4"/></mergeCells>
  <conditionalFormatting sqref="A2:D2"><cfRule type="cellIs" priority="1"/></conditionalFormatting>
  <dataValidations count="1"><dataValidation type="whole" sqref="A2:A3"/></dataValidations>
  <hyperlinks><hyperlink ref="A1" r:id="rIdHyperlink" display="Example"/></hyperlinks>
  <autoFilter ref="A1:D4"/>
  <pageMargins left="0.7" right="0.7" top="0.75" bottom="0.75" header="0.3" footer="0.3"/>
  <pageSetup orientation="landscape"/>
  <headerFooter/>
  <drawing r:id="rIdDrawing"/>
  <legacyDrawing r:id="rIdComments"/>
  <tableParts count="1"><tablePart r:id="rIdTable"/></tableParts>
  <controls><control r:id="rIdControl" shapeId="1"/></controls>
  <extLst><ext><x14:sparklineGroups><x14:sparklineGroup><x14:sparklines><x14:sparkline/></x14:sparklines></x14:sparklineGroup></x14:sparklineGroups></ext></extLst>
</worksheet>"#;

const SHEET_RELATIONSHIPS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rIdHyperlink" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.com" TargetMode="External"/>
  <Relationship Id="rIdDrawing" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing" Target="../drawings/drawing1.xml"/>
  <Relationship Id="rIdComments" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="../comments1.xml"/>
  <Relationship Id="rIdTable" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/table" Target="../tables/table1.xml"/>
  <Relationship Id="rIdControl" Type="http://schemas.microsoft.com/office/2006/relationships/ctrlProp" Target="../ctrlProps/ctrlProp1.xml"/>
</Relationships>"#;

const HIDDEN_SHEET: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <dimension ref="A1"/><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>hidden</t></is></c></row></sheetData>
</worksheet>"#;

const ADVERSARIAL_SHEET: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <dimension ref="A1:XFD1048576"/>
  <sheetData>
    <row r="1"><c r="A1" t="inlineStr"><is><t>semantic</t></is></c></row>
    <row r="1048576"><c r="XFD1048576" s="1"/></row>
  </sheetData>
</worksheet>"#;

fn write_fixture(path: &Path, first_sheet: &str) {
    write_fixture_parts(path, first_sheet, SHARED_STRINGS);
}

fn write_fixture_parts(path: &Path, first_sheet: &str, shared_strings: &str) {
    write_fixture_parts_with_styles(path, first_sheet, shared_strings, STYLES);
}

fn write_fixture_parts_with_styles(
    path: &Path,
    first_sheet: &str,
    shared_strings: &str,
    styles: &str,
) {
    let file = std::fs::File::create(path).unwrap();
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, content) in [
        ("[Content_Types].xml", CONTENT_TYPES),
        ("xl/workbook.xml", WORKBOOK),
        ("xl/_rels/workbook.xml.rels", RELATIONSHIPS),
        ("xl/sharedStrings.xml", shared_strings),
        ("xl/styles.xml", styles),
        ("xl/worksheets/sheet1.xml", first_sheet),
        ("xl/worksheets/_rels/sheet1.xml.rels", SHEET_RELATIONSHIPS),
        ("xl/worksheets/sheet2.xml", HIDDEN_SHEET),
        ("xl/theme/theme1.xml", "<theme/>"),
        ("xl/drawings/drawing1.xml", "<drawing/>"),
        ("xl/charts/chart1.xml", "<chart/>"),
        ("xl/media/image1.png", "fixture"),
        ("xl/tables/table1.xml", "<table/>"),
        ("xl/comments1.xml", "<comments/>"),
        ("xl/ctrlProps/ctrlProp1.xml", "<formControlPr/>"),
    ] {
        zip.start_file(name, options).unwrap();
        zip.write_all(content.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
}

#[tokio::test]
async fn excludes_phonetic_runs_from_shared_string_display_text() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("phonetic.xlsx");
    let shared_strings = SHARED_STRINGS.replace(
        "<si><t>Name</t></si>",
        r#"<si><r><t>漢字</t></r><rPh sb="0" eb="2"><t>かんじ</t></rPh></si>"#,
    );
    write_fixture_parts(&path, POSITIVE_SHEET, &shared_strings);
    let mut options = ReadOptions::default();
    options.spreadsheet.ranges = vec!["Main!A1".to_owned()];

    let ReadArtifact::Workbook(result) = read_artifact(ReadSource::File(path), &options)
        .await
        .unwrap()
    else {
        panic!("expected workbook artifact");
    };

    assert_eq!(result.workbook.selections[0].cells[0].display, "漢字");
}

#[tokio::test]
async fn color_directives_do_not_turn_numbers_into_dates() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("color-format.xlsx");
    let styles = STYLES.replace("yyyy-mm-dd", "[Red]0.00");
    let sheet = POSITIVE_SHEET.replace(
        r#"<c r="A2"><v>42</v></c>"#,
        r#"<c r="A2" s="1"><v>42</v></c>"#,
    );
    write_fixture_parts_with_styles(&path, &sheet, SHARED_STRINGS, &styles);
    let mut options = ReadOptions::default();
    options.spreadsheet.ranges = vec!["Main!A2".to_owned()];

    let ReadArtifact::Workbook(result) = read_artifact(ReadSource::File(path), &options)
        .await
        .unwrap()
    else {
        panic!("expected workbook artifact");
    };
    let cell = &result.workbook.selections[0].cells[0];

    assert!(matches!(cell.value_type, CellValueType::Number));
    assert_eq!(cell.display, "42");
}

#[tokio::test]
async fn elapsed_hour_formats_preserve_whole_days() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("elapsed-hours.xlsx");
    let styles = STYLES.replace("yyyy-mm-dd", "[h]:mm:ss");
    let sheet = POSITIVE_SHEET.replace(
        r#"<c r="A2"><v>42</v></c>"#,
        r#"<c r="A2" s="1"><v>1.5</v></c>"#,
    );
    write_fixture_parts_with_styles(&path, &sheet, SHARED_STRINGS, &styles);
    let mut options = ReadOptions::default();
    options.spreadsheet.ranges = vec!["Main!A2".to_owned()];

    let ReadArtifact::Workbook(result) = read_artifact(ReadSource::File(path), &options)
        .await
        .unwrap()
    else {
        panic!("expected workbook artifact");
    };

    assert_eq!(result.workbook.selections[0].cells[0].display, "36:00:00");
}

#[test]
fn revision_ignores_zip_recompression_when_part_content_is_unchanged() {
    let part = |compressed_bytes| WorkbookPartRevision {
        name: "xl/worksheets/sheet1.xml".to_owned(),
        crc32: "1234abcd".to_owned(),
        compressed_bytes,
        expanded_bytes: 4096,
    };
    let previous = WorkbookRevision {
        id: "previous".to_owned(),
        compressed_bytes: 100,
        expanded_bytes: 4096,
        parts: vec![part(100)],
    };
    let repacked = WorkbookRevision {
        id: "repacked".to_owned(),
        compressed_bytes: 90,
        expanded_bytes: 4096,
        parts: vec![part(90)],
    };

    let diff = previous.diff(&repacked);
    assert!(diff.unchanged);
    assert!(diff.changed_parts.is_empty());
}

#[test]
fn revision_maps_changed_worksheet_relationships_back_to_their_sheet() {
    let revision = |crc32: &str| WorkbookRevision {
        id: crc32.to_owned(),
        compressed_bytes: 10,
        expanded_bytes: 20,
        parts: vec![WorkbookPartRevision {
            name: "xl/worksheets/_rels/sheet1.xml.rels".to_owned(),
            crc32: crc32.to_owned(),
            compressed_bytes: 10,
            expanded_bytes: 20,
        }],
    };

    let diff = revision("11111111").diff(&revision("22222222"));
    assert_eq!(diff.changed_worksheet_parts, ["xl/worksheets/sheet1.xml"]);
    assert!(!diff.requires_full_refresh);
}

fn large_tail_sheet(first_value: u32) -> String {
    let mut worksheet = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <dimension ref="A1:A20000"/><sheetData>
  <row r="1"><c r="A1"><v>{first_value}</v></c></row>"#,
    );
    use std::fmt::Write as _;
    for row in 2..=20_000 {
        write!(
            worksheet,
            "<row r=\"{row}\"><c r=\"A{row}\" s=\"1\"/></row>"
        )
        .unwrap();
    }
    worksheet.push_str("</sheetData></worksheet>");
    worksheet
}

#[tokio::test]
async fn reads_values_formulas_dates_merges_and_visibility() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("positive.xlsx");
    write_fixture(&path, POSITIVE_SHEET);
    let mut options = ReadOptions::default();
    options.spreadsheet.ranges = vec!["Main!A1:D4".to_owned()];

    let artifact = read_artifact(ReadSource::File(path), &options)
        .await
        .unwrap();
    let ReadArtifact::Workbook(result) = artifact else {
        panic!("expected workbook artifact");
    };

    assert_eq!(result.workbook.sheets.len(), 2);
    assert!(matches!(
        result.workbook.sheets[1].state,
        SheetState::Hidden
    ));
    assert_eq!(
        result.workbook.sheets[0].semantic_bounds.as_deref(),
        Some("A1:D4")
    );
    assert_eq!(result.workbook.sheets[0].hidden_rows, 1);
    assert_eq!(result.workbook.sheets[0].hidden_columns, 1);
    assert_eq!(result.workbook.sheets[0].merged_ranges, ["A4:B4"]);
    let selection = &result.workbook.selections[0];
    assert_eq!(selection.cells.len(), 8);
    assert_eq!(
        selection
            .cells
            .iter()
            .find(|cell| cell.reference == "B1")
            .unwrap()
            .display,
        "A & B"
    );
    let formula = selection
        .cells
        .iter()
        .find(|cell| cell.reference == "C2")
        .unwrap();
    assert_eq!(formula.value, "50");
    assert_eq!(formula.formula.as_deref(), Some("=SUM(A2,8)"));
    assert!(matches!(formula.formula_kind, Some(FormulaKind::Shared)));
    assert_eq!(formula.formula_reference.as_deref(), Some("C2:D2"));
    assert_eq!(formula.shared_formula_index, Some(0));
    let shared_follower = selection
        .cells
        .iter()
        .find(|cell| cell.reference == "D2")
        .unwrap();
    assert_eq!(shared_follower.formula, None);
    assert!(matches!(
        shared_follower.formula_kind,
        Some(FormulaKind::Shared)
    ));
    assert_eq!(shared_follower.shared_formula_index, Some(0));
    assert!(
        selection
            .cells
            .iter()
            .find(|cell| cell.reference == "B1")
            .unwrap()
            .rich_text
    );
    assert_eq!(
        selection
            .cells
            .iter()
            .find(|cell| cell.reference == "A3")
            .unwrap()
            .display,
        "2023-03-15"
    );
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.contains("#REF!"))
    );
    assert!(result.content.contains("| Row | A | B | C |"));
    let features = &result.workbook.sheets[0].features;
    assert_eq!(features.formula_cells, 2);
    assert_eq!(features.hyperlinks.len(), 1);
    assert_eq!(
        features.hyperlinks[0].target.as_deref(),
        Some("https://example.com")
    );
    assert!(features.hyperlinks[0].external);
    assert_eq!(features.auto_filter.as_deref(), Some("A1:D4"));
    assert_eq!(features.conditional_format_rules, 1);
    assert_eq!(features.conditional_format_ranges, ["A2:D2"]);
    assert_eq!(features.data_validation_rules, 1);
    assert_eq!(features.data_validation_ranges, ["A2:A3"]);
    assert_eq!(features.table_parts, 1);
    assert_eq!(features.drawing_parts, 1);
    assert_eq!(features.comment_drawing_parts, 1);
    assert!(features.page_setup);
    assert!(features.header_footer);
    assert_eq!(features.outlined_rows, 1);
    assert_eq!(features.outlined_columns, 1);
    assert_eq!(features.max_row_outline_level, 1);
    assert_eq!(features.max_column_outline_level, 2);
    assert_eq!(features.sparklines, 1);
    assert_eq!(features.controls, 1);
    assert_eq!(result.workbook.features.rich_string_items, 1);
    assert_eq!(result.workbook.features.cell_formats, 2);
    assert_eq!(result.workbook.features.custom_number_formats, 1);
    assert_eq!(result.workbook.features.theme_parts, 1);
    assert_eq!(result.workbook.features.drawing_parts, 1);
    assert_eq!(result.workbook.features.chart_parts, 1);
    assert_eq!(result.workbook.features.image_parts, 1);
    assert_eq!(result.workbook.features.table_parts, 1);
    assert_eq!(result.workbook.features.comment_parts, 1);
    assert_eq!(result.workbook.features.control_property_parts, 1);
}

#[tokio::test]
async fn default_preview_does_not_publish_hidden_sheets_but_inventories_them() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("preview.xlsx");
    write_fixture(&path, POSITIVE_SHEET);

    let artifact = read_artifact(ReadSource::File(path), &ReadOptions::default())
        .await
        .unwrap();
    let ReadArtifact::Workbook(result) = artifact else {
        panic!("expected workbook artifact");
    };

    assert_eq!(result.workbook.selections.len(), 1);
    assert_eq!(result.workbook.selections[0].sheet, "Main");
    assert_eq!(result.workbook.statistics.scanned_sheets, 2);
    assert!(result.workbook.features.inventory_complete);
    assert!(result.workbook.sheets[1].features.scanned);
    assert!(!result.workbook.sheets[1].selected);
}

#[tokio::test]
async fn revision_probe_detects_one_changed_worksheet_without_expanding_it() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("revision.xlsx");
    write_fixture(&path, POSITIVE_SHEET);
    let initial = read_artifact(ReadSource::File(path.clone()), &ReadOptions::default())
        .await
        .unwrap();
    let ReadArtifact::Workbook(initial) = initial else {
        panic!("expected workbook artifact");
    };

    let modified = POSITIVE_SHEET.replacen("<v>42</v>", "<v>43</v>", 1);
    write_fixture(&path, &modified);
    let mut options = ReadOptions::default();
    options.spreadsheet.revision_only = true;
    let current = read_artifact(ReadSource::File(path), &options)
        .await
        .unwrap();
    let ReadArtifact::Workbook(current) = current else {
        panic!("expected workbook artifact");
    };

    assert!(current.workbook.selections.is_empty());
    assert_eq!(current.workbook.statistics.scanned_sheets, 0);
    assert!(!current.workbook.features.inventory_complete);
    assert!(
        (current.workbook.statistics.expanded_bytes_read as u64)
            < current
                .revision
                .parts
                .iter()
                .map(|part| part.expanded_bytes)
                .sum::<u64>()
    );
    let diff = initial.revision.diff(&current.revision);
    assert!(!diff.unchanged);
    assert!(!diff.requires_full_refresh);
    assert_eq!(diff.changed_worksheet_parts, ["xl/worksheets/sheet1.xml"]);
}

#[tokio::test]
async fn markdown_mirror_refresh_preserves_agent_authored_regions() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("mirror.xlsx");
    write_fixture(&path, POSITIVE_SHEET);
    let mut options = ReadOptions::default();
    options.spreadsheet.ranges = vec!["Main!A1:D4".to_owned()];
    let initial = read_artifact(ReadSource::File(path.clone()), &options)
        .await
        .unwrap();
    let ReadArtifact::Workbook(initial) = initial else {
        panic!("expected workbook artifact");
    };
    let existing = format!(
        "{}\n## Agent analysis\n\nThis paragraph must survive XLSX refresh.\n",
        initial.content
    );

    let modified = POSITIVE_SHEET.replacen("<v>42</v>", "<v>43</v>", 1);
    write_fixture(&path, &modified);
    let update = read_artifact(ReadSource::File(path), &options)
        .await
        .unwrap();
    let ReadArtifact::Workbook(update) = update else {
        panic!("expected workbook artifact");
    };
    let merged = merge_markdown_mirror(&existing, &update).unwrap();

    assert!(merged.contains("This paragraph must survive XLSX refresh."));
    assert!(merged.contains("| 2 | 43 | TRUE | 50 | 51 |"));
    assert!(!merged.contains("| 2 | 42 | TRUE | 50 | 51 |"));
    assert!(matches!(
        merge_markdown_mirror("agent notes without markers", &update),
        Err(ReadError::InvalidMarkdownMirror(_))
    ));
}

#[tokio::test]
async fn does_not_allocate_from_a_declared_xfd_dimension() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("adversarial.xlsx");
    write_fixture(&path, ADVERSARIAL_SHEET);
    let mut options = ReadOptions::default();
    options.spreadsheet.ranges = vec!["Main!A1:B2".to_owned()];

    let artifact = read_artifact(ReadSource::File(path), &options)
        .await
        .unwrap();
    let ReadArtifact::Workbook(result) = artifact else {
        panic!("expected workbook artifact");
    };

    assert_eq!(
        result.workbook.sheets[0].declared_dimension.as_deref(),
        Some("A1:XFD1048576")
    );
    assert_eq!(
        result.workbook.sheets[0].semantic_bounds.as_deref(),
        Some("A1:A1")
    );
    assert!(!result.workbook.sheets[0].semantic_bounds_complete);
    assert!(!result.workbook.sheets[0].features.complete);
    assert_eq!(result.workbook.statistics.cell_elements, 1);
    assert_eq!(result.workbook.statistics.non_empty_cells, 1);
    assert_eq!(result.workbook.statistics.style_only_cells, 0);
    assert_eq!(result.workbook.statistics.returned_cells, 1);
}

#[tokio::test]
async fn targeted_streaming_avoids_at_least_eighty_percent_of_expanded_bytes() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("large-tail.xlsx");
    let worksheet = large_tail_sheet(1);
    write_fixture(&path, &worksheet);

    let full = read_artifact(ReadSource::File(path.clone()), &ReadOptions::default())
        .await
        .unwrap();
    let ReadArtifact::Workbook(full) = full else {
        panic!("expected workbook artifact");
    };
    let mut targeted_options = ReadOptions::default();
    targeted_options.spreadsheet.ranges = vec!["Main!A1:A1".to_owned()];
    let targeted = read_artifact(ReadSource::File(path), &targeted_options)
        .await
        .unwrap();
    let ReadArtifact::Workbook(targeted) = targeted else {
        panic!("expected workbook artifact");
    };

    let full_bytes = full.workbook.statistics.expanded_bytes_read as f64;
    let targeted_bytes = targeted.workbook.statistics.expanded_bytes_read as f64;
    let saved = 1.0 - targeted_bytes / full_bytes;
    assert!(saved >= 0.8, "expanded byte saving was {saved:.3}");
    assert!(!targeted.workbook.sheets[0].features.complete);
    assert!(full.workbook.features.inventory_complete);
}

#[test]
fn session_refreshes_human_xlsx_edits_and_preserves_agent_markdown() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("collaboration.xlsx");
    write_fixture(&path, &large_tail_sheet(1));
    let mut session = WorkbookSession::open(path.clone(), ReadOptions::default()).unwrap();
    let cold_bytes = session.result().workbook.statistics.expanded_bytes_read;
    let existing = format!(
        "{}\n## Agent analysis\n\nKeep this analysis across human edits.\n",
        session.result().content
    );

    write_fixture(&path, &large_tail_sheet(2));
    let refresh = session.refresh().unwrap();
    let mirror = merge_markdown_mirror(&existing, refresh.result).unwrap();
    let changed = refresh.result.workbook.selections[0]
        .cells
        .iter()
        .find(|cell| cell.reference == "A1")
        .unwrap();

    assert!(refresh.metrics.changed);
    assert!(!refresh.metrics.full_refresh);
    assert_eq!(refresh.diff.changed_parts, ["xl/worksheets/sheet1.xml"]);
    assert!(refresh.metrics.expanded_bytes_read * 5 <= cold_bytes);
    assert_eq!(changed.value, "2");
    assert!(mirror.contains("Keep this analysis across human edits."));
    assert!(mirror.contains("| 1 | 2 |"));
    assert!(!mirror.contains("| 1 | 1 |"));
    let unchanged = session.refresh().unwrap();
    assert!(!unchanged.metrics.changed);
    assert_eq!(unchanged.metrics.expanded_bytes_read, 0);
}

#[test]
fn incremental_refresh_counts_unchanged_cached_cells_against_the_global_limit() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("bounded-refresh.xlsx");
    write_fixture(&path, POSITIVE_SHEET);
    let mut options = ReadOptions::default();
    options.spreadsheet.ranges = vec!["Main!A1:D4".to_owned(), "'Hidden Data'!A1".to_owned()];
    options.spreadsheet.max_cells = 9;
    let mut session = WorkbookSession::open(path.clone(), options).unwrap();
    assert_eq!(
        session
            .result()
            .workbook
            .selections
            .iter()
            .map(|selection| selection.cells.len())
            .sum::<usize>(),
        9
    );

    let changed_sheet = POSITIVE_SHEET.replace(
        r#"<row r="3" hidden="1" outlineLevel="1"><c r="A3" s="1"><v>45000</v></c></row>"#,
        r#"<row r="3" hidden="1" outlineLevel="1"><c r="A3" s="1"><v>45000</v></c><c r="B3"><v>7</v></c></row>"#,
    );
    write_fixture(&path, &changed_sheet);
    let refresh = session.refresh().unwrap();
    let returned = refresh
        .result
        .workbook
        .selections
        .iter()
        .map(|selection| selection.cells.len())
        .sum::<usize>();

    assert!(!refresh.metrics.full_refresh);
    assert_eq!(returned, 9);
    assert_eq!(refresh.result.workbook.statistics.returned_cells, 9);
    assert!(refresh.result.workbook.selections[0].truncated);
    assert_eq!(refresh.result.workbook.selections[1].cells.len(), 1);
}

#[test]
fn session_fully_refreshes_shared_string_changes_then_reuses_the_new_context() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("shared-string-change.xlsx");
    write_fixture(&path, POSITIVE_SHEET);
    let mut session = WorkbookSession::open(path.clone(), ReadOptions::default()).unwrap();
    let existing = format!(
        "{}\n\n## Agent analysis\n\nshared-string-note\n",
        session.result().content
    );

    let changed_strings = SHARED_STRINGS.replace("Name", "Title");
    write_fixture_parts(&path, POSITIVE_SHEET, &changed_strings);
    let refresh = session.refresh().unwrap();
    assert!(refresh.metrics.full_refresh);
    assert!(refresh.diff.requires_full_refresh);
    assert!(
        refresh
            .diff
            .changed_parts
            .iter()
            .any(|part| part == "xl/sharedStrings.xml")
    );
    assert_eq!(
        refresh.result.workbook.selections[0].cells[0].display,
        "Title"
    );
    let mirror = merge_markdown_mirror(&existing, refresh.result).unwrap();
    assert!(mirror.contains("shared-string-note"));

    let changed_sheet = POSITIVE_SHEET.replace("<v>42</v>", "<v>43</v>");
    write_fixture_parts(&path, &changed_sheet, &changed_strings);
    let partial = session.refresh().unwrap();
    assert!(!partial.metrics.full_refresh);
    let cells = &partial.result.workbook.selections[0].cells;
    assert_eq!(
        cells
            .iter()
            .find(|cell| cell.reference == "A1")
            .unwrap()
            .display,
        "Title"
    );
    assert_eq!(
        cells
            .iter()
            .find(|cell| cell.reference == "A2")
            .unwrap()
            .value,
        "43"
    );
}

#[tokio::test]
async fn rejects_invalid_ranges_and_expanded_size_overruns() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("limits.xlsx");
    write_fixture(&path, POSITIVE_SHEET);

    let mut invalid = ReadOptions::default();
    invalid.spreadsheet.ranges = vec!["Main!XFE1:XFE2".to_owned()];
    assert!(matches!(
        read_artifact(ReadSource::File(path.clone()), &invalid).await,
        Err(ReadError::InvalidSpreadsheetRange { .. })
    ));

    let mut bounded = ReadOptions::default();
    bounded.spreadsheet.max_expanded_bytes = 64;
    assert!(matches!(
        read_artifact(ReadSource::File(path), &bounded).await,
        Err(ReadError::SpreadsheetExpandedTooLarge { limit: 64 })
    ));
}
