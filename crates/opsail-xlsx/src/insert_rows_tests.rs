use super::*;
use crate::references::Shift;

impl Fixture {
    fn parts(mut self, parts: Vec<(&str, String)>) -> Self {
        for (path, text) in parts {
            self.package.parts.insert(path.into(), text.into_bytes());
        }
        let mut zip = ZipWriter::new(fs::File::create(&self.source).unwrap());
        for (name, bytes) in &self.package.parts {
            zip.start_file(name, SimpleFileOptions::default()).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
        self.package = Package::read(&self.source, Limits::new(None, None).unwrap()).unwrap();
        self
    }
    fn second(self, body: &str, definitions: &str) -> Self {
        let rels = String::from_utf8(self.package.parts["xl/_rels/workbook.xml.rels"].clone()).unwrap().replace("</Relationships>",&format!(r#"<Relationship Id="second" Type="{REL}/worksheet" Target="worksheets/sheet2.xml"/></Relationships>"#));
        self.parts(vec![
            ("xl/workbook.xml",format!(r#"<workbook xmlns="{MAIN}" xmlns:r="{REL}"><sheets><sheet name="UseCase" sheetId="4" r:id="r1"/><sheet name="Report Image" sheetId="9" r:id="second"/></sheets>{definitions}</workbook>"#)),
            ("xl/_rels/workbook.xml.rels",rels),
            ("xl/worksheets/sheet2.xml",format!(r#"<worksheet xmlns="{MAIN}">{body}</worksheet>"#)),
        ])
    }
}
fn insert(before: u32, count: u32) -> Value {
    json!({"op":"insertRows","sheet":"UseCase","before":before,"count":count})
}
fn output(book: &Book, f: &Fixture, part: &str) -> String {
    String::from_utf8(
        book.updates(&f.package)
            .unwrap()
            .get(part)
            .unwrap_or(&f.package.parts[part])
            .clone(),
    )
    .unwrap()
}
fn refusal(f: &Fixture, before: u32, fragment: &str) {
    let mut request = f.request(json!([insert(before, 2)]));
    request["validateOnly"] = json!(true);
    let report = execute(request).unwrap();
    let v = &report["violations"][0];
    assert_eq!(v["operationIndex"], 0, "{report}");
    assert_eq!(v["op"], "insertRows");
    assert_eq!(v["sheet"], "UseCase");
    assert_eq!(v["target"], before.to_string());
    assert!(
        v["message"].as_str().unwrap().contains(fragment),
        "{report}"
    );
    assert!(
        v["message"]
            .as_str()
            .unwrap()
            .contains("requires native application"),
        "{report}"
    );
    assert_eq!(report["wouldChangeParts"], json!([]));
    let err = execute(f.request(json!([insert(before, 2)])))
        .unwrap_err()
        .details();
    assert_eq!(&err, v);
    assert!(!f.source.with_file_name("candidate.xlsx").exists());
}
#[test]
fn insert_rows_renumbers_cells_rebuilds_indexes_and_preserves_raw_above() {
    let raw = "<row ht='19' customHeight='1' r='2'>\r\n <c t='inlineStr' r='B2'><is><t>keep &#65; &amp;</t></is></c>\r\n</row>";
    let f = Fixture::new(&format!(
        r#"<dimension ref="B2:D8"/><sheetData> {raw} <row r="4" spans="2:4"><c r="B4" t="inlineStr"><is><t>moved</t></is></c><c r="D4" s="1"/></row> <row r="8"/></sheetData>"#
    ));
    let mut b = f.book();
    apply(&mut b, insert(4, 2));
    let s = b.sheet("UseCase").unwrap();
    assert_eq!(s.rows.keys().copied().collect::<Vec<_>>(), [2, 4, 5, 6, 10]);
    assert_eq!(s.row(6).unwrap().unwrap().attrs["spans"], "2:4");
    assert_eq!(cell(&b, "B6").child("is").unwrap().text(), "moved");
    assert!(s.cell("B4").unwrap().is_none());
    assert!(s.cell("D6").unwrap().is_some());
    assert_eq!(
        s.root().unwrap().child("dimension").unwrap().attrs["ref"],
        "B2:D10"
    );
    apply(
        &mut b,
        json!({"op":"setText","sheet":"UseCase","cell":"B6","expectedText":"moved","value":"after"}),
    );
    assert!(output(&b, &f, "xl/worksheets/sheet1.xml").contains(raw));
    let reloaded = Sheet::new(
        "test".into(),
        xml::parse(&output(&b, &f, "xl/worksheets/sheet1.xml")).unwrap(),
    )
    .unwrap();
    assert_eq!(reloaded.rows, b.sheet("UseCase").unwrap().rows);
    assert_eq!(reloaded.cells, b.sheet("UseCase").unwrap().cells);
}
#[test]
fn insert_rows_copies_only_nondefault_cell_styles_and_visible_row_attributes() {
    let f = Fixture::new(
        r#"<dimension ref="A2:E2"/><sheetData><row r="2" hidden="1" s="1" customFormat="1" ht="22" customHeight="1" outlineLevel="2" thickTop="1" thickBot="1" spans="1:5"><c r="A2" s="0"/><c r="B2"/><c r="C2" s="1" t="inlineStr"><is><t>not copied</t></is></c><c r="E2" s="2"><f>1</f><v>1</v></c></row></sheetData>"#,
    );
    let mut b = f.book();
    apply(&mut b, insert(3, 2));
    for r in [3, 4] {
        let row = b.sheet("UseCase").unwrap().row(r).unwrap().unwrap();
        for (a, v) in [
            ("s", "1"),
            ("customFormat", "1"),
            ("ht", "22"),
            ("customHeight", "1"),
            ("outlineLevel", "2"),
            ("thickTop", "1"),
            ("thickBot", "1"),
            ("spans", "1:5"),
        ] {
            assert_eq!(row.attrs[a], v);
        }
        assert!(!row.attrs.contains_key("hidden"));
        assert_eq!(row.elements().count(), 2);
        for c in row.elements() {
            assert_eq!(c.attrs.len(), 2);
            assert!(c.children.is_empty());
        }
        assert_eq!(cell(&b, &format!("E{r}")).attrs["s"], "2");
    }
    assert_eq!(
        b.sheet("UseCase")
            .unwrap()
            .root()
            .unwrap()
            .child("dimension")
            .unwrap()
            .attrs["ref"],
        "A2:E4"
    );
}
#[test]
fn insert_rows_none_missing_above_and_first_row_are_plain() {
    for (before, style) in [(1, "above"), (3, "above"), (2, "none")] {
        let f = Fixture::new(r#"<sheetData><row r="1" s="1"><c r="A1" s="1"/></row></sheetData>"#);
        let mut b = f.book();
        let mut op = insert(before, 1);
        op["styleFrom"] = json!(style);
        apply(&mut b, op);
        let row = b.sheet("UseCase").unwrap().row(before).unwrap().unwrap();
        assert_eq!(row.attrs.len(), 1);
        assert!(row.children.is_empty());
    }
}
#[test]
fn insert_rows_merges_shift_grow_and_keep_anchor_valid() {
    let f = Fixture::new(
        r#"<sheetData><row r="6"><c r="A6"/></row></sheetData><mergeCells count="3"><mergeCell ref="A1:B2"/><mergeCell ref="C3:D6"/><mergeCell ref="A6:B7"/></mergeCells>"#,
    );
    let mut b = f.book();
    apply(&mut b, insert(5, 2));
    let s = b.sheet("UseCase").unwrap();
    assert_eq!(
        s.merged.iter().map(|a| a.reference()).collect::<Vec<_>>(),
        ["A1:B2", "C3:D8", "A8:B9"]
    );
    apply(
        &mut b,
        json!({"op":"setText","sheet":"UseCase","cell":"A8","expectedText":"","value":"anchor"}),
    );
    assert!(b.apply_bounded(&operation(json!({"op":"setText","sheet":"UseCase","cell":"B8","expectedText":"","value":"bad"})),10000,true).is_err());
}
#[test]
fn a1_tokenizer_preserves_every_nonreference_byte() {
    let names = vec!["UseCase".into(), "Report Image".into(), "It's".into()];
    let shift = Shift {
        sheet: "UseCase",
        sheets: &names,
        before: 5,
        count: 2,
    };
    let cases = [
        (
            "Other!A3 : $B$7 + Other!$3 : $7 + A3 : A7",
            "Other!A3 : $B$7 + Other!$3 : $7 + A3 : A9",
        ),
        (
            " SUM(A4:$B$7, 3:5, $3:$5, $A:$XFD) ",
            " SUM(A4:$B$9, 3:7, $3:$7, $A:$XFD) ",
        ),
        (
            "UseCase!C7+'UseCase'!$C$7+UseCase!U7+Other!A7",
            "UseCase!C9+'UseCase'!$C$9+UseCase!U9+Other!A7",
        ),
        (
            "IF(A5=\"A5\",INDIRECT(\"UseCase!A5\"),OFFSET(A5,1,\"A5\"))",
            "IF(A7=\"A5\",INDIRECT(\"UseCase!A5\"),OFFSET(A7,1,\"A5\"))",
        ),
        (
            "\"a\"\"A5\"&a5+LOG10 (A5)+MyName5+A5_name+R1C5+1E5",
            "\"a\"\"A5\"&a7+LOG10 (A7)+MyName5+A5_name+R1C5+1E5",
        ),
        (
            "Table1[[A5]:[B9]]+[1]UseCase!A5+'[a.xlsx]UseCase'!A5+Other!3:7",
            "Table1[[A5]:[B9]]+[1]UseCase!A5+'[a.xlsx]UseCase'!A5+Other!3:7",
        ),
        (
            "'Report Image'!$A$1:$AY$142 + 'It''s'!A5",
            "'Report Image'!$A$1:$AY$142 + 'It''s'!A5",
        ),
    ];
    for (input, expected) in cases {
        assert_eq!(
            shift.formula(input, true).unwrap().text,
            expected,
            "{input}"
        );
    }
    assert_eq!(
        shift.formula("A7+UseCase!C7+Other!A7", false).unwrap().text,
        "A7+UseCase!C9+Other!A7"
    );
    for target in ["Report Image", "It's"] {
        let shift = Shift {
            sheet: target,
            sheets: &names,
            before: 5,
            count: 2,
        };
        let (input, expected) = if target == "It's" {
            ("'It''s'!$A$5", "'It''s'!$A$7")
        } else {
            ("'Report Image'!$A$1:$AY$142", "'Report Image'!$A$1:$AY$144")
        };
        assert_eq!(shift.formula(input, false).unwrap().text, expected);
    }
}
#[test]
fn a1_tokenizer_3d_includes_intermediate_sheet_and_ignores_external() {
    let names = vec![
        "First".into(),
        "UseCase".into(),
        "Last".into(),
        "Other".into(),
    ];
    let shift = Shift {
        sheet: "UseCase",
        sheets: &names,
        before: 5,
        count: 2,
    };
    for formula in [
        "First:Last!A1",
        "'First:Last'!$A:$C",
        "'First':'Last'!3:5",
        "Last:First!A1",
    ] {
        assert!(
            shift
                .formula(formula, false)
                .unwrap_err()
                .to_string()
                .contains("3D")
        );
    }
    for formula in ["Last:Other!A7", "'[1]First:Last'!A7", "\"First:Last!A7\""] {
        assert_eq!(shift.formula(formula, true).unwrap().text, formula);
    }
}
#[test]
fn insert_rows_shifts_same_and_cross_sheet_formulas_and_keeps_unrelated_shared() {
    let f=Fixture::new(r#"<sheetData><row r="1"><c r="A1"><f>SUM(A3:A7)+Other!A7</f></c></row><row r="7"><c r="A7"><f>A7+$B$7</f></c></row></sheetData>"#)
        .second(r#"<sheetData><row r="7"><c r="A7"><f>UseCase!C7+'UseCase'!$C$7+UseCase!U7+A7</f></c><c r="B7"><f t="shared" si="0" ref="B7:B8">A7+1</f></c></row><row r="8"><c r="B8"><f t="shared" si="0"/></c></row></sheetData>"#,"");
    let mut b = f.book();
    apply(&mut b, insert(5, 2));
    assert_eq!(
        cell(&b, "A1").child("f").unwrap().text(),
        "SUM(A3:A9)+Other!A7"
    );
    assert_eq!(cell(&b, "A9").child("f").unwrap().text(), "A9+$B$9");
    assert_eq!(
        b.sheet("Report Image")
            .unwrap()
            .cell("A7")
            .unwrap()
            .unwrap()
            .child("f")
            .unwrap()
            .text(),
        "UseCase!C9+'UseCase'!$C$9+UseCase!U9+A7"
    );
    assert!(output(&b, &f, "xl/worksheets/sheet1.xml").contains("SUM(A3:A9)"));
    assert!(output(&b, &f, "xl/worksheets/sheet2.xml").contains("UseCase!C9"));
}
#[test]
fn insert_rows_updates_defined_names_including_print_areas_titles_and_local_names() {
    let definitions = r#"<definedNames><definedName name="_xlnm.Print_Area" localSheetId="0">UseCase!$A$1:$AY$142</definedName><definedName name="_xlnm.Print_Titles" localSheetId="0">'UseCase'!$3:$5,UseCase!$A:$C</definedName><definedName name="Custom">SUM(UseCase!A5,'Report Image'!A7)</definedName><definedName name="Local" localSheetId="0">A5</definedName><definedName name="_xlnm.Print_Area" localSheetId="1">'Report Image'!$A$1:$AY$142</definedName></definedNames>"#;
    let f = Fixture::new("<sheetData/>").second("<sheetData/>", definitions);
    let mut b = f.book();
    apply(&mut b, insert(5, 2));
    let texts: Vec<_> = b
        .workbook
        .root()
        .unwrap()
        .child("definedNames")
        .unwrap()
        .elements()
        .map(Element::text)
        .collect();
    assert_eq!(
        texts,
        [
            "UseCase!$A$1:$AY$144",
            "'UseCase'!$3:$7,UseCase!$A:$C",
            "SUM(UseCase!A7,'Report Image'!A7)",
            "A7",
            "'Report Image'!$A$1:$AY$142"
        ]
    );
}
#[test]
fn insert_rows_cf_dv_filters_hyperlinks_protection_views_and_breaks() {
    let f = Fixture::new(
        r#"<sheetViews><sheetView workbookViewId="0"><pane topLeftCell="C5"/><selection activeCell="C5" sqref="C5:D7 A1"/></sheetView></sheetViews><sheetData/><protectedRanges><protectedRange name="test" sqref="A3:A7"/></protectedRanges><autoFilter ref="A3:B7"><sortState ref="A5:B7"><sortCondition ref="B5:B7"/></sortState></autoFilter><conditionalFormatting sqref="A3:A7 C5"><cfRule type="expression"><formula>A5&gt;UseCase!B7</formula></cfRule></conditionalFormatting><dataValidations><dataValidation sqref="C5:C7"><formula1>SUM(A3:A7)</formula1><formula2>Other!A7</formula2></dataValidation></dataValidations><hyperlinks><hyperlink ref="A5"/></hyperlinks><rowBreaks count="3"><brk id="3"/><brk id="4"/><brk id="6"/></rowBreaks><colBreaks><brk id="4"/></colBreaks>"#,
    );
    let mut b = f.book();
    apply(&mut b, insert(5, 2));
    let root = b.sheet("UseCase").unwrap().root().unwrap();
    let view = root
        .child("sheetViews")
        .unwrap()
        .child("sheetView")
        .unwrap();
    assert_eq!(view.child("pane").unwrap().attrs["topLeftCell"], "C7");
    assert_eq!(view.child("selection").unwrap().attrs["activeCell"], "C7");
    assert_eq!(view.child("selection").unwrap().attrs["sqref"], "C7:D9 A1");
    assert_eq!(
        root.child("protectedRanges")
            .unwrap()
            .child("protectedRange")
            .unwrap()
            .attrs["sqref"],
        "A3:A9"
    );
    let filter = root.child("autoFilter").unwrap();
    assert_eq!(filter.attrs["ref"], "A3:B9");
    assert_eq!(filter.child("sortState").unwrap().attrs["ref"], "A7:B9");
    assert_eq!(
        filter
            .child("sortState")
            .unwrap()
            .child("sortCondition")
            .unwrap()
            .attrs["ref"],
        "B7:B9"
    );
    let cf = root.child("conditionalFormatting").unwrap();
    assert_eq!(cf.attrs["sqref"], "A3:A9 C7");
    assert_eq!(
        cf.child("cfRule").unwrap().child("formula").unwrap().text(),
        "A7>UseCase!B9"
    );
    let dv = root
        .child("dataValidations")
        .unwrap()
        .child("dataValidation")
        .unwrap();
    assert_eq!(dv.attrs["sqref"], "C7:C9");
    assert_eq!(dv.child("formula1").unwrap().text(), "SUM(A3:A9)");
    assert_eq!(dv.child("formula2").unwrap().text(), "Other!A7");
    assert_eq!(
        root.child("hyperlinks")
            .unwrap()
            .child("hyperlink")
            .unwrap()
            .attrs["ref"],
        "A7"
    );
    assert_eq!(
        root.child("rowBreaks")
            .unwrap()
            .elements()
            .map(|e| e.attrs["id"].as_str())
            .collect::<Vec<_>>(),
        ["3", "6", "8"]
    );
    assert_eq!(
        root.child("colBreaks").unwrap().child("brk").unwrap().attrs["id"],
        "4"
    );
}
fn related_fixture(kind: &str, path: &'static str, content: String) -> Fixture {
    Fixture::new("<sheetData/>").parts(vec![
        ("xl/worksheets/_rels/sheet1.xml.rels",format!(r#"<Relationships><Relationship Id="object" Type="{REL}/{kind}" Target="/{path}"/></Relationships>"#)),(path,content),
    ])
}
#[test]
fn insert_rows_drawing_two_cell_one_cell_spanning_absolute_and_edit_as() {
    let ns = "http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing";
    let f = related_fixture(
        "drawing",
        "xl/drawings/drawing1.xml",
        format!(
            r#"<xdr:wsDr xmlns:xdr="{ns}"><xdr:twoCellAnchor editAs="oneCell"><xdr:from><xdr:row>0</xdr:row></xdr:from><xdr:to><xdr:row>3</xdr:row></xdr:to></xdr:twoCellAnchor><xdr:twoCellAnchor><xdr:from><xdr:row>4</xdr:row></xdr:from><xdr:to><xdr:row>7</xdr:row></xdr:to></xdr:twoCellAnchor><xdr:twoCellAnchor><xdr:from><xdr:row>2</xdr:row></xdr:from><xdr:to><xdr:row>6</xdr:row></xdr:to></xdr:twoCellAnchor><xdr:oneCellAnchor><xdr:from><xdr:row>4</xdr:row></xdr:from><xdr:ext cy="900"/></xdr:oneCellAnchor><xdr:absoluteAnchor><xdr:pos x="1" y="900"/><xdr:ext cx="3" cy="4"/></xdr:absoluteAnchor></xdr:wsDr>"#
        ),
    );
    let mut b = f.book();
    apply(&mut b, insert(5, 2));
    let doc = xml::parse(&output(&b, &f, "xl/drawings/drawing1.xml")).unwrap();
    let e: Vec<_> = doc.root().unwrap().elements().collect();
    for (index, from, to) in [(0, "0", "3"), (1, "6", "9"), (2, "2", "8")] {
        assert_eq!(
            e[index].child("from").unwrap().child("row").unwrap().text(),
            from
        );
        assert_eq!(
            e[index].child("to").unwrap().child("row").unwrap().text(),
            to
        );
    }
    assert_eq!(e[0].attrs["editAs"], "oneCell");
    assert_eq!(
        e[3].child("from").unwrap().child("row").unwrap().text(),
        "6"
    );
    assert_eq!(e[3].child("ext").unwrap().attrs["cy"], "900");
    let original = f.package.xml("xl/drawings/drawing1.xml").unwrap();
    assert_eq!(e[4], original.root().unwrap().elements().nth(4).unwrap());
}
#[test]
fn insert_rows_legacy_vml_without_comments_moves_rows_and_retains_offsets() {
    let f=related_fixture("vmlDrawing","xl/drawings/vmlDrawing1.vml",r#"<xml xmlns:x="urn:schemas-microsoft-com:office:excel"><x:ClientData><x:Anchor>1, 2, 4, 8, 3, 4, 7, 9</x:Anchor><x:Row>4</x:Row></x:ClientData><x:ClientData><x:Anchor>1, 2, 0, 8, 3, 4, 6, 9</x:Anchor><x:Row>0</x:Row></x:ClientData></xml>"#.into());
    let mut b = f.book();
    apply(&mut b, insert(5, 2));
    let xml = output(&b, &f, "xl/drawings/vmlDrawing1.vml");
    assert!(xml.contains("1, 2, 6, 8, 3, 4, 9, 9"));
    assert!(xml.contains("1, 2, 0, 8, 3, 4, 8, 9"));
    assert!(xml.contains("<x:Row>6</x:Row>"));
    assert!(xml.contains("<x:Row>0</x:Row>"));
}
#[test]
fn insert_rows_comments_and_threaded_comments() {
    for (kind,path,content) in [
        ("comments","xl/comments1.xml",format!(r#"<comments xmlns="{MAIN}"><commentList><comment ref="A5"/><comment ref="B2"/></commentList></comments>"#)),
        ("threadedComment","xl/threadedComments/threadedComment1.xml",r#"<ThreadedComments xmlns="http://schemas.microsoft.com/office/spreadsheetml/2018/threadedcomments"><threadedComment ref="A5"/><threadedComment ref="B2"/></ThreadedComments>"#.into())] {
        let f=related_fixture(kind,path,content);let mut b=f.book();apply(&mut b,insert(5,2));let text=output(&b,&f,path);assert!(text.contains("ref=\"A7\""));assert!(text.contains("ref=\"B2\""));
    }
}
#[test]
fn insert_rows_calc_chain_inherits_one_based_sheet_index_not_sheet_id() {
    let f=Fixture::new("<sheetData/>").second("<sheetData/>","").parts(vec![("xl/calcChain.xml",format!(r#"<calcChain xmlns="{MAIN}"><c r="A5" i="1"/><c r="B7"/><c r="C7" i="2"/><c r="D7"/><c r="E3" i="1"/><c r="F5"/></calcChain>"#))]);
    let mut b = f.book();
    apply(&mut b, insert(5, 2));
    let doc = xml::parse(&output(&b, &f, "xl/calcChain.xml")).unwrap();
    let refs: Vec<_> = doc
        .root()
        .unwrap()
        .elements()
        .map(|e| e.attrs["r"].as_str())
        .collect();
    assert_eq!(refs, ["A7", "B9", "C7", "D7", "E3", "F7"]);
}
#[test]
fn insert_rows_chart_series_formulas() {
    let f=Fixture::new("<sheetData/>").parts(vec![("xl/charts/chart1.xml",r#"<c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart"><c:ser><c:numRef><c:f>'UseCase'!$A$3:$A$7</c:f></c:numRef><c:strRef><c:f>Other!A5:A7</c:f></c:strRef></c:ser></c:chartSpace>"#.into())]);
    let mut b = f.book();
    apply(&mut b, insert(5, 2));
    let text = output(&b, &f, "xl/charts/chart1.xml");
    assert!(text.contains("'UseCase'!$A$3:$A$9"));
    assert!(text.contains("Other!A5:A7"));
}
#[test]
fn insert_rows_refuses_special_formula_cells_and_spanning_refs() {
    for kind in ["shared", "array", "dataTable"] {
        for (cell, range) in [("A5", "A5:A6"), ("A2", "A2:A6")] {
            let r = address(cell).unwrap().1;
            let f = Fixture::new(&format!(
                r#"<sheetData><row r="{r}"><c r="{cell}"><f t="{kind}" ref="{range}">1</f></c></row></sheetData>"#
            ));
            refusal(&f, 5, kind);
        }
    }
}
#[test]
fn insert_rows_refuses_shared_references_to_target_even_above_insertion() {
    for source in ["UseCase!C1", "'UseCase'!$C$1", "UseCase!$A:$C"] {
        let f=Fixture::new("<sheetData/>").second(&format!(r#"<sheetData><row r="1"><c r="A1"><f t="shared" si="0" ref="A1:A2">{source}</f></c></row><row r="2"><c r="A2"><f t="shared" si="0"/></c></row></sheetData>"#),"");
        refusal(&f, 5, "shared formula references target");
    }
}
#[test]
fn insert_rows_refuses_tables_only_when_range_touches_insertion() {
    for range in ["A1:B6", "A5:B7", "A1:B3"] {
        let f=related_fixture("table","xl/tables/table1.xml",format!(r#"<table xmlns="{MAIN}" ref="{range}"/>"#)).parts(vec![("xl/worksheets/sheet1.xml",format!(r#"<worksheet xmlns="{MAIN}" xmlns:r="{REL}"><sheetData/><tableParts><tablePart r:id="object"/></tableParts></worksheet>"#))]);
        if range == "A1:B3" {
            let mut b = f.book();
            apply(&mut b, insert(5, 2));
            assert!(
                !b.updates(&f.package)
                    .unwrap()
                    .contains_key("xl/tables/table1.xml")
            );
        } else {
            refusal(&f, 5, "table range");
        }
    }
}
#[test]
fn insert_rows_refuses_pivot_sources_including_defined_names() {
    for source in [
        r#"sheet="UseCase" ref="A1:B7""#,
        r#"sheet="UseCase" ref="A5:B7""#,
        r#"name="PivotSource""#,
    ] {
        let f=Fixture::new("<sheetData/>").second("<sheetData/>","<definedNames><definedName name=\"PivotSource\">UseCase!A1:B7</definedName></definedNames>")
            .parts(vec![("xl/pivotCache/pivotCacheDefinition1.xml",format!(r#"<pivotCacheDefinition xmlns="{MAIN}"><cacheSource type="worksheet"><worksheetSource {source}/></cacheSource></pivotCacheDefinition>"#))]);
        refusal(&f, 5, "pivot source");
    }
}
#[test]
fn insert_rows_refuses_3d_in_formulas_names_and_charts() {
    for site in ["formula", "name", "chart"] {
        let f=Fixture::new(if site=="formula" {"<sheetData><row r=\"1\"><c r=\"A1\"><f>SUM(UseCase:'Report Image'!A1)</f></c></row></sheetData>"} else {"<sheetData/>"})
            .second("<sheetData/>",if site=="name" {"<definedNames><definedName name=\"x\">'UseCase:Report Image'!A1</definedName></definedNames>"} else {""});
        let f = if site == "chart" {
            f.parts(vec![("xl/charts/chart1.xml","<c:chartSpace xmlns:c=\"http://schemas.openxmlformats.org/drawingml/2006/chart\"><c:f>UseCase:'Report Image'!A1</c:f></c:chartSpace>".into())])
        } else {
            f
        };
        refusal(&f, 5, "3D reference");
    }
}
#[test]
fn insert_rows_refuses_row_reference_and_anchor_overflow() {
    for body in [
        r#"<sheetData><row r="1048576"/></sheetData>"#,
        r#"<sheetData><row r="1"><c r="A1"><f>A1048576</f></c></row></sheetData>"#,
        r#"<dimension ref="A1:A1048576"/><sheetData/>"#,
    ] {
        refusal(&Fixture::new(body), 5, "1048576");
    }
    let f=related_fixture("drawing","xl/drawings/drawing1.xml",r#"<x:wsDr xmlns:x="http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing"><x:oneCellAnchor><x:from><x:row>1048575</x:row></x:from></x:oneCellAnchor></x:wsDr>"#.into());
    refusal(&f, 5, "1048576");
    refusal(&Fixture::new("<sheetData/>"), 1048576, "1048576");
}
#[test]
fn insert_rows_refuses_protected_sheet_and_preserves_signed_refusal() {
    refusal(
        &Fixture::new("<sheetData/><sheetProtection sheet=\"1\"/>"),
        5,
        "protected",
    );
    let f = Fixture::new("<sheetData/>")
        .parts(vec![("_xmlsignatures/sig1.xml", "<Signature/>".into())]);
    let err = execute(f.request(json!([insert(5, 2)]))).unwrap_err();
    assert!(err.to_string().contains("digitally signed"));
}
#[test]
fn insert_rows_validate_only_rolls_back_all_parts_and_keeps_operation_order() {
    let f=Fixture::new(r#"<sheetData><row r="7"><c r="A7" t="inlineStr"><is><t>old</t></is></c></row></sheetData>"#).parts(vec![("xl/charts/chart1.xml",r#"<c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart"><c:f>UseCase!A1048576</c:f></c:chartSpace>"#.into())]);
    let mut request=f.request(json!([insert(5,2),{"op":"setText","sheet":"UseCase","cell":"A7","expectedText":"old","value":"after"}]));
    request["validateOnly"] = json!(true);
    let report = execute(request).unwrap();
    assert_eq!(report["violations"].as_array().unwrap().len(), 1);
    assert_eq!(report["targetsProcessed"], 1);
    assert_eq!(
        report["wouldChangeParts"],
        json!(["xl/worksheets/sheet1.xml"])
    );
    assert!(!f.source.with_file_name("candidate.xlsx").exists());
}
#[test]
fn insert_rows_then_set_text_style_copy_style_and_second_insert_publish_and_reload() {
    let f = Fixture::new(
        r#"<dimension ref="A2:C5"/><sheetData><row r="2"><c r="A2" s="1"/></row><row r="5"><c r="C5" t="inlineStr"><is><t>end</t></is></c></row></sheetData>"#,
    );
    let ops = json!([insert(3,2),{"op":"setText","sheet":"UseCase","cell":"B3","expectedText":"","value":"new"},{"op":"setStyle","sheet":"UseCase","range":"B3","style":{"bold":true}},{"op":"copyStyle","sheet":"UseCase","range":"C4","fromCell":"A3","components":ALL_COMPONENTS},insert(4,1),{"op":"setText","sheet":"UseCase","cell":"C8","expectedText":"end","value":"moved"}]);
    let report = execute(f.request(ops)).unwrap();
    assert_eq!(
        report["rowsInserted"],
        json!([{"sheet":"UseCase","before":3,"count":2},{"sheet":"UseCase","before":4,"count":1}])
    );
    assert_eq!(report["targetsProcessed"], 7);
    let pkg = Package::read(
        &f.source.with_file_name("candidate.xlsx"),
        Limits::new(None, None).unwrap(),
    )
    .unwrap();
    let b = Book::load(&pkg).unwrap();
    assert_eq!(cell(&b, "B3").child("is").unwrap().text(), "new");
    assert_eq!(cell(&b, "C5").attrs["s"], "1");
    assert_eq!(cell(&b, "C8").child("is").unwrap().text(), "moved");
    assert!(
        b.styles
            .root()
            .unwrap()
            .child("cellXfs")
            .unwrap()
            .elements()
            .count()
            > 3
    );
}
#[test]
fn insert_rows_budget_counts_rows_and_limits_created_style_cells() {
    let f = Fixture::new(
        r#"<sheetData><row r="1"><c r="A1" s="1"/></row><row r="5"><c r="A5"/></row></sheetData>"#,
    );
    let mut b = f.book();
    assert!(b.apply_bounded(&operation(insert(2, 3)), 2, true).is_err());
    assert_eq!(
        b.apply_bounded(&operation(insert(2, 3)), 3, true).unwrap(),
        3
    );
    assert_eq!(b.sheet("UseCase").unwrap().cells.len(), 5);
    let cells = (1..=401)
        .map(|c| format!(r#"<c r="{}1" s="1"/>"#, column_name(c)))
        .collect::<String>();
    let f = Fixture::new(&format!(
        "<sheetData><row r=\"1\">{cells}</row></sheetData>"
    ));
    let mut b = f.book();
    let err = b
        .apply_bounded(&operation(insert(2, 500)), 10000, true)
        .unwrap_err();
    assert!(err.to_string().contains("200000"));
    assert!(b.updates(&f.package).unwrap().is_empty());
}
#[test]
fn insert_rows_input_bounds_and_default_are_strict() {
    let f = Fixture::new("<sheetData/>");
    for (before, count) in [(0, 1), (1048577, 1), (1, 0), (1, 501)] {
        assert!(execute(f.request(json!([insert(before, count)]))).is_err());
    }
    for style in ["below", "", "Above"] {
        let mut op = insert(1, 1);
        op["styleFrom"] = json!(style);
        assert!(execute(f.request(json!([op]))).is_err());
    }
}

#[test]
fn insert_rows_renamed_chart_and_pivot_parts_use_content_types() {
    let types = r#"<Types><Override PartName="/custom/chart.xml" ContentType="application/vnd.openxmlformats-officedocument.drawingml.chart+xml"/><Override PartName="/custom/pivot.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.pivotCacheDefinition+xml"/></Types>"#;
    let f=Fixture::new("<sheetData/>").parts(vec![("[Content_Types].xml",types.into()),
        ("custom/chart.xml",r#"<c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart"><c:f>UseCase!A7</c:f></c:chartSpace>"#.into()),
        ("custom/pivot.xml",format!(r#"<pivotCacheDefinition xmlns="{MAIN}"><cacheSource><worksheetSource sheet="UseCase" ref="A1:B3"/></cacheSource></pivotCacheDefinition>"#))]);
    let mut b = f.book();
    apply(&mut b, insert(5, 2));
    assert!(output(&b, &f, "custom/chart.xml").contains("UseCase!A9"));
    let f=f.parts(vec![("custom/pivot.xml",format!(r#"<pivotCacheDefinition xmlns="{MAIN}"><cacheSource><worksheetSource sheet="UseCase" ref="A1:B7"/></cacheSource></pivotCacheDefinition>"#))]);
    refusal(&f, 5, "pivot source");
}
#[test]
fn insert_rows_named_dynamic_pivot_sources_fail_closed() {
    for text in ["OFFSET(UseCase!A1,0,0,100,3)", "OtherName", "A1:B3"] {
        let f=Fixture::new("<sheetData/>").second("<sheetData/>",&format!(r#"<definedNames><definedName name="Source">{text}</definedName></definedNames>"#))
            .parts(vec![("xl/pivotCache/pivotCacheDefinition1.xml",format!(r#"<pivotCacheDefinition xmlns="{MAIN}"><cacheSource><worksheetSource name="Source"/></cacheSource></pivotCacheDefinition>"#))]);
        refusal(&f, 5, "pivot source");
    }
}
#[test]
fn insert_rows_special_formulas_wholly_above_can_remain() {
    for kind in ["shared", "array", "dataTable"] {
        let f = Fixture::new(&format!(
            r#"<sheetData><row r="1"><c r="A1"><f t="{kind}" ref="A1:A2">1</f></c></row></sheetData>"#
        ));
        let mut b = f.book();
        apply(&mut b, insert(5, 2));
        assert_eq!(cell(&b, "A1").child("f").unwrap().text(), "1");
    }
}
#[test]
fn insert_rows_missing_or_malformed_objects_refuse_without_partial_changes() {
    let missing = Fixture::new(&format!(
        r#"<sheetData/><drawing xmlns:r="{REL}" r:id="missing"/>"#
    ));
    refusal(&missing, 5, "drawing relationship missing");
    for xml in [
        "<broken>",
        r#"<xml xmlns:x="urn:schemas-microsoft-com:office:excel"><x:Anchor>1,2</x:Anchor></xml>"#,
        r#"<xml xmlns:x="urn:schemas-microsoft-com:office:excel"><x:Row>bad</x:Row></xml>"#,
    ] {
        let f = related_fixture("vmlDrawing", "xl/drawings/vmlDrawing1.vml", xml.into());
        refusal(&f, 5, "xl/drawings/vmlDrawing1.vml");
    }
}
#[test]
fn insert_rows_repeated_insertions_reuse_staged_names_chain_and_drawings() {
    let f=related_fixture("drawing","xl/drawings/drawing1.xml",r#"<x:wsDr xmlns:x="http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing"><x:oneCellAnchor><x:from><x:row>5</x:row></x:from></x:oneCellAnchor></x:wsDr>"#.into())
        .second("<sheetData/>",r#"<definedNames><definedName name="x">UseCase!A6</definedName></definedNames>"#)
        .parts(vec![("xl/calcChain.xml",format!(r#"<calcChain xmlns="{MAIN}"><c i="1" r="A6"/></calcChain>"#))]);
    let mut b = f.book();
    apply(&mut b, insert(5, 2));
    apply(&mut b, insert(6, 3));
    assert_eq!(
        b.workbook
            .root()
            .unwrap()
            .child("definedNames")
            .unwrap()
            .child("definedName")
            .unwrap()
            .text(),
        "UseCase!A11"
    );
    assert!(output(&b, &f, "xl/calcChain.xml").contains("r=\"A11\""));
    assert!(output(&b, &f, "xl/drawings/drawing1.xml").contains("<x:row>10</x:row>"));
    apply(
        &mut b,
        json!({"op":"setFormula","sheet":"UseCase","cell":"A1","expectedText":"","formula":"1"}),
    );
    assert_eq!(
        b.workbook.root().unwrap().child("calcPr").unwrap().attrs["fullCalcOnLoad"],
        "1"
    );
}
#[test]
fn insert_rows_calc_chain_target_is_second_sheet() {
    let f=Fixture::new("<sheetData/>").second("<sheetData/>","").parts(vec![("xl/calcChain.xml",format!(r#"<calcChain xmlns="{MAIN}"><c i="1" r="A5"/><c i="2" r="A5"/><c r="B5"/></calcChain>"#))]);
    let mut b = f.book();
    let mut op = insert(5, 2);
    op["sheet"] = json!("Report Image");
    apply(&mut b, op);
    let doc = xml::parse(&output(&b, &f, "xl/calcChain.xml")).unwrap();
    assert_eq!(
        doc.root()
            .unwrap()
            .elements()
            .map(|e| e.attrs["r"].as_str())
            .collect::<Vec<_>>(),
        ["A5", "A7", "B7"]
    );
}
#[test]
fn insert_rows_workbook_style_cell_budget_and_moves_at_full_capacity() {
    let mut rows = String::from("<sheetData>");
    for r in 1..=20 {
        rows.push_str(&format!(r#"<row r="{r}">"#));
        for c in 1..=10000 {
            if (r, c) != (20, 10000) {
                rows.push_str(&format!(r#"<c r="{}{r}"/>"#, column_name(c)));
            }
        }
        rows.push_str("</row>");
    }
    rows.push_str("</sheetData>");
    let f = Fixture::new(r#"<sheetData><row r="1"><c r="A1" s="1"/></row></sheetData>"#)
        .second(&rows, "");
    let mut b = f.book();
    let err = b
        .apply_bounded(&operation(insert(2, 1)), 10000, true)
        .unwrap_err();
    assert!(err.to_string().contains("200000"));
    assert!(b.updates(&f.package).unwrap().is_empty());
    let mut op = insert(1, 1);
    op["sheet"] = json!("Report Image");
    op["styleFrom"] = json!("none");
    assert_eq!(apply(&mut b, op), 1);
    assert!(
        b.sheet("Report Image")
            .unwrap()
            .cell("A2")
            .unwrap()
            .is_some()
    );
}
