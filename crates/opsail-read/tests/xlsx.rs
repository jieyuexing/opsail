use std::io::Write as _;
use std::path::Path;

use opsail_read::{
    CellValueType, FormulaKind, ReadArtifact, ReadError, ReadOptions, ReadSource, SheetState,
    WorkbookMergeRole, WorkbookPartRevision, WorkbookRevision, WorkbookSession,
    merge_markdown_mirror, read_artifact,
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
    <definedName name="_xlnm.Print_Area" localSheetId="0">'Main'!$A$1:$Q$20</definedName>
    <definedName name="_xlnm.Print_Titles" localSheetId="0">'Main'!$1:$2</definedName>
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
  <printOptions gridLines="0" headings="1"/>
  <pageSetup orientation="landscape" paperSize="9" fitToPage="1" fitToWidth="1" fitToHeight="0" scale="75"/>
  <headerFooter/>
  <rowBreaks count="1" manualBreakCount="1"><brk id="3" min="0" max="16383" man="1"/></rowBreaks>
  <colBreaks count="1" manualBreakCount="1"><brk id="2" min="0" max="1048575" man="1"/></colBreaks>
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

const TINY_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x04, 0x00, 0x00, 0x00, 0xb5, 0x1c, 0x0c,
    0x02, 0x00, 0x00, 0x00, 0x0b, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x64, 0xf8, 0x0f, 0x00,
    0x01, 0x05, 0x01, 0x01, 0x27, 0x18, 0xe3, 0x66, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44,
    0xae, 0x42, 0x60, 0x82,
];

const TINY_PNG_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";

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
    write_fixture_parts_with_styles_and_theme(
        path,
        first_sheet,
        shared_strings,
        styles,
        "<theme/>",
    );
}

fn write_fixture_parts_with_styles_and_theme(
    path: &Path,
    first_sheet: &str,
    shared_strings: &str,
    styles: &str,
    theme: &str,
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
        ("xl/theme/theme1.xml", theme),
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

fn write_picture_fixture(path: &Path, media: &[u8]) {
    let content_types = CONTENT_TYPES.replace(
        r#"<Default Extension="xml" ContentType="application/xml"/>"#,
        r#"<Default Extension="xml" ContentType="application/xml"/>
  <Default Extension="png" ContentType="image/png"/>"#,
    );
    let sheet = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
      xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
      <dimension ref="A1:D10"/><sheetData/><drawing r:id="rIdDrawing"/>
    </worksheet>"#;
    let drawing = r#"<xdr:wsDr xmlns:xdr="http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing"
      xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
      xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
      <xdr:twoCellAnchor>
        <xdr:from><xdr:col>0</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>0</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from>
        <xdr:to><xdr:col>3</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>9</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:to>
        <xdr:pic><xdr:nvPicPr><xdr:cNvPr id="2" name="Picture 1"/><xdr:cNvPicPr/></xdr:nvPicPr>
          <xdr:blipFill><a:blip r:embed="rIdImage"/><a:stretch><a:fillRect/></a:stretch></xdr:blipFill>
          <xdr:spPr><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></xdr:spPr>
        </xdr:pic><xdr:clientData/>
      </xdr:twoCellAnchor>
    </xdr:wsDr>"#;
    let drawing_relationships = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
      <Relationship Id="rIdImage" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.png"/>
    </Relationships>"#;
    let file = std::fs::File::create(path).unwrap();
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, content) in [
        ("[Content_Types].xml", content_types.as_bytes()),
        ("xl/workbook.xml", WORKBOOK.as_bytes()),
        ("xl/_rels/workbook.xml.rels", RELATIONSHIPS.as_bytes()),
        ("xl/styles.xml", STYLES.as_bytes()),
        ("xl/sharedStrings.xml", SHARED_STRINGS.as_bytes()),
        ("xl/worksheets/sheet1.xml", sheet.as_bytes()),
        (
            "xl/worksheets/_rels/sheet1.xml.rels",
            SHEET_RELATIONSHIPS.as_bytes(),
        ),
        ("xl/worksheets/sheet2.xml", HIDDEN_SHEET.as_bytes()),
        ("xl/drawings/drawing1.xml", drawing.as_bytes()),
        (
            "xl/drawings/_rels/drawing1.xml.rels",
            drawing_relationships.as_bytes(),
        ),
        ("xl/media/image1.png", media),
    ] {
        zip.start_file(name, options).unwrap();
        zip.write_all(content).unwrap();
    }
    zip.finish().unwrap();
}

#[tokio::test]
async fn returns_bounded_pixels_only_for_intersecting_worksheet_pictures() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("picture.xlsx");
    write_picture_fixture(&path, TINY_PNG);
    let mut options = ReadOptions::default();
    options.spreadsheet.ranges = vec!["Main!A1:D10".to_owned(), "Main!E11:F12".to_owned()];

    let ReadArtifact::Workbook(result) = read_artifact(ReadSource::File(path), &options)
        .await
        .unwrap()
    else {
        panic!("expected workbook artifact");
    };

    let inventory = &result.workbook.sheets[0].pictures;
    assert_eq!(inventory.len(), 1);
    assert_eq!(inventory[0].from_cell, "A1");
    assert_eq!(inventory[0].to_cell.as_deref(), Some("D10"));
    assert_eq!(inventory[0].media_part, "xl/media/image1.png");
    assert_eq!(inventory[0].content_type, "image/png");
    assert_eq!(inventory[0].byte_size, TINY_PNG.len());
    assert_eq!(
        inventory[0].sha256,
        "431ced6916a2a21a156e38701afe55bbd7f88969fbbfc56d7fe099d47f265460"
    );
    assert!(inventory[0].data_uri.is_none());

    let hit = &result.workbook.selections[0];
    assert_eq!(hit.images.len(), 1);
    assert_eq!(hit.images[0].from_row_index, 0);
    assert_eq!(hit.images[0].from_column_index, 0);
    assert_eq!(hit.images[0].to_row_index, Some(9));
    assert_eq!(hit.images[0].to_column_index, Some(3));
    assert_eq!(
        hit.images[0].data_uri.as_deref(),
        Some(format!("data:image/png;base64,{TINY_PNG_BASE64}").as_str())
    );
    assert!(!hit.images[0].payload_truncated);
    assert!(!hit.images_truncated);

    let miss = &result.workbook.selections[1];
    assert!(miss.images.is_empty());
    assert!(!miss.images_truncated);
    assert!(
        result
            .content
            .contains("Intersecting worksheet pictures: 1")
    );
    assert!(result.content.contains("dataUri in JSON"));
    assert!(result.content.contains("does not OCR"));
}

#[tokio::test]
async fn oversized_worksheet_picture_is_metadata_only() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("oversized-picture.xlsx");
    let media = vec![0_u8; 2 * 1024 * 1024 + 1];
    write_picture_fixture(&path, &media);
    let mut options = ReadOptions::default();
    options.spreadsheet.ranges = vec!["Main!A1:D10".to_owned()];

    let ReadArtifact::Workbook(result) = read_artifact(ReadSource::File(path), &options)
        .await
        .unwrap()
    else {
        panic!("expected workbook artifact");
    };

    let selection = &result.workbook.selections[0];
    assert_eq!(selection.images.len(), 1);
    assert!(selection.images[0].data_uri.is_none());
    assert!(selection.images[0].payload_truncated);
    assert!(selection.images_truncated);
    assert!(
        result
            .warnings
            .iter()
            .any(|warning| warning.contains("2097152 bytes per image"))
    );
    assert!(result.content.contains("metadata only (limit)"));
}

#[tokio::test]
async fn publishes_cell_and_rich_run_strike_and_color_semantics() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("text-formatting.xlsx");
    let styles = r#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
      <fonts count="3"><font/><font><strike/><color theme="4" tint="0.5"/></font><font><color indexed="10"/></font></fonts>
      <fills count="1"><fill/></fills><borders count="1"><border/></borders>
      <cellStyleXfs count="1"><xf numFmtId="0"/></cellStyleXfs>
      <cellXfs count="3"><xf numFmtId="0" fontId="0"/><xf numFmtId="0" fontId="1"/><xf numFmtId="0" fontId="2"/></cellXfs>
    </styleSheet>"#;
    let shared = r#"<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="1" uniqueCount="1">
      <si><r><rPr><strike/><color rgb="FF123456"/></rPr><t>deleted</t></r><r><rPr><color indexed="10" tint="-0.25"/></rPr><t> kept</t></r></si>
    </sst>"#;
    let sheet = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1:C1"/><sheetData><row r="1">
      <c r="A1" t="s" s="1"><v>0</v></c>
      <c r="B1" t="inlineStr" s="2"><is><r><rPr><strike val="0"/><color theme="5"/></rPr><t>inline</t></r></is></c>
      <c r="C1" t="inlineStr"><is><t>plain</t></is></c>
    </row></sheetData></worksheet>"#;
    let theme = r#"<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:themeElements><a:clrScheme name="x">
      <a:dk1><a:srgbClr val="000000"/></a:dk1><a:lt1><a:srgbClr val="FFFFFF"/></a:lt1>
      <a:dk2><a:srgbClr val="111111"/></a:dk2><a:lt2><a:srgbClr val="EEEEEE"/></a:lt2>
      <a:accent1><a:srgbClr val="336699"/></a:accent1><a:accent2><a:srgbClr val="CC3300"/></a:accent2>
    </a:clrScheme></a:themeElements></a:theme>"#;
    write_fixture_parts_with_styles_and_theme(&path, sheet, shared, styles, theme);
    let mut options = ReadOptions::default();
    options.spreadsheet.ranges = vec!["Main!A1:C1".to_owned()];
    let ReadArtifact::Workbook(result) = read_artifact(ReadSource::File(path), &options)
        .await
        .unwrap()
    else {
        panic!()
    };
    let cells = &result.workbook.selections[0].cells;
    assert_eq!(cells[0].font_strike, Some(true));
    assert_eq!(cells[0].font_color.as_ref().unwrap().theme, Some(4));
    assert_eq!(
        cells[0]
            .font_color
            .as_ref()
            .unwrap()
            .resolved_rgb
            .as_deref(),
        Some("8CB3D9")
    );
    assert_eq!(cells[0].display, "deleted kept");
    assert!(cells[0].rich_text);
    assert_eq!(cells[0].rich_text_runs[0].strike, Some(true));
    assert_eq!(
        cells[0].rich_text_runs[0]
            .font_color
            .as_ref()
            .unwrap()
            .resolved_rgb
            .as_deref(),
        Some("123456")
    );
    assert_eq!(cells[1].font_color.as_ref().unwrap().indexed, Some(10));
    assert_eq!(
        cells[1]
            .font_color
            .as_ref()
            .unwrap()
            .resolved_rgb
            .as_deref(),
        Some("FF0000")
    );
    assert_eq!(cells[1].rich_text_runs[0].strike, Some(false));
    assert_eq!(
        cells[1].rich_text_runs[0]
            .font_color
            .as_ref()
            .unwrap()
            .theme,
        Some(5)
    );
    assert!(
        cells[2].font_strike.is_none()
            && cells[2].font_color.is_none()
            && cells[2].rich_text_runs.is_empty()
    );
    assert!(result.content.contains("### Text formatting"));
    assert!(result.content.contains("run 1"));
}

#[tokio::test]
async fn resolves_spreadsheet_theme_zero_one_and_respects_apply_font() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("theme-fonts.xlsx");
    let styles = r#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
      <fonts count="6"><font/><font><color theme="0"/></font><font><color theme="1"/></font>
        <font><strike/><color rgb="FF4F81BD"/></font><font><color theme="8"/></font>
        <font><color auto="1" theme="1"/></font></fonts>
      <fills count="1"><fill/></fills><borders count="1"><border/></borders>
      <cellStyleXfs count="1"><xf numFmtId="0"/></cellStyleXfs>
      <cellXfs count="7"><xf numFmtId="0"/><xf numFmtId="0" fontId="1" applyFont="1"/>
        <xf numFmtId="0" fontId="2" applyFont="1"/><xf numFmtId="0" fontId="3" applyFont="0"/>
        <xf numFmtId="0" fontId="3" applyFont="1"/><xf numFmtId="0" fontId="4" applyFont="1"/>
        <xf numFmtId="0" fontId="5" applyFont="1"/></cellXfs>
    </styleSheet>"#;
    let sheet = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1:F1"/><sheetData><row r="1">
      <c r="A1" t="inlineStr" s="1"><is><t>light</t></is></c>
      <c r="B1" t="inlineStr" s="2"><is><t>dark</t></is></c>
      <c r="C1" t="inlineStr" s="3"><is><t>not-applied</t></is></c>
      <c r="D1" t="inlineStr" s="4"><is><t>direct</t></is></c>
      <c r="E1" t="inlineStr" s="5"><is><t>accent</t></is></c>
      <c r="F1" t="inlineStr" s="6"><is><t>automatic</t></is></c>
    </row></sheetData></worksheet>"#;
    let theme = r#"<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:themeElements><a:clrScheme name="Office">
      <a:dk1><a:sysClr val="windowText" lastClr="000000"/></a:dk1>
      <a:lt1><a:sysClr val="window" lastClr="FFFFFF"/></a:lt1>
      <a:dk2><a:srgbClr val="1F497D"/></a:dk2><a:lt2><a:srgbClr val="EEECE1"/></a:lt2>
      <a:accent1><a:srgbClr val="4F81BD"/></a:accent1><a:accent2><a:srgbClr val="C0504D"/></a:accent2>
      <a:accent3><a:srgbClr val="9BBB59"/></a:accent3><a:accent4><a:srgbClr val="8064A2"/></a:accent4>
      <a:accent5><a:srgbClr val="4BACC6"/></a:accent5><a:accent6><a:srgbClr val="F79646"/></a:accent6>
      <a:hlink><a:srgbClr val="0000FF"/></a:hlink><a:folHlink><a:srgbClr val="800080"/></a:folHlink>
    </a:clrScheme></a:themeElements></a:theme>"#;
    write_fixture_parts_with_styles_and_theme(&path, sheet, SHARED_STRINGS, styles, theme);
    let mut options = ReadOptions::default();
    options.spreadsheet.ranges = vec!["Main!A1:F1".to_owned()];
    let ReadArtifact::Workbook(result) = read_artifact(ReadSource::File(path), &options)
        .await
        .unwrap()
    else {
        panic!()
    };
    let color = |reference: &str| {
        result.workbook.selections[0]
            .cells
            .iter()
            .find(|cell| cell.reference == reference)
            .unwrap()
            .font_color
            .as_ref()
            .unwrap()
            .resolved_rgb
            .as_deref()
            .unwrap()
    };
    assert_eq!(color("A1"), "FFFFFF");
    assert_eq!(color("B1"), "000000");
    let not_applied = result.workbook.selections[0]
        .cells
        .iter()
        .find(|cell| cell.reference == "C1")
        .unwrap();
    assert!(not_applied.font_strike.is_none());
    assert!(not_applied.font_color.is_none());
    assert_eq!(color("D1"), "4F81BD");
    assert_eq!(color("E1"), "4BACC6");
    let automatic = result.workbook.selections[0]
        .cells
        .iter()
        .find(|cell| cell.reference == "F1")
        .unwrap()
        .font_color
        .as_ref()
        .unwrap();
    assert_eq!(automatic.auto, Some(true));
    assert!(automatic.resolved_rgb.is_none());
}

#[tokio::test]
async fn publishes_merge_membership_for_horizontal_vertical_and_clipped_merges() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("merge-membership.xlsx");
    let sheet = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
      <dimension ref="A1:Q5"/><sheetData>
        <row r="1"><c r="A1" t="inlineStr"><is><t>horizontal</t></is></c></row>
        <row r="2"><c r="D2" t="inlineStr"><is><t>vertical</t></is></c></row>
        <row r="5"><c r="L5" t="inlineStr"><is><t>wide</t></is></c></row>
      </sheetData><mergeCells count="3">
        <mergeCell ref="A1:C1"/><mergeCell ref="D2:D4"/><mergeCell ref="L5:Q5"/>
      </mergeCells>
    </worksheet>"#;
    write_fixture(&path, sheet);
    let mut options = ReadOptions::default();
    options.spreadsheet.ranges = vec!["Main!B1:M5".to_owned()];
    let ReadArtifact::Workbook(result) = read_artifact(ReadSource::File(path), &options)
        .await
        .unwrap()
    else {
        panic!()
    };
    let selection = &result.workbook.selections[0];
    assert_eq!(selection.merged_ranges, ["A1:C1", "D2:D4", "L5:Q5"]);
    let cell = |reference: &str| {
        selection
            .cells
            .iter()
            .find(|cell| cell.reference == reference)
            .unwrap()
    };
    assert_eq!(cell("B1").merge.as_ref().unwrap().anchor, "A1");
    assert!(matches!(
        cell("B1").merge.as_ref().unwrap().role,
        WorkbookMergeRole::Covered
    ));
    assert!(matches!(
        cell("D2").merge.as_ref().unwrap().role,
        WorkbookMergeRole::Anchor
    ));
    assert_eq!(cell("D4").merge.as_ref().unwrap().anchor, "D2");
    assert!(matches!(
        cell("M5").merge.as_ref().unwrap().role,
        WorkbookMergeRole::Covered
    ));
    assert_eq!(cell("M5").merge.as_ref().unwrap().range, "L5:Q5");
    assert!(selection.cells.iter().all(|cell| cell.reference != "N5"));
    assert!(result.content.contains("Intersecting merges"));
    assert!(result.content.contains("`L5:Q5`"));
}

#[tokio::test]
async fn scans_selected_rows_to_their_used_columns_and_still_reads_the_worksheet_tail() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("tail-and-overflow.xlsx");
    let sheet = r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
      <dimension ref="A1:Z10"/><sheetData>
        <row r="1"><c r="A1"><v>1</v></c><c r="B1"><v>2</v></c><c r="Q1"><v>17</v></c></row>
        <row r="10"><c r="Z10"><v>26</v></c></row>
      </sheetData>
      <mergeCells count="1"><mergeCell ref="Y10:Z10"/></mergeCells>
      <autoFilter ref="A1:Q1"/>
      <printOptions gridLines="1" headings="0"/>
      <pageSetup orientation="landscape" paperSize="9" fitToPage="1" fitToWidth="1" fitToHeight="0" scale="80"/>
      <headerFooter/>
      <rowBreaks count="1"><brk id="8"/></rowBreaks>
      <colBreaks count="1"><brk id="16"/></colBreaks>
    </worksheet>"#;
    write_fixture(&path, sheet);
    let mut options = ReadOptions::default();
    options.spreadsheet.ranges = vec!["Main!B1:M1".to_owned()];
    let ReadArtifact::Workbook(result) = read_artifact(ReadSource::File(path), &options)
        .await
        .unwrap()
    else {
        panic!()
    };
    let sheet = &result.workbook.sheets[0];
    assert!(!sheet.features.cell_data_complete);
    assert!(sheet.features.tail_features_complete);
    assert!(!sheet.features.complete);
    assert_eq!(sheet.semantic_bounds.as_deref(), Some("A1:Q1"));
    assert_eq!(sheet.merged_ranges, ["Y10:Z10"]);
    assert_eq!(sheet.features.auto_filter.as_deref(), Some("A1:Q1"));
    assert_eq!(sheet.print.page_setup.as_ref().unwrap().scale, Some(80));
    assert_eq!(
        sheet.print.print_options.as_ref().unwrap().grid_lines,
        Some(true)
    );
    assert_eq!(sheet.print.row_breaks, [8]);
    assert_eq!(sheet.print.column_breaks, [16]);
    assert!(sheet.print.header_footer);
    let selection = &result.workbook.selections[0];
    assert_eq!(selection.used_bounds.as_deref(), Some("A1:Q1"));
    let left = selection.overflow.left.as_ref().unwrap();
    assert_eq!(
        (left.min_column, left.max_column, left.cell_count),
        (1, 1, 1)
    );
    let right = selection.overflow.right.as_ref().unwrap();
    assert_eq!(
        (right.min_column, right.max_column, right.cell_count),
        (17, 17, 1)
    );
    assert!(selection.overflow.below.is_none());
    assert!(
        result
            .content
            .contains("Column overflow right: Q through Q")
    );
    assert!(result.content.contains("'Main'!$A$1:$Q$20"));
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

#[test]
fn revision_treats_drawing_and_media_changes_as_dependency_wide() {
    let revision = |crc32: &str| WorkbookRevision {
        id: crc32.to_owned(),
        compressed_bytes: 10,
        expanded_bytes: 20,
        parts: vec![WorkbookPartRevision {
            name: "xl/media/image1.png".to_owned(),
            crc32: crc32.to_owned(),
            compressed_bytes: 10,
            expanded_bytes: 20,
        }],
    };

    let diff = revision("11111111").diff(&revision("22222222"));
    assert!(diff.requires_full_refresh);
    assert!(diff.changed_worksheet_parts.is_empty());
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
    assert!(result.workbook.sheets[0].features.cell_data_complete);
    assert!(result.workbook.sheets[0].features.tail_features_complete);
    let print = &result.workbook.sheets[0].print;
    assert_eq!(print.print_area.as_deref(), Some("'Main'!$A$1:$Q$20"));
    assert_eq!(print.print_titles.as_deref(), Some("'Main'!$1:$2"));
    assert_eq!(print.page_setup.as_ref().unwrap().fit_to_width, Some(1));
    assert_eq!(print.print_options.as_ref().unwrap().headings, Some(true));
    assert_eq!(print.row_breaks, [3]);
    assert_eq!(print.column_breaks, [2]);
    let selection = &result.workbook.selections[0];
    assert_eq!(selection.merged_ranges, ["A4:B4"]);
    assert_eq!(selection.cells.len(), 9);
    let covered = selection
        .cells
        .iter()
        .find(|cell| cell.reference == "B4")
        .unwrap();
    assert!(matches!(covered.value_type, CellValueType::Blank));
    assert!(matches!(
        covered.merge.as_ref().unwrap().role,
        WorkbookMergeRole::Covered
    ));
    assert_eq!(covered.merge.as_ref().unwrap().anchor, "A4");
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
async fn targeted_streaming_skips_later_cell_bodies_but_reaches_the_tail() {
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

    assert_eq!(full.workbook.statistics.cell_elements, 20_001);
    assert_eq!(targeted.workbook.statistics.cell_elements, 1);
    assert!(!targeted.workbook.sheets[0].features.cell_data_complete);
    assert!(targeted.workbook.sheets[0].features.tail_features_complete);
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
    assert!(refresh.metrics.expanded_bytes_read <= cold_bytes);
    assert_eq!(refresh.result.workbook.statistics.cell_elements, 24);
    assert!(
        refresh.result.workbook.sheets[0]
            .features
            .tail_features_complete
    );
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
    options.spreadsheet.max_cells = 10;
    let mut session = WorkbookSession::open(path.clone(), options).unwrap();
    assert_eq!(
        session
            .result()
            .workbook
            .selections
            .iter()
            .map(|selection| selection.cells.len())
            .sum::<usize>(),
        10
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
    assert_eq!(returned, 10);
    assert_eq!(refresh.result.workbook.statistics.returned_cells, 10);
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
