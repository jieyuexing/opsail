use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek};
use std::path::{Path, PathBuf};
use std::time::Instant;
use std::time::SystemTime;

use quick_xml::Reader;
use quick_xml::events::{BytesRef, BytesStart, Event};
use ring::digest::{SHA256, digest};
use serde::Serialize;
use serde_json::{Value, json};
use url::Url;
use zip::ZipArchive;
use zip::result::ZipError;

use crate::error::ReadError;
use crate::model::{ReadOptions, SourceInfo, SourceKind, SpreadsheetReadOptions};

const XLSX_CONTENT_TYPE: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
const PREVIEW_GRID_MAX_AREA: u64 = 2_000;
const MAX_CELL_TEXT_BYTES: usize = 32_767 * 4;
const MAX_FEATURE_REFERENCES: usize = 10_000;
const MAX_IMAGE_REFERENCES: usize = 256;
/// Raw image bytes are capped below the 16 MiB Host response ceiling. Base64
/// expands the total to at most about 5.4 MiB, leaving room for cells and the
/// duplicated Markdown/HTML evidence.
const MAX_IMAGE_PAYLOAD_BYTES: usize = 2 * 1024 * 1024;
const MAX_TOTAL_IMAGE_PAYLOAD_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArtifactKind {
    Workbook,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbookReadResult {
    pub schema_version: u8,
    pub artifact_kind: ArtifactKind,
    pub content: String,
    pub content_html: String,
    pub metadata: WorkbookMetadata,
    pub source: SourceInfo,
    pub extraction: WorkbookExtractionInfo,
    pub revision: WorkbookRevision,
    pub workbook: WorkbookInfo,
    pub warnings: Vec<String>,
}

impl WorkbookReadResult {
    pub fn property(&self, name: &str) -> Option<Value> {
        let value = match name {
            "content" | "markdown" => json!(self.content),
            "contentHtml" | "html" => json!(self.content_html),
            "title" => json!(self.metadata.title),
            "source" => json!(self.source),
            "extraction" => json!(self.extraction),
            "revision" => json!(self.revision),
            "workbook" => json!(self.workbook),
            "sheets" => json!(self.workbook.sheets),
            "selections" => json!(self.workbook.selections),
            "definedNames" => json!(self.workbook.defined_names),
            "features" => json!(self.workbook.features),
            "statistics" => json!(self.workbook.statistics),
            "metrics" => json!({
                "extraction": self.extraction,
                "revisionId": self.revision.id,
                "statistics": self.workbook.statistics,
                "sheets": self.workbook.sheets.iter().map(|sheet| json!({
                    "name": sheet.name,
                    "state": sheet.state,
                    "part": sheet.part,
                })).collect::<Vec<_>>(),
            }),
            _ => return None,
        };
        Some(value)
    }

    /// Retain at most `max_cells` published cells in stable selection order.
    ///
    /// This is used by stdout adapters that must fit a serialized workbook
    /// result into a bounded transport. Worksheet scanning remains governed by
    /// `SpreadsheetReadOptions::max_cells`; this method only trims the already
    /// selected result and regenerates its Markdown and HTML mirrors.
    pub fn truncate_published_cells(&mut self, max_cells: usize) -> usize {
        let mut remaining = max_cells;
        for selection in &mut self.workbook.selections {
            if selection.cells.len() > remaining {
                selection.cells.truncate(remaining);
                selection.truncated = true;
            }
            remaining = remaining.saturating_sub(selection.cells.len());
        }
        let retained = self
            .workbook
            .selections
            .iter()
            .map(|selection| selection.cells.len())
            .sum();
        self.workbook.statistics.returned_cells = retained;
        self.content = render_markdown(&self.metadata.title, &self.workbook);
        self.content_html = render_html(&self.metadata.title, &self.workbook);
        retained
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbookMetadata {
    pub title: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbookExtractionInfo {
    pub method: WorkbookExtractionMethod,
    pub duration_ms: u64,
    pub duration_micros: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkbookExtractionMethod {
    OoxmlSparse,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbookInfo {
    pub format: &'static str,
    pub date_system: DateSystem,
    pub sheets: Vec<WorkbookSheet>,
    pub defined_names: Vec<DefinedName>,
    pub selections: Vec<WorkbookSelection>,
    pub features: WorkbookFeatureInventory,
    pub statistics: WorkbookStatistics,
}

/// Workbook-level feature inventory. Package-part counts are complete without
/// expanding binary media. Worksheet-level inventory is complete only when
/// `inventory_complete` is true.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbookFeatureInventory {
    pub inventory_complete: bool,
    pub cell_formats: usize,
    pub custom_number_formats: usize,
    pub rich_string_items: usize,
    pub theme_parts: usize,
    pub drawing_parts: usize,
    pub chart_parts: usize,
    pub image_parts: usize,
    pub table_parts: usize,
    pub comment_parts: usize,
    pub control_property_parts: usize,
    pub macro_project_parts: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbookRevision {
    pub id: String,
    pub compressed_bytes: u64,
    pub expanded_bytes: u64,
    pub parts: Vec<WorkbookPartRevision>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbookPartRevision {
    pub name: String,
    pub crc32: String,
    pub compressed_bytes: u64,
    pub expanded_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbookRevisionDiff {
    pub unchanged: bool,
    pub requires_full_refresh: bool,
    pub changed_parts: Vec<String>,
    pub changed_worksheet_parts: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbookSessionMetrics {
    pub changed: bool,
    pub full_refresh: bool,
    pub probe_duration_micros: u64,
    pub refresh_duration_micros: u64,
    pub expanded_bytes_read: usize,
}

#[derive(Debug)]
pub struct WorkbookSessionRefresh<'a> {
    pub result: &'a WorkbookReadResult,
    pub diff: WorkbookRevisionDiff,
    pub metrics: WorkbookSessionMetrics,
}

#[derive(Debug)]
pub struct WorkbookSession {
    path: PathBuf,
    options: ReadOptions,
    result: WorkbookReadResult,
    context: WorkbookSessionContext,
    stamp: WorkbookFileStamp,
}

#[derive(Debug)]
struct WorkbookSessionContext {
    shared_strings: Vec<SharedString>,
    styles: Styles,
    content_types: PackageContentTypes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WorkbookFileStamp {
    bytes: u64,
    modified: Option<SystemTime>,
}

impl WorkbookRevision {
    #[must_use]
    pub fn diff(&self, current: &Self) -> WorkbookRevisionDiff {
        let previous: BTreeMap<&str, &WorkbookPartRevision> = self
            .parts
            .iter()
            .map(|part| (part.name.as_str(), part))
            .collect();
        let current_parts: BTreeMap<&str, &WorkbookPartRevision> = current
            .parts
            .iter()
            .map(|part| (part.name.as_str(), part))
            .collect();
        let mut names: Vec<&str> = previous
            .keys()
            .chain(current_parts.keys())
            .copied()
            .collect();
        names.sort_unstable();
        names.dedup();
        let changed_parts: Vec<String> = names
            .into_iter()
            .filter(|name| match (previous.get(name), current_parts.get(name)) {
                (Some(previous), Some(current)) => !same_part_content(previous, current),
                (None, None) => false,
                _ => true,
            })
            .map(str::to_owned)
            .collect();
        let changed_worksheet_parts = changed_parts
            .iter()
            .filter_map(|name| worksheet_part_for_change(name))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let requires_full_refresh = changed_parts.iter().any(|name| {
            matches!(
                name.as_str(),
                "[Content_Types].xml"
                    | "xl/workbook.xml"
                    | "xl/_rels/workbook.xml.rels"
                    | "xl/sharedStrings.xml"
                    | "xl/styles.xml"
            ) || name.starts_with("xl/theme/")
                || name.starts_with("xl/drawings/")
                || name.starts_with("xl/media/")
        });
        WorkbookRevisionDiff {
            unchanged: changed_parts.is_empty(),
            requires_full_refresh,
            changed_parts,
            changed_worksheet_parts,
        }
    }
}

/// Compression ratios and ZIP offsets can change when Excel rewrites a
/// package even when an OOXML part is byte-for-byte identical. CRC32 plus the
/// expanded size identifies the part content for incremental refreshes without
/// turning a harmless package rewrite into a full workbook refresh.
fn same_part_content(previous: &WorkbookPartRevision, current: &WorkbookPartRevision) -> bool {
    previous.crc32 == current.crc32 && previous.expanded_bytes == current.expanded_bytes
}

impl WorkbookSession {
    /// Open a workbook and retain its bounded result for revision-aware refreshes.
    pub fn open(path: PathBuf, mut options: ReadOptions) -> Result<Self, ReadError> {
        options.spreadsheet.revision_only = false;
        let stamp = WorkbookFileStamp::read(&path, options.max_bytes)?;
        let (result, context) = read_xlsx_with_context(path.clone(), &options)?;
        Ok(Self {
            path,
            options,
            result,
            context,
            stamp,
        })
    }

    #[must_use]
    pub fn result(&self) -> &WorkbookReadResult {
        &self.result
    }

    /// Refresh changed worksheet selections while reusing unchanged selection
    /// results. Workbook, shared-string, style, or theme changes fail over to a
    /// full refresh because they can affect every sheet.
    pub fn refresh(&mut self) -> Result<WorkbookSessionRefresh<'_>, ReadError> {
        let refresh_started = Instant::now();
        let probe_started = Instant::now();
        let stamp = WorkbookFileStamp::read(&self.path, self.options.max_bytes)?;
        if stamp == self.stamp {
            return Ok(WorkbookSessionRefresh {
                result: &self.result,
                diff: WorkbookRevisionDiff {
                    unchanged: true,
                    requires_full_refresh: false,
                    changed_parts: Vec::new(),
                    changed_worksheet_parts: Vec::new(),
                },
                metrics: WorkbookSessionMetrics {
                    changed: false,
                    full_refresh: false,
                    probe_duration_micros: elapsed_micros(probe_started.elapsed()),
                    refresh_duration_micros: elapsed_micros(refresh_started.elapsed()),
                    expanded_bytes_read: 0,
                },
            });
        }
        let (package, revision) = open_workbook_package(
            &self.path,
            self.options.max_bytes,
            self.options.spreadsheet.max_expanded_bytes,
        )?;
        let probe_duration_micros = elapsed_micros(probe_started.elapsed());
        let diff = self.result.revision.diff(&revision);
        if diff.unchanged {
            apply_revision(&mut self.result, revision);
            self.stamp = stamp;
            return Ok(WorkbookSessionRefresh {
                result: &self.result,
                diff,
                metrics: WorkbookSessionMetrics {
                    changed: false,
                    full_refresh: false,
                    probe_duration_micros,
                    refresh_duration_micros: elapsed_micros(refresh_started.elapsed()),
                    expanded_bytes_read: 0,
                },
            });
        }

        let mut full_refresh = diff.requires_full_refresh;
        let mut ranges = Vec::new();
        for part in &diff.changed_worksheet_parts {
            let Some(sheet) = self
                .result
                .workbook
                .sheets
                .iter()
                .find(|sheet| sheet.part == *part)
            else {
                full_refresh = true;
                break;
            };
            let mut sheet_ranges = self
                .result
                .workbook
                .selections
                .iter()
                .filter(|selection| selection.sheet == sheet.name)
                .map(|selection| selection.requested.clone())
                .peekable();
            if sheet_ranges.peek().is_none() {
                full_refresh = true;
                break;
            }
            ranges.extend(sheet_ranges);
        }

        let expanded_bytes_read;
        if full_refresh {
            drop(package);
            let (result, context) = read_xlsx_with_context(self.path.clone(), &self.options)?;
            self.context = context;
            expanded_bytes_read = result.workbook.statistics.expanded_bytes_read;
            self.result = result;
        } else if ranges.is_empty() {
            apply_revision(&mut self.result, revision);
            expanded_bytes_read = 0;
        } else {
            let mut options = self.options.clone();
            options.spreadsheet.ranges = ranges;
            read_xlsx_incremental(
                package,
                &self.path,
                &options,
                &mut self.result,
                &self.context,
                revision,
                &diff.changed_worksheet_parts,
            )?;
            expanded_bytes_read = self.result.workbook.statistics.expanded_bytes_read;
        }
        self.stamp = stamp;
        Ok(WorkbookSessionRefresh {
            result: &self.result,
            diff,
            metrics: WorkbookSessionMetrics {
                changed: true,
                full_refresh,
                probe_duration_micros,
                refresh_duration_micros: elapsed_micros(refresh_started.elapsed()),
                expanded_bytes_read,
            },
        })
    }
}

impl WorkbookFileStamp {
    fn read(path: &Path, max_bytes: usize) -> Result<Self, ReadError> {
        let metadata = std::fs::metadata(path).map_err(|source| ReadError::ReadFile {
            path: path.to_path_buf(),
            source,
        })?;
        if !metadata.is_file() {
            return Err(ReadError::NotRegularFile {
                path: path.to_path_buf(),
            });
        }
        if metadata.len() > max_bytes as u64 {
            return Err(ReadError::InputTooLarge { limit: max_bytes });
        }
        Ok(Self {
            bytes: metadata.len(),
            modified: metadata.modified().ok(),
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DateSystem {
    Excel1900,
    Excel1904,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbookSheet {
    pub index: usize,
    pub name: String,
    pub part: String,
    pub revision: WorkbookPartRevision,
    pub state: SheetState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declared_dimension: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_bounds: Option<String>,
    pub semantic_bounds_complete: bool,
    pub selected: bool,
    pub merged_ranges: Vec<String>,
    /// Worksheet pictures. Inventory entries never carry `dataUri`; pixel
    /// payloads are published only on intersecting selections.
    pub pictures: Vec<WorkbookPicture>,
    pub hidden_rows: usize,
    pub hidden_columns: usize,
    pub print: WorksheetPrintEvidence,
    pub features: WorksheetFeatureInventory,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorksheetFeatureInventory {
    pub scanned: bool,
    /// True only when every cell element in `sheetData` was inspected.
    pub cell_data_complete: bool,
    /// True when parsing reached the end of the worksheet, including features
    /// serialized after `sheetData`.
    pub tail_features_complete: bool,
    pub complete: bool,
    pub feature_references_truncated: bool,
    pub formula_cells: usize,
    pub hyperlinks: Vec<WorkbookHyperlink>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_filter: Option<String>,
    pub table_parts: usize,
    pub drawing_parts: usize,
    pub comment_drawing_parts: usize,
    pub conditional_format_rules: usize,
    pub conditional_format_ranges: Vec<String>,
    pub data_validation_rules: usize,
    pub data_validation_ranges: Vec<String>,
    pub page_setup: bool,
    pub header_footer: bool,
    pub outlined_rows: usize,
    pub outlined_columns: usize,
    pub max_row_outline_level: u8,
    pub max_column_outline_level: u8,
    pub sparklines: usize,
    pub controls: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorksheetPrintEvidence {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub print_area: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub print_titles: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_setup: Option<WorksheetPageSetup>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub print_options: Option<WorksheetPrintOptions>,
    pub row_breaks: Vec<u32>,
    pub column_breaks: Vec<u32>,
    pub header_footer: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorksheetPageSetup {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orientation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paper_size: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fit_to_page: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fit_to_width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fit_to_height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorksheetPrintOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grid_lines: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headings: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbookHyperlink {
    pub reference: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
    pub external: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SheetState {
    Visible,
    Hidden,
    VeryHidden,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DefinedName {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_sheet_index: Option<usize>,
    pub reference: String,
    pub valid_reference: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbookSelection {
    pub requested: String,
    pub sheet: String,
    pub range: String,
    pub bounds: SelectionBounds,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used_bounds: Option<String>,
    pub merged_ranges: Vec<String>,
    /// Pictures whose anchors intersect this selection. Payloads are bounded
    /// independently from the cell cap.
    pub images: Vec<WorkbookPicture>,
    pub images_truncated: bool,
    #[serde(skip_serializing_if = "WorkbookSelectionOverflow::is_empty")]
    pub overflow: WorkbookSelectionOverflow,
    pub cells: Vec<WorkbookCell>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbookPicture {
    pub sheet: String,
    pub from_cell: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_cell: Option<String>,
    /// Zero-based OOXML drawing marker row index.
    pub from_row_index: u32,
    /// Zero-based OOXML drawing marker column index.
    pub from_column_index: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_row_index: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_column_index: Option<u32>,
    pub media_part: String,
    pub content_type: String,
    pub byte_size: usize,
    pub sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_uri: Option<String>,
    pub payload_truncated: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbookSelectionOverflow {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub left: Option<WorkbookColumnOverflow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub right: Option<WorkbookColumnOverflow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub above: Option<WorkbookRowOverflow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub below: Option<WorkbookRowOverflow>,
}

impl WorkbookSelectionOverflow {
    fn is_empty(&self) -> bool {
        self.left.is_none() && self.right.is_none() && self.above.is_none() && self.below.is_none()
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbookColumnOverflow {
    pub min_column: u16,
    pub max_column: u16,
    pub cell_count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbookRowOverflow {
    pub min_row: u32,
    pub max_row: u32,
    pub cell_count: usize,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionBounds {
    pub start_row: u32,
    pub start_column: u16,
    pub end_row: u32,
    pub end_column: u16,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbookCell {
    pub reference: String,
    pub row: u32,
    pub column: u16,
    pub value_type: CellValueType,
    pub value: String,
    pub display: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub formula: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub formula_kind: Option<FormulaKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub formula_reference: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shared_formula_index: Option<u32>,
    pub rich_text: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_strike: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_color: Option<WorkbookFontColor>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rich_text_runs: Vec<WorkbookRichTextRun>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge: Option<WorkbookMergeMembership>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number_format: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbookMergeMembership {
    pub range: String,
    pub anchor: String,
    pub role: WorkbookMergeRole,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkbookMergeRole {
    Anchor,
    Covered,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbookFontColor {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rgb: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexed: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tint: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_rgb: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbookRichTextRun {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strike: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_color: Option<WorkbookFontColor>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CellValueType {
    String,
    Number,
    Boolean,
    Error,
    Date,
    Blank,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FormulaKind {
    Normal,
    Shared,
    Array,
    DataTable,
    Other,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbookStatistics {
    pub archive_entries: usize,
    pub expanded_bytes_read: usize,
    pub scanned_sheets: usize,
    pub cell_elements: usize,
    pub non_empty_cells: usize,
    pub style_only_cells: usize,
    pub returned_cells: usize,
}

#[derive(Debug)]
struct SheetDescriptor {
    name: String,
    state: SheetState,
    path: String,
}

#[derive(Debug)]
struct ParsedWorkbook {
    date_system: DateSystem,
    sheets: Vec<SheetDescriptor>,
    defined_names: Vec<DefinedName>,
}

#[derive(Debug, Clone)]
struct SelectionPlan {
    output_index: usize,
    sheet_index: usize,
    bounds: SelectionBounds,
}

#[derive(Debug, Default)]
struct Styles {
    custom_formats: HashMap<u32, String>,
    cell_formats: Vec<u32>,
    fonts: Vec<FontFormat>,
    cell_font_indexes: Vec<Option<usize>>,
    theme: Vec<Option<String>>,
}

impl Styles {
    fn number_format(&self, style_index: Option<usize>) -> Option<String> {
        let id = *self.cell_formats.get(style_index?)?;
        self.custom_formats
            .get(&id)
            .cloned()
            .or_else(|| builtin_number_format(id).map(str::to_owned))
    }

    fn font(&self, style_index: Option<usize>) -> Option<&FontFormat> {
        let font_index = (*self.cell_font_indexes.get(style_index?)?)?;
        self.fonts.get(font_index)
    }
}

#[derive(Debug, Clone, Default)]
struct FontFormat {
    strike: Option<bool>,
    color: Option<WorkbookFontColor>,
}

#[derive(Debug, Clone)]
struct SharedString {
    text: String,
    rich: bool,
    runs: Vec<WorkbookRichTextRun>,
}

#[derive(Debug, Clone)]
struct PartRelationship {
    relationship_type: String,
    target: String,
    external: bool,
}

#[derive(Debug, Clone, Default)]
struct PackageContentTypes {
    defaults: HashMap<String, String>,
    overrides: HashMap<String, String>,
}

impl PackageContentTypes {
    fn for_part(&self, part: &str) -> Option<String> {
        let part = part.trim_start_matches('/');
        self.overrides.get(part).cloned().or_else(|| {
            let extension = part.rsplit_once('.')?.1.to_ascii_lowercase();
            self.defaults.get(&extension).cloned()
        })
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct DrawingMarker {
    row: Option<u32>,
    column: Option<u32>,
}

#[derive(Debug, Default)]
struct DrawingAnchorBuilder {
    from: DrawingMarker,
    to: DrawingMarker,
    two_cell: bool,
    has_picture: bool,
    inside_picture: bool,
    image_relationship_id: Option<String>,
}

#[derive(Debug)]
struct DrawingPicture {
    from: DrawingMarker,
    to: Option<DrawingMarker>,
    media_part: String,
}

#[derive(Debug, Clone)]
struct MediaAsset {
    content_type: String,
    bytes: Vec<u8>,
    sha256: String,
}

#[derive(Debug, Default)]
struct CellBuilder {
    reference: String,
    cell_type: Option<String>,
    style_index: Option<usize>,
    value: String,
    inline: String,
    formula: String,
    formula_kind: Option<FormulaKind>,
    formula_reference: Option<String>,
    shared_formula_index: Option<u32>,
    rich_text: bool,
    rich_text_runs: Vec<WorkbookRichTextRun>,
    current_run: Option<WorkbookRichTextRun>,
    inside_run_properties: bool,
    has_value: bool,
    has_inline: bool,
    has_formula: bool,
    capture_value: bool,
    capture_formula: bool,
    capture_inline_text: bool,
}

#[derive(Debug, Default)]
struct SheetScan {
    dimension: Option<String>,
    semantic: Option<SelectionBounds>,
    merged_ranges: Vec<String>,
    drawing_relationship_ids: Vec<String>,
    pictures: Vec<WorkbookPicture>,
    hidden_rows: usize,
    hidden_columns: usize,
    print: WorksheetPrintEvidence,
    cell_elements: usize,
    non_empty_cells: usize,
    style_only_cells: usize,
    features: WorksheetFeatureInventory,
}

struct ArchiveReader<R> {
    archive: ZipArchive<R>,
    expanded_bytes: usize,
    max_expanded_bytes: usize,
}

struct CountingReader<R> {
    inner: R,
    bytes_read: usize,
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.bytes_read = self.bytes_read.saturating_add(read);
        Ok(read)
    }
}

impl<R: Read + Seek> ArchiveReader<R> {
    fn new(reader: R, max_expanded_bytes: usize) -> Result<Self, ReadError> {
        let archive = ZipArchive::new(reader).map_err(invalid_zip)?;
        Ok(Self {
            archive,
            expanded_bytes: 0,
            max_expanded_bytes,
        })
    }

    fn entry_count(&self) -> usize {
        self.archive.len()
    }

    fn feature_inventory(&self) -> WorkbookFeatureInventory {
        let mut inventory = WorkbookFeatureInventory::default();
        for name in self.archive.file_names() {
            let name = name.to_ascii_lowercase();
            if name.starts_with("xl/theme/") && name.ends_with(".xml") {
                inventory.theme_parts += 1;
            } else if name.starts_with("xl/drawings/drawing") && name.ends_with(".xml") {
                inventory.drawing_parts += 1;
            } else if name.starts_with("xl/charts/chart") && name.ends_with(".xml") {
                inventory.chart_parts += 1;
            } else if name.starts_with("xl/media/") && !name.ends_with('/') {
                inventory.image_parts += 1;
            } else if name.starts_with("xl/tables/table") && name.ends_with(".xml") {
                inventory.table_parts += 1;
            } else if name.starts_with("xl/comments") && name.ends_with(".xml") {
                inventory.comment_parts += 1;
            } else if name.starts_with("xl/ctrlprops/") && name.ends_with(".xml") {
                inventory.control_property_parts += 1;
            } else if name == "xl/vbaproject.bin" {
                inventory.macro_project_parts += 1;
            }
        }
        inventory
    }

    fn revision(&mut self) -> Result<WorkbookRevision, ReadError> {
        let mut parts = Vec::with_capacity(self.archive.len());
        for index in 0..self.archive.len() {
            let entry = self.archive.by_index_raw(index).map_err(invalid_zip)?;
            if entry.is_dir() {
                continue;
            }
            if entry.enclosed_name().is_none() {
                return Err(ReadError::InvalidXlsx(
                    "ZIP package contains an unsafe part path".to_owned(),
                ));
            }
            parts.push(WorkbookPartRevision {
                name: entry.name().to_owned(),
                crc32: format!("{:08x}", entry.crc32()),
                compressed_bytes: entry.compressed_size(),
                expanded_bytes: entry.size(),
            });
        }
        parts.sort_by(|left, right| left.name.cmp(&right.name));
        let mut fingerprint = 0xcbf2_9ce4_8422_2325_u64;
        let mut compressed_bytes = 0_u64;
        let mut expanded_bytes = 0_u64;
        for part in &parts {
            fnv1a_update(&mut fingerprint, part.name.as_bytes());
            fnv1a_update(&mut fingerprint, part.crc32.as_bytes());
            fnv1a_update(&mut fingerprint, &part.expanded_bytes.to_le_bytes());
            compressed_bytes = compressed_bytes.saturating_add(part.compressed_bytes);
            expanded_bytes = expanded_bytes.saturating_add(part.expanded_bytes);
        }
        Ok(WorkbookRevision {
            id: format!("fnv1a64-{fingerprint:016x}"),
            compressed_bytes,
            expanded_bytes,
            parts,
        })
    }

    fn read_required_xml(&mut self, name: &str) -> Result<String, ReadError> {
        self.read_xml(name)?.ok_or_else(|| {
            ReadError::InvalidXlsx(format!("required OOXML part `{name}` is missing"))
        })
    }

    fn read_xml(&mut self, name: &str) -> Result<Option<String>, ReadError> {
        let Some(bytes) = self.read_bytes(name)? else {
            return Ok(None);
        };
        String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| ReadError::InvalidXlsx(format!("OOXML part `{name}` is not UTF-8 XML")))
    }

    fn read_bytes(&mut self, name: &str) -> Result<Option<Vec<u8>>, ReadError> {
        let mut entry = match self.archive.by_name(name) {
            Ok(entry) => entry,
            Err(ZipError::FileNotFound) => return Ok(None),
            Err(error) => return Err(invalid_zip(error)),
        };
        if entry.is_dir() {
            return Err(ReadError::InvalidXlsx(format!(
                "OOXML part `{name}` is a directory"
            )));
        }
        let remaining = self
            .max_expanded_bytes
            .checked_sub(self.expanded_bytes)
            .ok_or(ReadError::SpreadsheetExpandedTooLarge {
                limit: self.max_expanded_bytes,
            })?;
        if entry.size() > u64::try_from(remaining).unwrap_or(u64::MAX) {
            return Err(ReadError::SpreadsheetExpandedTooLarge {
                limit: self.max_expanded_bytes,
            });
        }
        let limit = u64::try_from(remaining)
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        let mut bytes = Vec::with_capacity(
            usize::try_from(entry.size())
                .unwrap_or(remaining)
                .min(remaining),
        );
        entry
            .by_ref()
            .take(limit)
            .read_to_end(&mut bytes)
            .map_err(|_| {
                ReadError::InvalidXlsx(format!("OOXML part `{name}` could not be read"))
            })?;
        if bytes.len() > remaining {
            return Err(ReadError::SpreadsheetExpandedTooLarge {
                limit: self.max_expanded_bytes,
            });
        }
        self.expanded_bytes += bytes.len();
        Ok(Some(bytes))
    }

    #[allow(clippy::too_many_arguments)]
    fn scan_worksheet(
        &mut self,
        name: &str,
        plans: &[SelectionPlan],
        selections: &mut [WorkbookSelection],
        shared_strings: &[SharedString],
        styles: &Styles,
        date_system: DateSystem,
        options: &SpreadsheetReadOptions,
        relationships: &HashMap<String, PartRelationship>,
        remaining_cells: &mut usize,
        warnings: &mut Vec<String>,
    ) -> Result<SheetScan, ReadError> {
        let entry = match self.archive.by_name(name) {
            Ok(entry) => entry,
            Err(ZipError::FileNotFound) => {
                return Err(ReadError::InvalidXlsx(format!(
                    "required OOXML part `{name}` is missing"
                )));
            }
            Err(error) => return Err(invalid_zip(error)),
        };
        if entry.is_dir() {
            return Err(ReadError::InvalidXlsx(format!(
                "OOXML part `{name}` is a directory"
            )));
        }
        let remaining = self
            .max_expanded_bytes
            .checked_sub(self.expanded_bytes)
            .ok_or(ReadError::SpreadsheetExpandedTooLarge {
                limit: self.max_expanded_bytes,
            })?;
        let limit = u64::try_from(remaining)
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        let mut counted = CountingReader {
            inner: entry.take(limit),
            bytes_read: 0,
        };
        let result = parse_worksheet(
            BufReader::new(&mut counted),
            plans,
            selections,
            shared_strings,
            styles,
            date_system,
            options,
            relationships,
            remaining_cells,
            warnings,
        );
        let bytes_read = counted.bytes_read;
        self.expanded_bytes = self
            .expanded_bytes
            .saturating_add(bytes_read.min(remaining));
        if bytes_read > remaining {
            return Err(ReadError::SpreadsheetExpandedTooLarge {
                limit: self.max_expanded_bytes,
            });
        }
        result
    }
}

/// Read only ZIP central-directory revision metadata. No OOXML part is
/// expanded, so this is suitable for frequent change probes.
pub fn inspect_workbook_revision(
    path: &Path,
    max_bytes: usize,
) -> Result<WorkbookRevision, ReadError> {
    open_workbook_package(path, max_bytes, usize::MAX).map(|(_package, revision)| revision)
}

fn open_workbook_package(
    path: &Path,
    max_bytes: usize,
    max_expanded_bytes: usize,
) -> Result<(ArchiveReader<File>, WorkbookRevision), ReadError> {
    WorkbookFileStamp::read(path, max_bytes)?;
    let file = File::open(path).map_err(|source| ReadError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    let mut package = ArchiveReader::new(file, max_expanded_bytes)?;
    let revision = package.revision()?;
    Ok((package, revision))
}

fn apply_revision(result: &mut WorkbookReadResult, revision: WorkbookRevision) {
    let part_revisions: HashMap<&str, &WorkbookPartRevision> = revision
        .parts
        .iter()
        .map(|part| (part.name.as_str(), part))
        .collect();
    for sheet in &mut result.workbook.sheets {
        if let Some(part) = part_revisions.get(sheet.part.as_str()) {
            sheet.revision = (*part).clone();
        }
    }
    result.revision = revision;
}

#[allow(clippy::too_many_arguments)]
fn read_xlsx_incremental(
    mut package: ArchiveReader<File>,
    path: &Path,
    options: &ReadOptions,
    result: &mut WorkbookReadResult,
    context: &WorkbookSessionContext,
    revision: WorkbookRevision,
    changed_worksheet_parts: &[String],
) -> Result<(), ReadError> {
    struct SheetPatch {
        sheet_index: usize,
        scan: SheetScan,
        selections: Vec<(usize, WorkbookSelection)>,
    }

    let started = Instant::now();
    let archive_entries = package.entry_count();

    let mut statistics = WorkbookStatistics {
        archive_entries,
        ..WorkbookStatistics::default()
    };
    let mut warnings = result.warnings.clone();
    let partial_warning = "incremental worksheet refresh is bounded to the cached selections; later cell bodies and semantic bounds may be partial while worksheet tail features are still scanned";
    if !warnings.iter().any(|warning| warning == partial_warning) {
        warnings.push(partial_warning.to_owned());
    }
    let changed_sheet_names = changed_worksheet_parts
        .iter()
        .map(|part| {
            result
                .workbook
                .sheets
                .iter()
                .find(|sheet| sheet.part == *part)
                .map(|sheet| sheet.name.clone())
                .ok_or_else(|| {
                    ReadError::InvalidXlsx(format!(
                        "changed worksheet part `{part}` is absent from the cached workbook manifest"
                    ))
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let retained_cells = result
        .workbook
        .selections
        .iter()
        .filter(|selection| !changed_sheet_names.contains(&selection.sheet))
        .map(|selection| selection.cells.len())
        .sum::<usize>();
    let mut remaining_cells = options.spreadsheet.max_cells.saturating_sub(retained_cells);
    let retained_image_payload_bytes = result
        .workbook
        .selections
        .iter()
        .filter(|selection| !changed_sheet_names.contains(&selection.sheet))
        .flat_map(|selection| &selection.images)
        .filter(|picture| picture.data_uri.is_some())
        .map(|picture| picture.byte_size)
        .sum::<usize>();
    let mut remaining_image_payload_bytes =
        MAX_TOTAL_IMAGE_PAYLOAD_BYTES.saturating_sub(retained_image_payload_bytes);
    let mut media_cache = HashMap::new();
    let mut patches = Vec::new();
    for part in changed_worksheet_parts {
        let sheet_index = result
            .workbook
            .sheets
            .iter()
            .position(|sheet| sheet.part == *part)
            .ok_or_else(|| {
                ReadError::InvalidXlsx(format!(
                    "changed worksheet part `{part}` is absent from the cached workbook manifest"
                ))
            })?;
        let sheet_name = result.workbook.sheets[sheet_index].name.clone();
        let selected = result
            .workbook
            .selections
            .iter()
            .enumerate()
            .filter(|(_, selection)| selection.sheet == sheet_name)
            .map(|(global_index, selection)| {
                let mut selection = selection.clone();
                selection.cells.clear();
                selection.used_bounds = None;
                selection.merged_ranges.clear();
                selection.images.clear();
                selection.images_truncated = false;
                selection.overflow = WorkbookSelectionOverflow::default();
                selection.truncated = false;
                (global_index, selection)
            })
            .collect::<Vec<_>>();
        let (global_indexes, mut local_selections): (Vec<_>, Vec<_>) = selected.into_iter().unzip();
        let plans = local_selections
            .iter()
            .enumerate()
            .map(|(output_index, selection)| SelectionPlan {
                output_index,
                sheet_index,
                bounds: selection.bounds,
            })
            .collect::<Vec<_>>();
        if plans.is_empty() {
            return Err(ReadError::InvalidXlsx(format!(
                "changed worksheet `{sheet_name}` has no cached selection"
            )));
        }
        let relationship_path = worksheet_relationship_path(part)?;
        let relationships = package
            .read_xml(&relationship_path)?
            .map(|xml| parse_part_relationships(&xml))
            .transpose()?
            .unwrap_or_default();
        let mut scan = package.scan_worksheet(
            part,
            &plans,
            &mut local_selections,
            &context.shared_strings,
            &context.styles,
            result.workbook.date_system,
            &options.spreadsheet,
            &relationships,
            &mut remaining_cells,
            &mut warnings,
        )?;
        let (pictures, picture_references_truncated) = read_sheet_pictures(
            &mut package,
            &sheet_name,
            part,
            &scan.drawing_relationship_ids,
            &relationships,
            &context.content_types,
            &mut local_selections,
            &mut media_cache,
            &mut remaining_image_payload_bytes,
            &mut warnings,
        )?;
        scan.pictures = pictures;
        scan.features.feature_references_truncated |= picture_references_truncated;
        statistics.scanned_sheets += 1;
        statistics.cell_elements += scan.cell_elements;
        statistics.non_empty_cells += scan.non_empty_cells;
        statistics.style_only_cells += scan.style_only_cells;
        statistics.returned_cells += local_selections
            .iter()
            .map(|selection| selection.cells.len())
            .sum::<usize>();
        patches.push(SheetPatch {
            sheet_index,
            scan,
            selections: global_indexes.into_iter().zip(local_selections).collect(),
        });
    }
    statistics.expanded_bytes_read = package.expanded_bytes;

    let mut features = package.feature_inventory();
    features.rich_string_items = context
        .shared_strings
        .iter()
        .filter(|item| item.rich)
        .count();
    features.cell_formats = context.styles.cell_formats.len();
    features.custom_number_formats = context.styles.custom_formats.len();
    let mut refreshed_selection_indexes = Vec::new();
    for patch in patches {
        for (global_index, selection) in patch.selections {
            result.workbook.selections[global_index] = selection;
            refreshed_selection_indexes.push(global_index);
        }
        let sheet = &mut result.workbook.sheets[patch.sheet_index];
        sheet.declared_dimension = patch.scan.dimension;
        sheet.semantic_bounds = patch.scan.semantic.map(format_bounds);
        sheet.semantic_bounds_complete = patch.scan.features.cell_data_complete;
        sheet.merged_ranges = patch.scan.merged_ranges;
        sheet.pictures = patch.scan.pictures;
        sheet.hidden_rows = patch.scan.hidden_rows;
        sheet.hidden_columns = patch.scan.hidden_columns;
        apply_worksheet_print_scan(&mut sheet.print, patch.scan.print);
        sheet.features = patch.scan.features;
    }
    statistics.returned_cells = result
        .workbook
        .selections
        .iter()
        .map(|selection| selection.cells.len())
        .sum();
    if result
        .workbook
        .selections
        .iter()
        .any(|selection| selection.truncated)
    {
        let warning = format!(
            "spreadsheet output was truncated at {} returned cells",
            options.spreadsheet.max_cells
        );
        if !warnings.iter().any(|existing| existing == &warning) {
            warnings.push(warning);
        }
    }
    features.inventory_complete = result
        .workbook
        .sheets
        .iter()
        .all(|sheet| sheet.features.complete);
    result.workbook.features = features;
    result.workbook.statistics = statistics;
    result.source.bytes = usize::try_from(
        std::fs::metadata(path)
            .map_err(|source| ReadError::ReadFile {
                path: path.to_path_buf(),
                source,
            })?
            .len(),
    )
    .unwrap_or(usize::MAX);
    result.warnings = warnings;
    apply_revision(result, revision);
    refreshed_selection_indexes.sort_unstable();
    refreshed_selection_indexes.dedup();
    let markdown_update =
        render_markdown_generated_update(&result.workbook, &refreshed_selection_indexes);
    let html_update = render_html_generated_update(&result.workbook, &refreshed_selection_indexes);
    result.content = merge_generated_blocks(&result.content, &markdown_update)?;
    result.content_html = merge_generated_blocks(&result.content_html, &html_update)?;
    let elapsed = started.elapsed();
    result.extraction.duration_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
    result.extraction.duration_micros = elapsed_micros(elapsed);
    Ok(())
}

fn elapsed_micros(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

pub(crate) fn is_xlsx_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("xlsx"))
}

pub(crate) fn read_xlsx(
    path: PathBuf,
    options: &ReadOptions,
) -> Result<WorkbookReadResult, ReadError> {
    read_xlsx_with_context(path, options).map(|(result, _context)| result)
}

fn read_xlsx_with_context(
    path: PathBuf,
    options: &ReadOptions,
) -> Result<(WorkbookReadResult, WorkbookSessionContext), ReadError> {
    if options.spreadsheet.max_cells == 0
        || options.spreadsheet.max_expanded_bytes == 0
        || options.spreadsheet.preview_rows == 0
        || options.spreadsheet.preview_columns == 0
    {
        return Err(ReadError::InvalidXlsx(
            "spreadsheet limits must be greater than zero".to_owned(),
        ));
    }

    let metadata = std::fs::metadata(&path).map_err(|source| ReadError::ReadFile {
        path: path.clone(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(ReadError::NotRegularFile { path });
    }
    if metadata.len() > options.max_bytes as u64 {
        return Err(ReadError::InputTooLarge {
            limit: options.max_bytes,
        });
    }
    let canonical = std::fs::canonicalize(&path).map_err(|source| ReadError::ResolveFile {
        path: path.clone(),
        source,
    })?;
    let file = File::open(&path).map_err(|source| ReadError::ReadFile {
        path: path.clone(),
        source,
    })?;

    let started = Instant::now();
    let mut package = ArchiveReader::new(file, options.spreadsheet.max_expanded_bytes)?;
    let archive_entries = package.entry_count();
    let mut features = package.feature_inventory();
    let revision = package.revision()?;
    let part_revisions: HashMap<&str, &WorkbookPartRevision> = revision
        .parts
        .iter()
        .map(|part| (part.name.as_str(), part))
        .collect();
    let content_types_xml = package.read_required_xml("[Content_Types].xml")?;
    if !content_types_xml.contains(XLSX_CONTENT_TYPE) {
        return Err(ReadError::InvalidXlsx(
            "package does not declare an XLSX workbook content type".to_owned(),
        ));
    }
    let content_types = parse_content_types(&content_types_xml)?;
    let workbook_xml = package.read_required_xml("xl/workbook.xml")?;
    let relationships_xml = package.read_required_xml("xl/_rels/workbook.xml.rels")?;
    let relationships = parse_relationships(&relationships_xml)?;
    let mut workbook = parse_workbook(&workbook_xml, &relationships)?;
    if workbook.sheets.is_empty() {
        return Err(ReadError::InvalidXlsx(
            "workbook contains no worksheets".to_owned(),
        ));
    }
    let (shared_strings, styles) = if options.spreadsheet.revision_only {
        (Vec::new(), Styles::default())
    } else {
        let mut shared_strings = package
            .read_xml("xl/sharedStrings.xml")?
            .map(|xml| parse_shared_strings(&xml))
            .transpose()?
            .unwrap_or_default();
        let mut styles = package
            .read_xml("xl/styles.xml")?
            .map(|xml| parse_styles(&xml))
            .transpose()?
            .unwrap_or_default();
        let theme = package
            .read_xml("xl/theme/theme1.xml")?
            .map(|xml| parse_theme_colors(&xml))
            .transpose()?
            .unwrap_or_default();
        resolve_published_colors(&mut styles, &mut shared_strings, &theme);
        (shared_strings, styles)
    };
    features.rich_string_items = shared_strings.iter().filter(|item| item.rich).count();
    features.cell_formats = styles.cell_formats.len();
    features.custom_number_formats = styles.custom_formats.len();

    let (plans, mut selections) = if options.spreadsheet.revision_only {
        (Vec::new(), Vec::new())
    } else {
        plan_selections(&workbook.sheets, &options.spreadsheet)?
    };
    if let Some(sheet) = workbook
        .sheets
        .iter()
        .find(|sheet| !part_revisions.contains_key(sheet.path.as_str()))
    {
        return Err(ReadError::InvalidXlsx(format!(
            "worksheet part `{}` is missing",
            sheet.path
        )));
    }
    let mut sheets: Vec<WorkbookSheet> = workbook
        .sheets
        .iter()
        .enumerate()
        .map(|(index, sheet)| WorkbookSheet {
            index,
            name: sheet.name.clone(),
            part: sheet.path.clone(),
            revision: (*part_revisions
                .get(sheet.path.as_str())
                .expect("worksheet part revisions were validated"))
            .clone(),
            state: sheet.state,
            declared_dimension: None,
            semantic_bounds: None,
            semantic_bounds_complete: false,
            selected: plans.iter().any(|plan| plan.sheet_index == index),
            merged_ranges: Vec::new(),
            pictures: Vec::new(),
            hidden_rows: 0,
            hidden_columns: 0,
            print: WorksheetPrintEvidence {
                print_area: print_defined_name(&workbook.defined_names, index, "_xlnm.Print_Area"),
                print_titles: print_defined_name(
                    &workbook.defined_names,
                    index,
                    "_xlnm.Print_Titles",
                ),
                ..WorksheetPrintEvidence::default()
            },
            features: WorksheetFeatureInventory::default(),
        })
        .collect();
    let mut statistics = WorkbookStatistics {
        archive_entries,
        ..WorkbookStatistics::default()
    };
    let mut warnings = Vec::new();
    let mut remaining_cells = options.spreadsheet.max_cells;
    let mut remaining_image_payload_bytes = MAX_TOTAL_IMAGE_PAYLOAD_BYTES;
    let mut media_cache = HashMap::new();

    let mut plans_by_sheet: BTreeMap<usize, Vec<SelectionPlan>> = BTreeMap::new();
    for plan in plans {
        plans_by_sheet
            .entry(plan.sheet_index)
            .or_default()
            .push(plan);
    }
    let scan_all_sheets =
        options.spreadsheet.ranges.is_empty() && !options.spreadsheet.revision_only;
    let sheet_indexes: Vec<usize> = if scan_all_sheets {
        (0..workbook.sheets.len()).collect()
    } else {
        plans_by_sheet.keys().copied().collect()
    };
    for sheet_index in sheet_indexes {
        let sheet_plans = plans_by_sheet.remove(&sheet_index).unwrap_or_default();
        let descriptor = &workbook.sheets[sheet_index];
        let relationship_path = worksheet_relationship_path(&descriptor.path)?;
        let sheet_relationships = package
            .read_xml(&relationship_path)?
            .map(|xml| parse_part_relationships(&xml))
            .transpose()?
            .unwrap_or_default();
        let mut scan = package.scan_worksheet(
            &descriptor.path,
            &sheet_plans,
            &mut selections,
            &shared_strings,
            &styles,
            workbook.date_system,
            &options.spreadsheet,
            &sheet_relationships,
            &mut remaining_cells,
            &mut warnings,
        )?;
        let (pictures, picture_references_truncated) = read_sheet_pictures(
            &mut package,
            &descriptor.name,
            &descriptor.path,
            &scan.drawing_relationship_ids,
            &sheet_relationships,
            &content_types,
            &mut selections,
            &mut media_cache,
            &mut remaining_image_payload_bytes,
            &mut warnings,
        )?;
        scan.pictures = pictures;
        scan.features.feature_references_truncated |= picture_references_truncated;
        let sheet = &mut sheets[sheet_index];
        sheet.declared_dimension = scan.dimension;
        sheet.semantic_bounds = scan.semantic.map(format_bounds);
        sheet.semantic_bounds_complete = scan.features.cell_data_complete;
        sheet.merged_ranges = scan.merged_ranges;
        sheet.pictures = scan.pictures;
        sheet.hidden_rows = scan.hidden_rows;
        sheet.hidden_columns = scan.hidden_columns;
        apply_worksheet_print_scan(&mut sheet.print, scan.print);
        sheet.features = scan.features;
        statistics.scanned_sheets += 1;
        statistics.cell_elements += scan.cell_elements;
        statistics.non_empty_cells += scan.non_empty_cells;
        statistics.style_only_cells += scan.style_only_cells;
    }
    statistics.returned_cells = selections
        .iter()
        .map(|selection| selection.cells.len())
        .sum();
    statistics.expanded_bytes_read = package.expanded_bytes;
    features.inventory_complete = statistics.scanned_sheets == workbook.sheets.len()
        && sheets.iter().all(|sheet| sheet.features.complete);
    if selections.iter().any(|selection| selection.truncated) {
        warnings.push(format!(
            "spreadsheet output was truncated at {} returned cells",
            options.spreadsheet.max_cells
        ));
    }
    if sheets.iter().any(|sheet| {
        sheet.features.scanned
            && !sheet.features.cell_data_complete
            && sheet.features.tail_features_complete
    }) {
        warnings.push(
            "targeted worksheet scan skipped cell bodies after the requested rows; semantic bounds and cell statistics are partial, but worksheet tail features were scanned"
                .to_owned(),
        );
    }
    if workbook
        .defined_names
        .iter()
        .any(|defined_name| !defined_name.valid_reference)
    {
        warnings.push("workbook contains defined names with invalid #REF! references".to_owned());
    }

    let title = path
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| "workbook.xlsx".to_owned());
    let workbook_info = WorkbookInfo {
        format: "xlsx",
        date_system: workbook.date_system,
        sheets,
        defined_names: std::mem::take(&mut workbook.defined_names),
        selections,
        features,
        statistics,
    };
    let content = render_markdown(&title, &workbook_info);
    let content_html = render_html(&title, &workbook_info);
    let elapsed = started.elapsed();
    let duration_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
    let duration_micros = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
    let source = SourceInfo {
        kind: SourceKind::File,
        requested: path.display().to_string(),
        resolved_url: Url::from_file_path(canonical).ok(),
        content_type: Some(XLSX_CONTENT_TYPE.to_owned()),
        charset: "binary".to_owned(),
        bytes: usize::try_from(metadata.len()).unwrap_or(usize::MAX),
    };

    let result = WorkbookReadResult {
        schema_version: 1,
        artifact_kind: ArtifactKind::Workbook,
        content,
        content_html,
        metadata: WorkbookMetadata { title },
        source,
        extraction: WorkbookExtractionInfo {
            method: WorkbookExtractionMethod::OoxmlSparse,
            duration_ms,
            duration_micros,
        },
        revision,
        workbook: workbook_info,
        warnings,
    };
    Ok((
        result,
        WorkbookSessionContext {
            shared_strings,
            styles,
            content_types,
        },
    ))
}

fn parse_relationships(xml: &str) -> Result<HashMap<String, String>, ReadError> {
    let mut reader = xml_reader(xml);
    let mut relationships = HashMap::new();
    loop {
        match reader.read_event().map_err(xml_error)? {
            Event::Start(element) | Event::Empty(element)
                if local_name(element.name().as_ref()) == b"Relationship" =>
            {
                let id = attribute(&element, b"Id")?.ok_or_else(|| {
                    ReadError::InvalidXlsx("workbook relationship has no Id".to_owned())
                })?;
                let target = attribute(&element, b"Target")?.ok_or_else(|| {
                    ReadError::InvalidXlsx("workbook relationship has no Target".to_owned())
                })?;
                let relationship_type = attribute(&element, b"Type")?.unwrap_or_default();
                if relationship_type.ends_with("/worksheet") {
                    relationships.insert(id, normalize_workbook_target(&target)?);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(relationships)
}

fn parse_part_relationships(xml: &str) -> Result<HashMap<String, PartRelationship>, ReadError> {
    let mut reader = xml_reader(xml);
    let mut relationships = HashMap::new();
    loop {
        match reader.read_event().map_err(xml_error)? {
            Event::Start(element) | Event::Empty(element)
                if local_name(element.name().as_ref()) == b"Relationship" =>
            {
                let id = attribute(&element, b"Id")?.ok_or_else(|| {
                    ReadError::InvalidXlsx("part relationship has no Id".to_owned())
                })?;
                let relationship_type = attribute(&element, b"Type")?.unwrap_or_default();
                let target = attribute(&element, b"Target")?.ok_or_else(|| {
                    ReadError::InvalidXlsx("part relationship has no Target".to_owned())
                })?;
                let external = attribute(&element, b"TargetMode")?
                    .as_deref()
                    .is_some_and(|mode| mode.eq_ignore_ascii_case("external"));
                relationships.insert(
                    id,
                    PartRelationship {
                        relationship_type,
                        target,
                        external,
                    },
                );
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(relationships)
}

fn parse_content_types(xml: &str) -> Result<PackageContentTypes, ReadError> {
    let mut reader = xml_reader(xml);
    let mut content_types = PackageContentTypes::default();
    loop {
        match reader.read_event().map_err(xml_error)? {
            Event::Start(element) | Event::Empty(element)
                if local_name(element.name().as_ref()) == b"Default" =>
            {
                let extension = attribute(&element, b"Extension")?
                    .ok_or_else(|| {
                        ReadError::InvalidXlsx("content-type default has no Extension".to_owned())
                    })?
                    .to_ascii_lowercase();
                let content_type = attribute(&element, b"ContentType")?.ok_or_else(|| {
                    ReadError::InvalidXlsx("content-type default has no ContentType".to_owned())
                })?;
                content_types.defaults.insert(extension, content_type);
            }
            Event::Start(element) | Event::Empty(element)
                if local_name(element.name().as_ref()) == b"Override" =>
            {
                let part = attribute(&element, b"PartName")?
                    .ok_or_else(|| {
                        ReadError::InvalidXlsx("content-type override has no PartName".to_owned())
                    })?
                    .trim_start_matches('/')
                    .to_owned();
                let content_type = attribute(&element, b"ContentType")?.ok_or_else(|| {
                    ReadError::InvalidXlsx("content-type override has no ContentType".to_owned())
                })?;
                content_types.overrides.insert(part, content_type);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(content_types)
}

#[derive(Debug, Clone, Copy)]
enum DrawingMarkerKind {
    From,
    To,
}

#[derive(Debug, Clone, Copy)]
enum DrawingMarkerField {
    Row,
    Column,
}

fn parse_drawing_pictures(
    xml: &str,
    drawing_part: &str,
    relationships: &HashMap<String, PartRelationship>,
) -> Result<Vec<DrawingPicture>, ReadError> {
    let mut reader = xml_reader(xml);
    let mut pictures = Vec::new();
    let mut anchor: Option<DrawingAnchorBuilder> = None;
    let mut marker_kind: Option<DrawingMarkerKind> = None;
    let mut marker_field: Option<DrawingMarkerField> = None;
    loop {
        match reader.read_event().map_err(xml_error)? {
            Event::Start(element)
                if matches!(
                    local_name(element.name().as_ref()),
                    b"twoCellAnchor" | b"oneCellAnchor"
                ) =>
            {
                anchor = Some(DrawingAnchorBuilder {
                    two_cell: local_name(element.name().as_ref()) == b"twoCellAnchor",
                    ..DrawingAnchorBuilder::default()
                });
            }
            Event::Start(element)
                if anchor.is_some() && local_name(element.name().as_ref()) == b"from" =>
            {
                marker_kind = Some(DrawingMarkerKind::From);
            }
            Event::Start(element)
                if anchor.is_some() && local_name(element.name().as_ref()) == b"to" =>
            {
                marker_kind = Some(DrawingMarkerKind::To);
            }
            Event::Start(element)
                if marker_kind.is_some() && local_name(element.name().as_ref()) == b"row" =>
            {
                marker_field = Some(DrawingMarkerField::Row);
            }
            Event::Start(element)
                if marker_kind.is_some() && local_name(element.name().as_ref()) == b"col" =>
            {
                marker_field = Some(DrawingMarkerField::Column);
            }
            Event::Text(text)
                if anchor.is_some() && marker_kind.is_some() && marker_field.is_some() =>
            {
                let value = text.decode().map_err(encoding_error)?;
                if let Ok(value) = value.trim().parse::<u32>() {
                    let marker = match marker_kind.expect("marker kind is checked") {
                        DrawingMarkerKind::From => {
                            &mut anchor.as_mut().expect("anchor is checked").from
                        }
                        DrawingMarkerKind::To => {
                            &mut anchor.as_mut().expect("anchor is checked").to
                        }
                    };
                    match marker_field.expect("marker field is checked") {
                        DrawingMarkerField::Row => marker.row = Some(value),
                        DrawingMarkerField::Column => marker.column = Some(value),
                    }
                }
            }
            Event::Start(element)
                if anchor.is_some() && local_name(element.name().as_ref()) == b"pic" =>
            {
                let anchor = anchor.as_mut().expect("anchor is checked");
                anchor.has_picture = true;
                anchor.inside_picture = true;
            }
            Event::Empty(element)
                if anchor.is_some() && local_name(element.name().as_ref()) == b"pic" =>
            {
                anchor.as_mut().expect("anchor is checked").has_picture = true;
            }
            Event::Start(element) | Event::Empty(element)
                if anchor.as_ref().is_some_and(|anchor| anchor.inside_picture)
                    && local_name(element.name().as_ref()) == b"blip" =>
            {
                if let Some(relationship_id) = attribute_exact(&element, b"r:embed")? {
                    anchor
                        .as_mut()
                        .expect("anchor is checked")
                        .image_relationship_id = Some(relationship_id);
                }
            }
            Event::End(element)
                if matches!(local_name(element.name().as_ref()), b"row" | b"col") =>
            {
                marker_field = None;
            }
            Event::End(element)
                if matches!(local_name(element.name().as_ref()), b"from" | b"to") =>
            {
                marker_kind = None;
            }
            Event::End(element) if local_name(element.name().as_ref()) == b"pic" => {
                if let Some(anchor) = anchor.as_mut() {
                    anchor.inside_picture = false;
                }
            }
            Event::End(element)
                if matches!(
                    local_name(element.name().as_ref()),
                    b"twoCellAnchor" | b"oneCellAnchor"
                ) =>
            {
                if let Some(anchor) = anchor.take()
                    && anchor.has_picture
                    && anchor.from.row.is_some()
                    && anchor.from.column.is_some()
                    && (!anchor.two_cell || (anchor.to.row.is_some() && anchor.to.column.is_some()))
                    && let Some(relationship_id) = anchor.image_relationship_id
                    && let Some(relationship) = relationships.get(&relationship_id)
                    && relationship.relationship_type.ends_with("/image")
                    && !relationship.external
                {
                    pictures.push(DrawingPicture {
                        from: anchor.from,
                        to: anchor.two_cell.then_some(anchor.to),
                        media_part: resolve_part_target(drawing_part, &relationship.target)?,
                    });
                }
                marker_kind = None;
                marker_field = None;
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(pictures)
}

#[allow(clippy::too_many_arguments)]
fn read_sheet_pictures<R: Read + Seek>(
    package: &mut ArchiveReader<R>,
    sheet_name: &str,
    sheet_part: &str,
    drawing_relationship_ids: &[String],
    sheet_relationships: &HashMap<String, PartRelationship>,
    content_types: &PackageContentTypes,
    selections: &mut [WorkbookSelection],
    media_cache: &mut HashMap<String, MediaAsset>,
    remaining_payload_bytes: &mut usize,
    warnings: &mut Vec<String>,
) -> Result<(Vec<WorkbookPicture>, bool), ReadError> {
    let mut inventory = Vec::new();
    let mut inventory_truncated = false;
    for relationship_id in drawing_relationship_ids {
        let Some(relationship) = sheet_relationships
            .get(relationship_id)
            .filter(|relationship| {
                relationship.relationship_type.ends_with("/drawing") && !relationship.external
            })
        else {
            continue;
        };
        let drawing_part = resolve_part_target(sheet_part, &relationship.target)?;
        let drawing_xml = package.read_required_xml(&drawing_part)?;
        let drawing_relationships = package
            .read_xml(&part_relationship_path(&drawing_part)?)?
            .map(|xml| parse_part_relationships(&xml))
            .transpose()?
            .unwrap_or_default();
        let drawing_pictures =
            parse_drawing_pictures(&drawing_xml, &drawing_part, &drawing_relationships)?;
        for drawing_picture in drawing_pictures {
            let Some(from_cell) = drawing_marker_cell(drawing_picture.from) else {
                push_unique_warning(
                    warnings,
                    format!("worksheet `{sheet_name}` has a picture with an invalid from marker"),
                );
                continue;
            };
            let to_cell = drawing_picture.to.and_then(drawing_marker_cell);
            let inventory_slot = inventory.len() < MAX_IMAGE_REFERENCES;
            if !inventory_slot {
                inventory_truncated = true;
            }
            let mut selection_indexes = Vec::new();
            for (index, selection) in selections.iter_mut().enumerate() {
                if selection.sheet != sheet_name
                    || !drawing_picture_intersects_selection(&drawing_picture, selection.bounds)
                {
                    continue;
                }
                if selection.images.len() >= MAX_IMAGE_REFERENCES {
                    selection.images_truncated = true;
                    push_unique_warning(
                        warnings,
                        format!(
                            "worksheet image inventory was truncated at {MAX_IMAGE_REFERENCES} pictures per selection"
                        ),
                    );
                } else {
                    selection_indexes.push(index);
                }
            }
            if !inventory_slot && selection_indexes.is_empty() {
                continue;
            }
            if !media_cache.contains_key(&drawing_picture.media_part) {
                let bytes = package
                    .read_bytes(&drawing_picture.media_part)?
                    .ok_or_else(|| {
                        ReadError::InvalidXlsx(format!(
                            "drawing image part `{}` is missing",
                            drawing_picture.media_part
                        ))
                    })?;
                let content_type = content_types
                    .for_part(&drawing_picture.media_part)
                    .unwrap_or_else(|| "application/octet-stream".to_owned());
                let sha256 = hex_lower(digest(&SHA256, &bytes).as_ref());
                media_cache.insert(
                    drawing_picture.media_part.clone(),
                    MediaAsset {
                        content_type,
                        bytes,
                        sha256,
                    },
                );
            }
            let asset = media_cache
                .get(&drawing_picture.media_part)
                .expect("media asset was inserted");
            let picture = WorkbookPicture {
                sheet: sheet_name.to_owned(),
                from_cell,
                to_cell,
                from_row_index: drawing_picture.from.row.unwrap_or_default(),
                from_column_index: drawing_picture.from.column.unwrap_or_default(),
                to_row_index: drawing_picture.to.and_then(|marker| marker.row),
                to_column_index: drawing_picture.to.and_then(|marker| marker.column),
                media_part: drawing_picture.media_part,
                content_type: asset.content_type.clone(),
                byte_size: asset.bytes.len(),
                sha256: asset.sha256.clone(),
                data_uri: None,
                payload_truncated: false,
            };
            if inventory_slot {
                inventory.push(picture.clone());
            }
            for index in selection_indexes {
                let selection = &mut selections[index];
                let mut selected_picture = picture.clone();
                if asset.bytes.len() <= MAX_IMAGE_PAYLOAD_BYTES
                    && asset.bytes.len() <= *remaining_payload_bytes
                {
                    selected_picture.data_uri = Some(format!(
                        "data:{};base64,{}",
                        asset.content_type,
                        encode_base64(&asset.bytes)
                    ));
                    *remaining_payload_bytes -= asset.bytes.len();
                } else {
                    selected_picture.payload_truncated = true;
                    selection.images_truncated = true;
                    push_unique_warning(
                        warnings,
                        format!(
                            "worksheet image payloads are limited to {MAX_IMAGE_PAYLOAD_BYTES} bytes per image and {MAX_TOTAL_IMAGE_PAYLOAD_BYTES} bytes total; omitted payloads remain available as metadata"
                        ),
                    );
                }
                selection.images.push(selected_picture);
            }
        }
    }
    if inventory_truncated {
        push_unique_warning(
            warnings,
            format!(
                "worksheet picture inventory was truncated at {MAX_IMAGE_REFERENCES} pictures per scanned sheet"
            ),
        );
    }
    Ok((inventory, inventory_truncated))
}

fn drawing_marker_cell(marker: DrawingMarker) -> Option<String> {
    let row = marker.row?.checked_add(1)?;
    let column = marker.column?.checked_add(1)?;
    if row > 1_048_576 || column > 16_384 {
        return None;
    }
    Some(format!(
        "{}{}",
        column_name(u16::try_from(column).ok()?),
        row
    ))
}

fn drawing_picture_intersects_selection(picture: &DrawingPicture, bounds: SelectionBounds) -> bool {
    drawing_anchor_intersects_selection(picture.from, picture.to, bounds)
}

fn drawing_anchor_intersects_selection(
    from: DrawingMarker,
    to: Option<DrawingMarker>,
    bounds: SelectionBounds,
) -> bool {
    let Some(from_row) = from.row.map(|value| value.saturating_add(1)) else {
        return false;
    };
    let Some(from_column) = from.column.map(|value| value.saturating_add(1)) else {
        return false;
    };
    let to_row = to
        .and_then(|marker| marker.row)
        .unwrap_or(from_row - 1)
        .saturating_add(1);
    let to_column = to
        .and_then(|marker| marker.column)
        .unwrap_or(from_column - 1)
        .saturating_add(1);
    let start_row = from_row.min(to_row);
    let end_row = from_row.max(to_row);
    let start_column = from_column.min(to_column);
    let end_column = from_column.max(to_column);
    start_row <= bounds.end_row
        && end_row >= bounds.start_row
        && start_column <= u32::from(bounds.end_column)
        && end_column >= u32::from(bounds.start_column)
}

fn encode_base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        output.push(char::from(TABLE[usize::from(first >> 2)]));
        output.push(char::from(
            TABLE[usize::from(((first & 0x03) << 4) | (second >> 4))],
        ));
        if chunk.len() > 1 {
            output.push(char::from(
                TABLE[usize::from(((second & 0x0f) << 2) | (third >> 6))],
            ));
        } else {
            output.push('=');
        }
        if chunk.len() > 2 {
            output.push(char::from(TABLE[usize::from(third & 0x3f)]));
        } else {
            output.push('=');
        }
    }
    output
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn push_unique_warning(warnings: &mut Vec<String>, warning: String) {
    if !warnings.iter().any(|existing| existing == &warning) {
        warnings.push(warning);
    }
}

fn parse_workbook(
    xml: &str,
    relationships: &HashMap<String, String>,
) -> Result<ParsedWorkbook, ReadError> {
    let mut reader = xml_reader(xml);
    let mut date_system = DateSystem::Excel1900;
    let mut sheets = Vec::new();
    let mut defined_names = Vec::new();
    let mut current_defined_name: Option<(String, Option<usize>, String)> = None;
    loop {
        match reader.read_event().map_err(xml_error)? {
            Event::Start(element) | Event::Empty(element)
                if local_name(element.name().as_ref()) == b"workbookPr" =>
            {
                if attribute(&element, b"date1904")?
                    .as_deref()
                    .is_some_and(xml_truthy)
                {
                    date_system = DateSystem::Excel1904;
                }
            }
            Event::Start(element) | Event::Empty(element)
                if local_name(element.name().as_ref()) == b"sheet" =>
            {
                let name = attribute(&element, b"name")?.ok_or_else(|| {
                    ReadError::InvalidXlsx("worksheet entry has no name".to_owned())
                })?;
                let relationship_id = attribute_exact(&element, b"r:id")?.ok_or_else(|| {
                    ReadError::InvalidXlsx(format!("worksheet `{name}` has no relationship id"))
                })?;
                let path = relationships
                    .get(&relationship_id)
                    .cloned()
                    .ok_or_else(|| {
                        ReadError::InvalidXlsx(format!(
                            "worksheet `{name}` relationship target is missing"
                        ))
                    })?;
                let state = match attribute(&element, b"state")?.as_deref() {
                    Some("hidden") => SheetState::Hidden,
                    Some("veryHidden") => SheetState::VeryHidden,
                    _ => SheetState::Visible,
                };
                sheets.push(SheetDescriptor { name, state, path });
            }
            Event::Start(element) if local_name(element.name().as_ref()) == b"definedName" => {
                let name = attribute(&element, b"name")?.unwrap_or_default();
                let local_sheet_index =
                    attribute(&element, b"localSheetId")?.and_then(|value| value.parse().ok());
                current_defined_name = Some((name, local_sheet_index, String::new()));
            }
            Event::Text(text) if current_defined_name.is_some() => {
                if let Some((_, _, reference)) = current_defined_name.as_mut() {
                    reference.push_str(&text.decode().map_err(encoding_error)?);
                }
            }
            Event::GeneralRef(entity) if current_defined_name.is_some() => {
                if let Some((_, _, reference)) = current_defined_name.as_mut() {
                    reference.push_str(&decode_reference(&entity)?);
                }
            }
            Event::End(element) if local_name(element.name().as_ref()) == b"definedName" => {
                if let Some((name, local_sheet_index, reference)) = current_defined_name.take() {
                    defined_names.push(DefinedName {
                        name,
                        local_sheet_index,
                        valid_reference: !reference.contains("#REF!"),
                        reference,
                    });
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(ParsedWorkbook {
        date_system,
        sheets,
        defined_names,
    })
}

fn print_defined_name(
    defined_names: &[DefinedName],
    sheet_index: usize,
    name: &str,
) -> Option<String> {
    defined_names
        .iter()
        .find(|defined_name| {
            defined_name.name == name && defined_name.local_sheet_index == Some(sheet_index)
        })
        .map(|defined_name| defined_name.reference.clone())
}

fn apply_worksheet_print_scan(target: &mut WorksheetPrintEvidence, scan: WorksheetPrintEvidence) {
    target.page_setup = scan.page_setup;
    target.print_options = scan.print_options;
    target.row_breaks = scan.row_breaks;
    target.column_breaks = scan.column_breaks;
    target.header_footer = scan.header_footer;
}

fn parse_shared_strings(xml: &str) -> Result<Vec<SharedString>, ReadError> {
    let mut reader = xml_reader(xml);
    let mut strings = Vec::new();
    let mut current: Option<String> = None;
    let mut rich = false;
    let mut runs = Vec::new();
    let mut run: Option<WorkbookRichTextRun> = None;
    let mut inside_run_properties = false;
    let mut inside_text = false;
    let mut phonetic_depth = 0usize;
    loop {
        match reader.read_event().map_err(xml_error)? {
            Event::Start(element) if local_name(element.name().as_ref()) == b"si" => {
                current = Some(String::new());
                rich = false;
                runs.clear();
            }
            Event::Start(element) if local_name(element.name().as_ref()) == b"r" => {
                if current.is_some() {
                    rich = true;
                    run = Some(WorkbookRichTextRun::default());
                }
            }
            Event::Start(element) if local_name(element.name().as_ref()) == b"rPr" => {
                inside_run_properties = true;
            }
            Event::Start(element) | Event::Empty(element)
                if inside_run_properties && local_name(element.name().as_ref()) == b"strike" =>
            {
                if let Some(run) = run.as_mut() {
                    run.strike = Some(
                        attribute(&element, b"val")?
                            .as_deref()
                            .is_none_or(xml_truthy),
                    );
                }
            }
            Event::Start(element) | Event::Empty(element)
                if inside_run_properties && local_name(element.name().as_ref()) == b"color" =>
            {
                if let Some(run) = run.as_mut() {
                    run.font_color = parse_font_color(&element)?;
                }
            }
            Event::Start(element) if local_name(element.name().as_ref()) == b"rPh" => {
                phonetic_depth += 1;
            }
            Event::Start(element)
                if local_name(element.name().as_ref()) == b"t" && phonetic_depth == 0 =>
            {
                inside_text = true;
            }
            Event::Text(text) if inside_text => {
                if let Some(value) = current.as_mut() {
                    let decoded = text.decode().map_err(encoding_error)?;
                    push_bounded(value, &decoded);
                    if let Some(run) = run.as_mut() {
                        push_bounded(&mut run.text, &decoded);
                    }
                }
            }
            Event::GeneralRef(entity) if inside_text => {
                if let Some(value) = current.as_mut() {
                    push_bounded(value, &decode_reference(&entity)?);
                    if let Some(run) = run.as_mut() {
                        push_bounded(&mut run.text, &decode_reference(&entity)?);
                    }
                }
            }
            Event::End(element) if local_name(element.name().as_ref()) == b"rPr" => {
                inside_run_properties = false;
            }
            Event::End(element) if local_name(element.name().as_ref()) == b"r" => {
                if let Some(run) = run.take() {
                    runs.push(run);
                }
            }
            Event::End(element) if local_name(element.name().as_ref()) == b"t" => {
                inside_text = false;
            }
            Event::End(element) if local_name(element.name().as_ref()) == b"rPh" => {
                phonetic_depth = phonetic_depth.saturating_sub(1);
            }
            Event::End(element) if local_name(element.name().as_ref()) == b"si" => {
                strings.push(SharedString {
                    text: current.take().unwrap_or_default(),
                    rich,
                    runs: std::mem::take(&mut runs),
                });
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(strings)
}

fn parse_styles(xml: &str) -> Result<Styles, ReadError> {
    let mut reader = xml_reader(xml);
    let mut styles = Styles::default();
    let mut inside_cell_xfs = false;
    let mut inside_fonts = false;
    let mut font: Option<FontFormat> = None;
    loop {
        match reader.read_event().map_err(xml_error)? {
            Event::Start(element) | Event::Empty(element)
                if local_name(element.name().as_ref()) == b"numFmt" =>
            {
                if let (Some(id), Some(code)) = (
                    attribute(&element, b"numFmtId")?.and_then(|value| value.parse().ok()),
                    attribute(&element, b"formatCode")?,
                ) {
                    styles.custom_formats.insert(id, code);
                }
            }
            Event::Start(element) if local_name(element.name().as_ref()) == b"cellXfs" => {
                inside_cell_xfs = true;
            }
            Event::Start(element) if local_name(element.name().as_ref()) == b"fonts" => {
                inside_fonts = true;
            }
            Event::End(element) if local_name(element.name().as_ref()) == b"fonts" => {
                inside_fonts = false;
            }
            Event::Start(element)
                if inside_fonts && local_name(element.name().as_ref()) == b"font" =>
            {
                font = Some(FontFormat::default());
            }
            Event::Empty(element)
                if inside_fonts && local_name(element.name().as_ref()) == b"font" =>
            {
                styles.fonts.push(FontFormat::default());
            }
            Event::Start(element) | Event::Empty(element)
                if font.is_some() && local_name(element.name().as_ref()) == b"strike" =>
            {
                font.as_mut().unwrap().strike = Some(
                    attribute(&element, b"val")?
                        .as_deref()
                        .is_none_or(xml_truthy),
                );
            }
            Event::Start(element) | Event::Empty(element)
                if font.is_some() && local_name(element.name().as_ref()) == b"color" =>
            {
                font.as_mut().unwrap().color = parse_font_color(&element)?;
            }
            Event::End(element) if local_name(element.name().as_ref()) == b"font" => {
                if let Some(font) = font.take() {
                    styles.fonts.push(font);
                }
            }
            Event::End(element) if local_name(element.name().as_ref()) == b"cellXfs" => {
                inside_cell_xfs = false;
            }
            Event::Start(element) | Event::Empty(element)
                if inside_cell_xfs && local_name(element.name().as_ref()) == b"xf" =>
            {
                let id = attribute(&element, b"numFmtId")?
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(0);
                styles.cell_formats.push(id);
                let apply_font = attribute(&element, b"applyFont")?
                    .as_deref()
                    .map(xml_truthy);
                let font_index =
                    attribute(&element, b"fontId")?.and_then(|value| value.parse().ok());
                styles
                    .cell_font_indexes
                    .push((apply_font != Some(false)).then_some(font_index).flatten());
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(styles)
}

fn parse_font_color(element: &BytesStart<'_>) -> Result<Option<WorkbookFontColor>, ReadError> {
    let color = WorkbookFontColor {
        rgb: attribute(element, b"rgb")?.map(|value| value.to_ascii_uppercase()),
        theme: attribute(element, b"theme")?.and_then(|value| value.parse().ok()),
        indexed: attribute(element, b"indexed")?.and_then(|value| value.parse().ok()),
        auto: attribute(element, b"auto")?.as_deref().map(xml_truthy),
        tint: attribute(element, b"tint")?.and_then(|value| value.parse().ok()),
        resolved_rgb: None,
    };
    Ok((color.rgb.is_some()
        || color.theme.is_some()
        || color.indexed.is_some()
        || color.auto.is_some()
        || color.tint.is_some())
    .then_some(color))
}

fn parse_theme_colors(xml: &str) -> Result<Vec<Option<String>>, ReadError> {
    let mut reader = xml_reader(xml);
    let mut colors = vec![None; 12];
    let mut inside_scheme = false;
    let mut slot = None;
    loop {
        match reader.read_event().map_err(xml_error)? {
            Event::Start(element) if local_name(element.name().as_ref()) == b"clrScheme" => {
                inside_scheme = true;
            }
            Event::End(element) if local_name(element.name().as_ref()) == b"clrScheme" => break,
            Event::Start(element) if inside_scheme && slot.is_none() => {
                slot = theme_color_index(local_name(element.name().as_ref()));
            }
            Event::Start(element) | Event::Empty(element)
                if inside_scheme
                    && slot.is_some()
                    && matches!(local_name(element.name().as_ref()), b"srgbClr" | b"sysClr") =>
            {
                let value = if local_name(element.name().as_ref()) == b"sysClr" {
                    attribute(&element, b"val")?
                        .filter(|value| is_rgb_hex(value))
                        .or(attribute(&element, b"lastClr")?)
                } else {
                    attribute(&element, b"val")?
                };
                if let (Some(index), Some(value)) = (slot, value) {
                    colors[index] = Some(value.to_ascii_uppercase());
                }
            }
            Event::End(element)
                if inside_scheme
                    && theme_color_index(local_name(element.name().as_ref())).is_some() =>
            {
                slot = None;
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(colors)
}

fn theme_color_index(name: &[u8]) -> Option<usize> {
    match name {
        // SpreadsheetML theme indexes deliberately swap the first two pairs
        // from DrawingML clrScheme order: 0=lt1, 1=dk1, 2=lt2, 3=dk2.
        b"lt1" => Some(0),
        b"dk1" => Some(1),
        b"lt2" => Some(2),
        b"dk2" => Some(3),
        b"accent1" => Some(4),
        b"accent2" => Some(5),
        b"accent3" => Some(6),
        b"accent4" => Some(7),
        b"accent5" => Some(8),
        b"accent6" => Some(9),
        b"hlink" => Some(10),
        b"folHlink" => Some(11),
        _ => None,
    }
}

fn is_rgb_hex(value: &str) -> bool {
    value.len() == 6 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn resolve_published_colors(
    styles: &mut Styles,
    shared_strings: &mut [SharedString],
    theme: &[Option<String>],
) {
    styles.theme = theme.to_vec();
    for font in &mut styles.fonts {
        if let Some(color) = &mut font.color {
            color.resolved_rgb = resolve_color(color, theme);
        }
    }
    for item in shared_strings {
        for run in &mut item.runs {
            if let Some(color) = &mut run.font_color {
                color.resolved_rgb = resolve_color(color, theme);
            }
        }
    }
}

fn resolve_color(color: &WorkbookFontColor, theme: &[Option<String>]) -> Option<String> {
    if color.auto == Some(true) {
        return None;
    }
    let base = if let Some(rgb) = &color.rgb {
        (rgb.len() >= 6).then(|| rgb[rgb.len() - 6..].to_ascii_uppercase())
    } else if let Some(index) = color.theme {
        theme.get(index as usize).cloned().flatten()
    } else if let Some(index) = color.indexed {
        indexed_color(index).map(str::to_owned)
    } else {
        None
    }?;
    apply_tint(&base, color.tint.unwrap_or(0.0))
}

fn indexed_color(index: u32) -> Option<&'static str> {
    const COLORS: [&str; 64] = [
        "000000", "FFFFFF", "FF0000", "00FF00", "0000FF", "FFFF00", "FF00FF", "00FFFF", "000000",
        "FFFFFF", "FF0000", "00FF00", "0000FF", "FFFF00", "FF00FF", "00FFFF", "800000", "008000",
        "000080", "808000", "800080", "008080", "C0C0C0", "808080", "9999FF", "993366", "FFFFCC",
        "CCFFFF", "660066", "FF8080", "0066CC", "CCCCFF", "000080", "FF00FF", "FFFF00", "00FFFF",
        "800080", "800000", "008080", "0000FF", "00CCFF", "CCFFFF", "CCFFCC", "FFFF99", "99CCFF",
        "FF99CC", "CC99FF", "FFCC99", "3366FF", "33CCCC", "99CC00", "FFCC00", "FF9900", "FF6600",
        "666699", "969696", "003366", "339966", "003300", "333300", "993300", "993366", "333399",
        "333333",
    ];
    COLORS.get(index as usize).copied()
}

fn apply_tint(rgb: &str, tint: f64) -> Option<String> {
    if rgb.len() != 6 || !(-1.0..=1.0).contains(&tint) {
        return None;
    }
    let r = u8::from_str_radix(&rgb[0..2], 16).ok()? as f64 / 255.0;
    let g = u8::from_str_radix(&rgb[2..4], 16).ok()? as f64 / 255.0;
    let b = u8::from_str_radix(&rgb[4..6], 16).ok()? as f64 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let mut h = 0.0;
    let l = (max + min) / 2.0;
    let d = max - min;
    let s = if d == 0.0 {
        0.0
    } else {
        d / (1.0 - (2.0 * l - 1.0).abs())
    };
    if d != 0.0 {
        h = if max == r {
            ((g - b) / d) % 6.0
        } else if max == g {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        } / 6.0;
        if h < 0.0 {
            h += 1.0;
        }
    }
    let l = if tint < 0.0 {
        l * (1.0 + tint)
    } else {
        l * (1.0 - tint) + tint
    };
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - (((h * 6.0) % 2.0) - 1.0).abs());
    let m = l - c / 2.0;
    let (r1, g1, b1) = match (h * 6.0).floor() as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    Some(format!(
        "{:02X}{:02X}{:02X}",
        ((r1 + m) * 255.0).round() as u8,
        ((g1 + m) * 255.0).round() as u8,
        ((b1 + m) * 255.0).round() as u8
    ))
}

#[allow(clippy::too_many_arguments)]
fn parse_worksheet<R: BufRead>(
    input: R,
    plans: &[SelectionPlan],
    selections: &mut [WorkbookSelection],
    shared_strings: &[SharedString],
    styles: &Styles,
    date_system: DateSystem,
    options: &SpreadsheetReadOptions,
    relationships: &HashMap<String, PartRelationship>,
    remaining_cells: &mut usize,
    warnings: &mut Vec<String>,
) -> Result<SheetScan, ReadError> {
    let mut reader = Reader::from_reader(input);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut scan = SheetScan::default();
    scan.features.scanned = true;
    scan.features.cell_data_complete = true;
    let mut cell: Option<CellBuilder> = None;
    let allow_cell_skip = !options.ranges.is_empty();
    let max_requested_row = plans.iter().map(|plan| plan.bounds.end_row).max();
    let mut selection_extents = vec![None; selections.len()];
    let mut inside_sheet_data = false;
    let mut inside_row_breaks = false;
    let mut inside_column_breaks = false;
    let mut phonetic_depth = 0usize;
    loop {
        match reader.read_event_into(&mut buffer).map_err(xml_error)? {
            Event::Start(element) if local_name(element.name().as_ref()) == b"c" => {
                scan.cell_elements += 1;
                cell = Some(CellBuilder {
                    reference: attribute(&element, b"r")?.unwrap_or_default(),
                    cell_type: attribute(&element, b"t")?,
                    style_index: attribute(&element, b"s")?.and_then(|value| value.parse().ok()),
                    ..CellBuilder::default()
                });
            }
            Event::Empty(element) if local_name(element.name().as_ref()) == b"c" => {
                scan.cell_elements += 1;
                scan.style_only_cells += 1;
            }
            Event::Start(element) if local_name(element.name().as_ref()) == b"v" => {
                if let Some(cell) = cell.as_mut() {
                    cell.capture_value = true;
                    cell.has_value = true;
                }
            }
            Event::Start(element) if local_name(element.name().as_ref()) == b"f" => {
                if let Some(cell) = cell.as_mut() {
                    cell.capture_formula = true;
                    cell.has_formula = true;
                    capture_formula_metadata(cell, &element)?;
                    scan.features.formula_cells += 1;
                }
            }
            Event::Empty(element) if local_name(element.name().as_ref()) == b"f" => {
                if let Some(cell) = cell.as_mut() {
                    cell.has_formula = true;
                    capture_formula_metadata(cell, &element)?;
                    scan.features.formula_cells += 1;
                }
            }
            Event::Start(element) if local_name(element.name().as_ref()) == b"t" => {
                if let Some(cell) = cell.as_mut()
                    && cell.cell_type.as_deref() == Some("inlineStr")
                {
                    cell.capture_inline_text = true;
                    cell.has_inline = true;
                }
            }
            Event::Start(element) if local_name(element.name().as_ref()) == b"r" => {
                if let Some(cell) = cell.as_mut()
                    && cell.cell_type.as_deref() == Some("inlineStr")
                {
                    cell.rich_text = true;
                    cell.current_run = Some(WorkbookRichTextRun::default());
                }
            }
            Event::Start(element) if local_name(element.name().as_ref()) == b"rPr" => {
                if let Some(cell) = cell.as_mut()
                    && cell.current_run.is_some()
                {
                    cell.inside_run_properties = true;
                }
            }
            Event::Start(element) | Event::Empty(element)
                if local_name(element.name().as_ref()) == b"strike" =>
            {
                if let Some(cell) = cell.as_mut()
                    && cell.inside_run_properties
                    && let Some(run) = cell.current_run.as_mut()
                {
                    run.strike = Some(
                        attribute(&element, b"val")?
                            .as_deref()
                            .is_none_or(xml_truthy),
                    );
                }
            }
            Event::Start(element) | Event::Empty(element)
                if local_name(element.name().as_ref()) == b"color" =>
            {
                if let Some(cell) = cell.as_mut()
                    && cell.inside_run_properties
                    && let Some(run) = cell.current_run.as_mut()
                {
                    run.font_color = parse_font_color(&element)?;
                }
            }
            Event::Start(element) if local_name(element.name().as_ref()) == b"rPh" => {
                phonetic_depth += 1;
            }
            Event::Text(text) if phonetic_depth == 0 => {
                let text = text.decode().map_err(encoding_error)?;
                if let Some(cell) = cell.as_mut() {
                    if cell.capture_value {
                        push_bounded(&mut cell.value, &text);
                    } else if cell.capture_formula {
                        push_bounded(&mut cell.formula, &text);
                    } else if cell.capture_inline_text {
                        push_bounded(&mut cell.inline, &text);
                        if let Some(run) = cell.current_run.as_mut() {
                            push_bounded(&mut run.text, &text);
                        }
                    }
                }
            }
            Event::GeneralRef(entity) if phonetic_depth == 0 => {
                let text = decode_reference(&entity)?;
                if let Some(cell) = cell.as_mut() {
                    if cell.capture_value {
                        push_bounded(&mut cell.value, &text);
                    } else if cell.capture_formula {
                        push_bounded(&mut cell.formula, &text);
                    } else if cell.capture_inline_text {
                        push_bounded(&mut cell.inline, &text);
                        if let Some(run) = cell.current_run.as_mut() {
                            push_bounded(&mut run.text, &text);
                        }
                    }
                }
            }
            Event::End(element) if local_name(element.name().as_ref()) == b"v" => {
                if let Some(cell) = cell.as_mut() {
                    cell.capture_value = false;
                }
            }
            Event::End(element) if local_name(element.name().as_ref()) == b"f" => {
                if let Some(cell) = cell.as_mut() {
                    cell.capture_formula = false;
                }
            }
            Event::End(element) if local_name(element.name().as_ref()) == b"t" => {
                if let Some(cell) = cell.as_mut() {
                    cell.capture_inline_text = false;
                }
            }
            Event::End(element) if local_name(element.name().as_ref()) == b"rPh" => {
                phonetic_depth = phonetic_depth.saturating_sub(1);
            }
            Event::End(element) if local_name(element.name().as_ref()) == b"rPr" => {
                if let Some(cell) = cell.as_mut() {
                    cell.inside_run_properties = false;
                }
            }
            Event::End(element) if local_name(element.name().as_ref()) == b"r" => {
                if let Some(cell) = cell.as_mut()
                    && let Some(run) = cell.current_run.take()
                {
                    cell.rich_text_runs.push(run);
                }
            }
            Event::End(element) if local_name(element.name().as_ref()) == b"c" => {
                if let Some(builder) = cell.take() {
                    if let Some(parsed) = finish_cell(
                        builder,
                        shared_strings,
                        styles,
                        date_system,
                        options.include_formulas,
                        warnings,
                    )? {
                        scan.non_empty_cells += 1;
                        update_semantic_bounds(&mut scan.semantic, parsed.row, parsed.column);
                        for plan in plans {
                            record_selection_extent(
                                &mut selection_extents[plan.output_index],
                                &mut selections[plan.output_index].overflow,
                                plan.bounds,
                                parsed.row,
                                parsed.column,
                            );
                            if contains(plan.bounds, parsed.row, parsed.column) {
                                let selection = &mut selections[plan.output_index];
                                if *remaining_cells > 0 {
                                    selection.cells.push(parsed.clone());
                                    *remaining_cells -= 1;
                                } else {
                                    selection.truncated = true;
                                }
                            }
                        }
                    } else {
                        scan.style_only_cells += 1;
                    }
                }
            }
            Event::Start(element) | Event::Empty(element)
                if local_name(element.name().as_ref()) == b"dimension" =>
            {
                scan.dimension = attribute(&element, b"ref")?;
            }
            Event::Start(element) | Event::Empty(element)
                if local_name(element.name().as_ref()) == b"mergeCell" =>
            {
                if let Some(reference) = attribute(&element, b"ref")? {
                    scan.merged_ranges.push(reference);
                }
            }
            Event::Start(element) if local_name(element.name().as_ref()) == b"sheetData" => {
                inside_sheet_data = true;
            }
            Event::End(element) if local_name(element.name().as_ref()) == b"sheetData" => {
                inside_sheet_data = false;
            }
            Event::Start(element) if local_name(element.name().as_ref()) == b"row" => {
                let row = record_row_metadata(&element, &mut scan)?;
                if inside_sheet_data
                    && allow_cell_skip
                    && let (Some(row), Some(max_row)) = (row, max_requested_row)
                    && row > max_row
                {
                    scan.features.cell_data_complete = false;
                    let mut skipped = Vec::new();
                    reader
                        .read_to_end_into(element.name(), &mut skipped)
                        .map_err(xml_error)?;
                }
            }
            Event::Empty(element) if local_name(element.name().as_ref()) == b"row" => {
                record_row_metadata(&element, &mut scan)?;
            }
            Event::Start(element) | Event::Empty(element)
                if local_name(element.name().as_ref()) == b"col" =>
            {
                if attribute(&element, b"hidden")?
                    .as_deref()
                    .is_some_and(xml_truthy)
                {
                    let min = attribute(&element, b"min")?
                        .and_then(|value| value.parse::<usize>().ok())
                        .unwrap_or(1);
                    let max = attribute(&element, b"max")?
                        .and_then(|value| value.parse::<usize>().ok())
                        .unwrap_or(min);
                    scan.hidden_columns += max.saturating_sub(min).saturating_add(1);
                }
                let min = attribute(&element, b"min")?
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(1);
                let max = attribute(&element, b"max")?
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(min);
                let outline_level = attribute(&element, b"outlineLevel")?
                    .and_then(|value| value.parse::<u8>().ok())
                    .unwrap_or(0);
                if outline_level > 0 {
                    scan.features.outlined_columns += max.saturating_sub(min).saturating_add(1);
                    scan.features.max_column_outline_level =
                        scan.features.max_column_outline_level.max(outline_level);
                }
            }
            Event::Start(element) | Event::Empty(element)
                if local_name(element.name().as_ref()) == b"hyperlink" =>
            {
                let relationship = attribute_exact(&element, b"r:id")?
                    .and_then(|id| relationships.get(&id))
                    .filter(|relationship| relationship.relationship_type.ends_with("/hyperlink"));
                let hyperlink = WorkbookHyperlink {
                    reference: attribute(&element, b"ref")?.unwrap_or_default(),
                    target: relationship.map(|relationship| relationship.target.clone()),
                    location: attribute(&element, b"location")?,
                    display: attribute(&element, b"display")?,
                    external: relationship.is_some_and(|relationship| relationship.external),
                };
                push_feature_reference(
                    &mut scan.features.hyperlinks,
                    hyperlink,
                    &mut scan.features.feature_references_truncated,
                );
            }
            Event::Start(element) | Event::Empty(element)
                if local_name(element.name().as_ref()) == b"autoFilter" =>
            {
                scan.features.auto_filter = attribute(&element, b"ref")?;
            }
            Event::Start(element) | Event::Empty(element)
                if local_name(element.name().as_ref()) == b"conditionalFormatting" =>
            {
                if let Some(reference) = attribute(&element, b"sqref")? {
                    push_feature_reference(
                        &mut scan.features.conditional_format_ranges,
                        reference,
                        &mut scan.features.feature_references_truncated,
                    );
                }
            }
            Event::Start(element) | Event::Empty(element)
                if local_name(element.name().as_ref()) == b"cfRule" =>
            {
                scan.features.conditional_format_rules += 1;
            }
            Event::Start(element) | Event::Empty(element)
                if local_name(element.name().as_ref()) == b"dataValidation" =>
            {
                scan.features.data_validation_rules += 1;
                if let Some(reference) = attribute(&element, b"sqref")? {
                    push_feature_reference(
                        &mut scan.features.data_validation_ranges,
                        reference,
                        &mut scan.features.feature_references_truncated,
                    );
                }
            }
            Event::Start(element) | Event::Empty(element)
                if local_name(element.name().as_ref()) == b"tablePart" =>
            {
                scan.features.table_parts += 1;
            }
            Event::Start(element) | Event::Empty(element)
                if local_name(element.name().as_ref()) == b"drawing" =>
            {
                scan.features.drawing_parts += 1;
                if let Some(relationship_id) = attribute_exact(&element, b"r:id")? {
                    push_feature_reference(
                        &mut scan.drawing_relationship_ids,
                        relationship_id,
                        &mut scan.features.feature_references_truncated,
                    );
                }
            }
            Event::Start(element) | Event::Empty(element)
                if local_name(element.name().as_ref()) == b"legacyDrawing" =>
            {
                scan.features.comment_drawing_parts += 1;
            }
            Event::Start(element) | Event::Empty(element)
                if local_name(element.name().as_ref()) == b"pageSetUpPr" =>
            {
                scan.features.page_setup = true;
                let page_setup = scan.print.page_setup.get_or_insert_default();
                page_setup.fit_to_page = attribute(&element, b"fitToPage")?
                    .as_deref()
                    .map(xml_truthy);
            }
            Event::Start(element) | Event::Empty(element)
                if local_name(element.name().as_ref()) == b"pageSetup" =>
            {
                scan.features.page_setup = true;
                let page_setup = scan.print.page_setup.get_or_insert_default();
                page_setup.orientation = attribute(&element, b"orientation")?;
                page_setup.paper_size =
                    attribute(&element, b"paperSize")?.and_then(|value| value.parse().ok());
                if let Some(value) = attribute(&element, b"fitToPage")? {
                    page_setup.fit_to_page = Some(xml_truthy(&value));
                }
                page_setup.fit_to_width =
                    attribute(&element, b"fitToWidth")?.and_then(|value| value.parse().ok());
                page_setup.fit_to_height =
                    attribute(&element, b"fitToHeight")?.and_then(|value| value.parse().ok());
                page_setup.scale =
                    attribute(&element, b"scale")?.and_then(|value| value.parse().ok());
            }
            Event::Start(element) | Event::Empty(element)
                if local_name(element.name().as_ref()) == b"printOptions" =>
            {
                scan.features.page_setup = true;
                scan.print.print_options = Some(WorksheetPrintOptions {
                    grid_lines: attribute(&element, b"gridLines")?
                        .as_deref()
                        .map(xml_truthy),
                    headings: attribute(&element, b"headings")?.as_deref().map(xml_truthy),
                });
            }
            Event::Start(element) | Event::Empty(element)
                if local_name(element.name().as_ref()) == b"pageMargins" =>
            {
                scan.features.page_setup = true;
            }
            Event::Start(element) | Event::Empty(element)
                if local_name(element.name().as_ref()) == b"headerFooter" =>
            {
                scan.features.header_footer = true;
                scan.print.header_footer = true;
            }
            Event::Start(element) if local_name(element.name().as_ref()) == b"rowBreaks" => {
                inside_row_breaks = true;
            }
            Event::End(element) if local_name(element.name().as_ref()) == b"rowBreaks" => {
                inside_row_breaks = false;
            }
            Event::Start(element) if local_name(element.name().as_ref()) == b"colBreaks" => {
                inside_column_breaks = true;
            }
            Event::End(element) if local_name(element.name().as_ref()) == b"colBreaks" => {
                inside_column_breaks = false;
            }
            Event::Start(element) | Event::Empty(element)
                if local_name(element.name().as_ref()) == b"brk" =>
            {
                if let Some(reference) =
                    attribute(&element, b"id")?.and_then(|value| value.parse::<u32>().ok())
                {
                    if inside_row_breaks {
                        push_feature_reference(
                            &mut scan.print.row_breaks,
                            reference,
                            &mut scan.features.feature_references_truncated,
                        );
                    } else if inside_column_breaks {
                        push_feature_reference(
                            &mut scan.print.column_breaks,
                            reference,
                            &mut scan.features.feature_references_truncated,
                        );
                    }
                }
            }
            Event::Start(element) | Event::Empty(element)
                if local_name(element.name().as_ref()) == b"sparkline" =>
            {
                scan.features.sparklines += 1;
            }
            Event::Start(element) | Event::Empty(element)
                if local_name(element.name().as_ref()) == b"control" =>
            {
                scan.features.controls += 1;
            }
            Event::Eof => {
                scan.features.tail_features_complete = true;
                break;
            }
            _ => {}
        }
        buffer.clear();
    }
    scan.features.complete =
        scan.features.cell_data_complete && scan.features.tail_features_complete;
    for plan in plans {
        selections[plan.output_index].used_bounds =
            selection_extents[plan.output_index].map(format_bounds);
    }
    publish_selection_merges(plans, selections, &scan.merged_ranges, remaining_cells);
    Ok(scan)
}

fn record_row_metadata(
    element: &BytesStart<'_>,
    scan: &mut SheetScan,
) -> Result<Option<u32>, ReadError> {
    let row = attribute(element, b"r")?.and_then(|value| value.parse().ok());
    if attribute(element, b"hidden")?
        .as_deref()
        .is_some_and(xml_truthy)
    {
        scan.hidden_rows += 1;
    }
    let outline_level = attribute(element, b"outlineLevel")?
        .and_then(|value| value.parse::<u8>().ok())
        .unwrap_or(0);
    if outline_level > 0 {
        scan.features.outlined_rows += 1;
        scan.features.max_row_outline_level =
            scan.features.max_row_outline_level.max(outline_level);
    }
    Ok(row)
}

fn record_selection_extent(
    used_bounds: &mut Option<SelectionBounds>,
    overflow: &mut WorkbookSelectionOverflow,
    requested: SelectionBounds,
    row: u32,
    column: u16,
) {
    if (requested.start_row..=requested.end_row).contains(&row) {
        update_semantic_bounds(used_bounds, row, column);
        if column < requested.start_column {
            update_column_overflow(&mut overflow.left, column);
        } else if column > requested.end_column {
            update_column_overflow(&mut overflow.right, column);
        }
    } else if (requested.start_column..=requested.end_column).contains(&column) {
        if row < requested.start_row {
            update_row_overflow(&mut overflow.above, row);
        } else if row > requested.end_row {
            update_row_overflow(&mut overflow.below, row);
        }
    }
}

fn update_column_overflow(overflow: &mut Option<WorkbookColumnOverflow>, column: u16) {
    if let Some(overflow) = overflow {
        overflow.min_column = overflow.min_column.min(column);
        overflow.max_column = overflow.max_column.max(column);
        overflow.cell_count += 1;
    } else {
        *overflow = Some(WorkbookColumnOverflow {
            min_column: column,
            max_column: column,
            cell_count: 1,
        });
    }
}

fn update_row_overflow(overflow: &mut Option<WorkbookRowOverflow>, row: u32) {
    if let Some(overflow) = overflow {
        overflow.min_row = overflow.min_row.min(row);
        overflow.max_row = overflow.max_row.max(row);
        overflow.cell_count += 1;
    } else {
        *overflow = Some(WorkbookRowOverflow {
            min_row: row,
            max_row: row,
            cell_count: 1,
        });
    }
}

fn publish_selection_merges(
    plans: &[SelectionPlan],
    selections: &mut [WorkbookSelection],
    merged_ranges: &[String],
    remaining_cells: &mut usize,
) {
    let parsed_merges = merged_ranges
        .iter()
        .filter_map(|reference| parse_range_reference(reference).map(|bounds| (reference, bounds)))
        .collect::<Vec<_>>();
    for plan in plans {
        let selection = &mut selections[plan.output_index];
        let intersecting = parsed_merges
            .iter()
            .filter(|(_, bounds)| bounds_intersect(plan.bounds, *bounds))
            .copied()
            .collect::<Vec<_>>();
        selection.merged_ranges = intersecting
            .iter()
            .map(|(reference, _)| (*reference).clone())
            .collect();
        let mut existing = selection
            .cells
            .iter()
            .map(|cell| (cell.row, cell.column))
            .collect::<HashSet<_>>();
        for (reference, merge_bounds) in intersecting {
            let anchor = format!(
                "{}{}",
                column_name(merge_bounds.start_column),
                merge_bounds.start_row
            );
            for cell in &mut selection.cells {
                if contains(merge_bounds, cell.row, cell.column) && cell.merge.is_none() {
                    cell.merge = Some(merge_membership(
                        reference,
                        &anchor,
                        merge_bounds,
                        cell.row,
                        cell.column,
                    ));
                }
            }
            let intersection = intersect_bounds(plan.bounds, merge_bounds);
            'rows: for row in intersection.start_row..=intersection.end_row {
                for column in intersection.start_column..=intersection.end_column {
                    if existing.contains(&(row, column)) {
                        continue;
                    }
                    if *remaining_cells == 0 {
                        selection.truncated = true;
                        break 'rows;
                    }
                    selection.cells.push(blank_merge_cell(
                        row,
                        column,
                        merge_membership(reference, &anchor, merge_bounds, row, column),
                    ));
                    existing.insert((row, column));
                    *remaining_cells -= 1;
                }
            }
        }
        selection.cells.sort_by_key(|cell| (cell.row, cell.column));
    }
}

fn merge_membership(
    range: &str,
    anchor: &str,
    bounds: SelectionBounds,
    row: u32,
    column: u16,
) -> WorkbookMergeMembership {
    WorkbookMergeMembership {
        range: range.to_owned(),
        anchor: anchor.to_owned(),
        role: if row == bounds.start_row && column == bounds.start_column {
            WorkbookMergeRole::Anchor
        } else {
            WorkbookMergeRole::Covered
        },
    }
}

fn blank_merge_cell(row: u32, column: u16, merge: WorkbookMergeMembership) -> WorkbookCell {
    WorkbookCell {
        reference: format!("{}{row}", column_name(column)),
        row,
        column,
        value_type: CellValueType::Blank,
        value: String::new(),
        display: String::new(),
        formula: None,
        formula_kind: None,
        formula_reference: None,
        shared_formula_index: None,
        rich_text: false,
        font_strike: None,
        font_color: None,
        rich_text_runs: Vec::new(),
        merge: Some(merge),
        style_index: None,
        number_format: None,
    }
}

fn parse_range_reference(reference: &str) -> Option<SelectionBounds> {
    let (start, end) = reference
        .split_once(':')
        .map_or((reference, reference), |parts| parts);
    let (start_row, start_column) = parse_cell_reference(start)?;
    let (end_row, end_column) = parse_cell_reference(end)?;
    (start_row <= end_row && start_column <= end_column).then_some(SelectionBounds {
        start_row,
        start_column,
        end_row,
        end_column,
    })
}

fn bounds_intersect(left: SelectionBounds, right: SelectionBounds) -> bool {
    left.start_row <= right.end_row
        && right.start_row <= left.end_row
        && left.start_column <= right.end_column
        && right.start_column <= left.end_column
}

fn intersect_bounds(left: SelectionBounds, right: SelectionBounds) -> SelectionBounds {
    SelectionBounds {
        start_row: left.start_row.max(right.start_row),
        start_column: left.start_column.max(right.start_column),
        end_row: left.end_row.min(right.end_row),
        end_column: left.end_column.min(right.end_column),
    }
}

fn capture_formula_metadata(
    cell: &mut CellBuilder,
    element: &BytesStart<'_>,
) -> Result<(), ReadError> {
    cell.formula_kind = Some(match attribute(element, b"t")?.as_deref() {
        None | Some("normal") => FormulaKind::Normal,
        Some("shared") => FormulaKind::Shared,
        Some("array") => FormulaKind::Array,
        Some("dataTable") => FormulaKind::DataTable,
        Some(_) => FormulaKind::Other,
    });
    cell.formula_reference = attribute(element, b"ref")?;
    cell.shared_formula_index = attribute(element, b"si")?.and_then(|value| value.parse().ok());
    Ok(())
}

fn push_feature_reference<T>(target: &mut Vec<T>, value: T, truncated: &mut bool) {
    if target.len() < MAX_FEATURE_REFERENCES {
        target.push(value);
    } else {
        *truncated = true;
    }
}

fn finish_cell(
    cell: CellBuilder,
    shared_strings: &[SharedString],
    styles: &Styles,
    date_system: DateSystem,
    include_formulas: bool,
    warnings: &mut Vec<String>,
) -> Result<Option<WorkbookCell>, ReadError> {
    if !cell.has_value && !cell.has_inline && !cell.has_formula {
        return Ok(None);
    }
    let (row, column) = parse_cell_reference(&cell.reference).ok_or_else(|| {
        ReadError::InvalidXlsx("worksheet cell has an invalid or missing reference".to_owned())
    })?;
    let number_format = styles.number_format(cell.style_index);
    let font = styles.font(cell.style_index);
    let raw = if cell.has_inline {
        cell.inline
    } else {
        cell.value
    };
    let mut rich_text = cell.rich_text;
    let mut rich_text_runs = cell.rich_text_runs;
    for run in &mut rich_text_runs {
        if let Some(color) = &mut run.font_color {
            color.resolved_rgb = resolve_color(color, &styles.theme);
        }
    }
    let (value_type, display) = match cell.cell_type.as_deref() {
        Some("s") => {
            let index = raw.parse::<usize>().map_err(|_| {
                ReadError::InvalidXlsx("shared string cell has an invalid index".to_owned())
            })?;
            match shared_strings.get(index) {
                Some(value) => {
                    rich_text = value.rich;
                    rich_text_runs = value.runs.clone();
                    (CellValueType::String, value.text.clone())
                }
                None => {
                    warnings.push(format!(
                        "cell {} refers to missing shared string index {index}",
                        cell.reference
                    ));
                    (CellValueType::String, raw.clone())
                }
            }
        }
        Some("inlineStr" | "str") => (CellValueType::String, raw.clone()),
        Some("b") => (
            CellValueType::Boolean,
            if raw == "1" { "TRUE" } else { "FALSE" }.to_owned(),
        ),
        Some("e") => (CellValueType::Error, raw.clone()),
        Some("d") => (CellValueType::Date, raw.clone()),
        _ if number_format.as_deref().is_some_and(is_date_format) => {
            let display = raw
                .parse::<f64>()
                .ok()
                .and_then(|serial| {
                    format_excel_serial(serial, date_system, number_format.as_deref())
                })
                .unwrap_or_else(|| raw.clone());
            (CellValueType::Date, display)
        }
        _ if raw.is_empty() => (CellValueType::Blank, String::new()),
        _ => (CellValueType::Number, raw.clone()),
    };
    let formula = (include_formulas && cell.has_formula && !cell.formula.is_empty()).then(|| {
        if cell.formula.starts_with('=') {
            cell.formula
        } else {
            format!("={}", cell.formula)
        }
    });
    let formula_kind = (include_formulas && cell.has_formula)
        .then_some(cell.formula_kind.unwrap_or(FormulaKind::Normal));
    Ok(Some(WorkbookCell {
        reference: cell.reference,
        row,
        column,
        value_type,
        value: raw,
        display,
        formula,
        formula_kind,
        formula_reference: include_formulas.then_some(cell.formula_reference).flatten(),
        shared_formula_index: include_formulas
            .then_some(cell.shared_formula_index)
            .flatten(),
        rich_text,
        font_strike: font.and_then(|font| font.strike),
        font_color: font.and_then(|font| font.color.clone()),
        rich_text_runs,
        merge: None,
        style_index: cell.style_index,
        number_format,
    }))
}

fn plan_selections(
    sheets: &[SheetDescriptor],
    options: &SpreadsheetReadOptions,
) -> Result<(Vec<SelectionPlan>, Vec<WorkbookSelection>), ReadError> {
    let mut plans = Vec::new();
    let mut selections = Vec::new();
    if options.ranges.is_empty() {
        let mut indexes: Vec<usize> = sheets
            .iter()
            .enumerate()
            .filter_map(|(index, sheet)| (sheet.state == SheetState::Visible).then_some(index))
            .collect();
        if indexes.is_empty() {
            indexes.push(0);
        }
        let bounds = SelectionBounds {
            start_row: 1,
            start_column: 1,
            end_row: options.preview_rows,
            end_column: options.preview_columns,
        };
        for sheet_index in indexes {
            let sheet = &sheets[sheet_index];
            let range = format_bounds(bounds);
            let requested = format!("{}!{range}", quote_sheet_name(&sheet.name));
            push_selection(
                &mut plans,
                &mut selections,
                sheet_index,
                sheet.name.clone(),
                requested,
                bounds,
            );
        }
        return Ok((plans, selections));
    }

    for selector in &options.ranges {
        let (sheet_name, bounds) = parse_selector(selector)?;
        let sheet_index = sheets
            .iter()
            .position(|sheet| sheet.name == sheet_name)
            .or_else(|| {
                sheets
                    .iter()
                    .position(|sheet| sheet.name.eq_ignore_ascii_case(&sheet_name))
            })
            .ok_or_else(|| ReadError::WorksheetNotFound(sheet_name.clone()))?;
        push_selection(
            &mut plans,
            &mut selections,
            sheet_index,
            sheets[sheet_index].name.clone(),
            selector.clone(),
            bounds,
        );
    }
    Ok((plans, selections))
}

fn push_selection(
    plans: &mut Vec<SelectionPlan>,
    selections: &mut Vec<WorkbookSelection>,
    sheet_index: usize,
    sheet: String,
    requested: String,
    bounds: SelectionBounds,
) {
    let output_index = selections.len();
    selections.push(WorkbookSelection {
        requested,
        sheet,
        range: format_bounds(bounds),
        bounds,
        used_bounds: None,
        merged_ranges: Vec::new(),
        images: Vec::new(),
        images_truncated: false,
        overflow: WorkbookSelectionOverflow::default(),
        cells: Vec::new(),
        truncated: false,
    });
    plans.push(SelectionPlan {
        output_index,
        sheet_index,
        bounds,
    });
}

fn parse_selector(selector: &str) -> Result<(String, SelectionBounds), ReadError> {
    let (sheet, range) =
        split_selector(selector).ok_or_else(|| invalid_range(selector, "expected Sheet!A1:D20"))?;
    if sheet.is_empty() {
        return Err(invalid_range(selector, "worksheet name is empty"));
    }
    let (start, end) = range.split_once(':').map_or((range, range), |parts| parts);
    let (start_row, start_column) =
        parse_cell_reference(start).ok_or_else(|| invalid_range(selector, "invalid start cell"))?;
    let (end_row, end_column) =
        parse_cell_reference(end).ok_or_else(|| invalid_range(selector, "invalid end cell"))?;
    if start_row > end_row || start_column > end_column {
        return Err(invalid_range(
            selector,
            "start cell must not follow end cell",
        ));
    }
    Ok((
        sheet,
        SelectionBounds {
            start_row,
            start_column,
            end_row,
            end_column,
        },
    ))
}

fn split_selector(selector: &str) -> Option<(String, &str)> {
    if let Some(rest) = selector.strip_prefix('\'') {
        let bytes = rest.as_bytes();
        let mut index = 0;
        let mut sheet = String::new();
        while index < bytes.len() {
            if bytes[index] == b'\'' {
                if bytes.get(index + 1) == Some(&b'\'') {
                    sheet.push('\'');
                    index += 2;
                    continue;
                }
                if bytes.get(index + 1) == Some(&b'!') {
                    return Some((sheet, &rest[index + 2..]));
                }
                return None;
            }
            let character = rest[index..].chars().next()?;
            sheet.push(character);
            index += character.len_utf8();
        }
        None
    } else {
        let (sheet, range) = selector.rsplit_once('!')?;
        Some((sheet.to_owned(), range))
    }
}

fn parse_cell_reference(reference: &str) -> Option<(u32, u16)> {
    let reference = reference.replace('$', "");
    let split = reference.find(|character: char| character.is_ascii_digit())?;
    let (letters, digits) = reference.split_at(split);
    if letters.is_empty()
        || digits.is_empty()
        || !letters
            .chars()
            .all(|character| character.is_ascii_alphabetic())
        || !digits.chars().all(|character| character.is_ascii_digit())
    {
        return None;
    }
    let mut column: u32 = 0;
    for letter in letters.bytes() {
        column = column
            .checked_mul(26)?
            .checked_add(u32::from(letter.to_ascii_uppercase() - b'A' + 1))?;
    }
    let row: u32 = digits.parse().ok()?;
    if row == 0 || row > 1_048_576 || column == 0 || column > 16_384 {
        return None;
    }
    Some((row, u16::try_from(column).ok()?))
}

fn contains(bounds: SelectionBounds, row: u32, column: u16) -> bool {
    row >= bounds.start_row
        && row <= bounds.end_row
        && column >= bounds.start_column
        && column <= bounds.end_column
}

fn update_semantic_bounds(bounds: &mut Option<SelectionBounds>, row: u32, column: u16) {
    match bounds {
        Some(bounds) => {
            bounds.start_row = bounds.start_row.min(row);
            bounds.start_column = bounds.start_column.min(column);
            bounds.end_row = bounds.end_row.max(row);
            bounds.end_column = bounds.end_column.max(column);
        }
        None => {
            *bounds = Some(SelectionBounds {
                start_row: row,
                start_column: column,
                end_row: row,
                end_column: column,
            });
        }
    }
}

fn format_bounds(bounds: SelectionBounds) -> String {
    format!(
        "{}{}:{}{}",
        column_name(bounds.start_column),
        bounds.start_row,
        column_name(bounds.end_column),
        bounds.end_row
    )
}

fn column_name(column: u16) -> String {
    let mut value = u32::from(column);
    let mut bytes = Vec::new();
    while value > 0 {
        value -= 1;
        bytes.push(b'A' + u8::try_from(value % 26).unwrap_or(0));
        value /= 26;
    }
    bytes.reverse();
    String::from_utf8(bytes).unwrap_or_default()
}

fn quote_sheet_name(name: &str) -> String {
    if name
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        name.to_owned()
    } else {
        format!("'{}'", name.replace('\'', "''"))
    }
}

fn render_markdown(title: &str, workbook: &WorkbookInfo) -> String {
    let mut output = format!("# Workbook: {}\n\n", markdown_escape(title));
    render_markdown_manifest(&mut output, workbook);
    for selection in &workbook.selections {
        render_markdown_selection(&mut output, selection);
    }
    output
}

fn render_markdown_generated_update(
    workbook: &WorkbookInfo,
    selection_indexes: &[usize],
) -> String {
    let mut output = String::new();
    render_markdown_manifest(&mut output, workbook);
    for index in selection_indexes {
        render_markdown_selection(&mut output, &workbook.selections[*index]);
    }
    output
}

fn render_markdown_manifest(output: &mut String, workbook: &WorkbookInfo) {
    output.push_str("<!-- opsail:xlsx-generated:manifest:start -->\n");
    output.push_str(
        "| # | Sheet | State | Declared dimension | Semantic bounds | Pictures | Selected |\n",
    );
    output.push_str("|---:|---|---|---|---|---:|---|\n");
    for sheet in &workbook.sheets {
        output.push_str(&format!(
            "| {} | {} | {:?} | {} | {} | {} | {} |\n",
            sheet.index + 1,
            markdown_escape(&sheet.name),
            sheet.state,
            sheet.declared_dimension.as_deref().unwrap_or(""),
            sheet.semantic_bounds.as_deref().unwrap_or(""),
            sheet.pictures.len(),
            if sheet.selected { "yes" } else { "no" }
        ));
    }
    if !workbook.defined_names.is_empty() {
        output.push_str("\n## Defined names\n\n");
        output.push_str("| Name | Scope | Reference | Valid |\n|---|---|---|---|\n");
        for name in &workbook.defined_names {
            output.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                markdown_escape(&name.name),
                name.local_sheet_index
                    .map(|index| index.to_string())
                    .unwrap_or_else(|| "workbook".to_owned()),
                markdown_escape(&name.reference),
                if name.valid_reference { "yes" } else { "no" }
            ));
        }
    }
    if workbook
        .sheets
        .iter()
        .any(|sheet| has_print_evidence(&sheet.print))
    {
        output.push_str("\n## Print evidence\n\n");
        output.push_str(
            "| Sheet | Print area | Print titles | Page setup | Row breaks | Column breaks |\n|---|---|---|---|---|---|\n",
        );
        for sheet in workbook
            .sheets
            .iter()
            .filter(|sheet| has_print_evidence(&sheet.print))
        {
            output.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} |\n",
                markdown_escape(&sheet.name),
                markdown_escape(sheet.print.print_area.as_deref().unwrap_or("")),
                markdown_escape(sheet.print.print_titles.as_deref().unwrap_or("")),
                markdown_escape(&page_setup_summary(sheet.print.page_setup.as_ref())),
                join_u32(&sheet.print.row_breaks),
                join_u32(&sheet.print.column_breaks),
            ));
        }
    }
    output.push_str("<!-- opsail:xlsx-generated:manifest:end -->\n");
}

fn render_markdown_selection(output: &mut String, selection: &WorkbookSelection) {
    let marker = selection_marker(&selection.requested);
    output.push_str(&format!(
        "\n<!-- opsail:xlsx-generated:{marker}:start -->\n"
    ));
    output.push_str(&format!(
        "\n## {}!{}\n\n",
        markdown_escape(&selection.sheet),
        selection.range
    ));
    if let Some(used_bounds) = &selection.used_bounds {
        output.push_str(&format!(
            "> Used cells on the requested rows span `{used_bounds}`.\n"
        ));
    }
    render_markdown_selection_overflow(output, &selection.overflow);
    if !selection.merged_ranges.is_empty() {
        output.push_str(&format!(
            "> Intersecting merges: {}. Covered cells remain blank and point to their anchor in JSON.\n",
            selection
                .merged_ranges
                .iter()
                .map(|range| format!("`{range}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !selection.images.is_empty() {
        output.push_str(&format!(
            "> Intersecting worksheet pictures: {}. Pixels are not OCR text; open the bounded `dataUri` in JSON when present.\n",
            selection.images.len()
        ));
    }
    if selection.used_bounds.is_some()
        || !selection.overflow.is_empty()
        || !selection.merged_ranges.is_empty()
        || !selection.images.is_empty()
    {
        output.push('\n');
    }
    let rows = u64::from(selection.bounds.end_row - selection.bounds.start_row + 1);
    let columns = u64::from(selection.bounds.end_column - selection.bounds.start_column + 1);
    if rows.saturating_mul(columns) <= PREVIEW_GRID_MAX_AREA && columns <= 50 {
        render_markdown_grid(output, selection);
    } else {
        output.push_str("| Cell | Value | Type | Formula |\n|---|---|---|---|\n");
        for cell in &selection.cells {
            output.push_str(&format!(
                "| {} | {} | {:?} | {} |\n",
                cell.reference,
                markdown_escape(&cell.display),
                cell.value_type,
                markdown_escape(cell.formula.as_deref().unwrap_or(""))
            ));
        }
    }
    render_markdown_text_formatting(output, selection);
    render_markdown_pictures(output, selection);
    if selection.truncated {
        output.push_str("\n> Selection output was truncated by the cell limit.\n");
    }
    if selection.images_truncated {
        output.push_str("\n> Some picture payloads or picture references were omitted by image limits; metadata for retained entries remains available.\n");
    }
    output.push_str(&format!("<!-- opsail:xlsx-generated:{marker}:end -->\n"));
}

fn render_markdown_pictures(output: &mut String, selection: &WorkbookSelection) {
    if selection.images.is_empty() {
        return;
    }
    output.push_str("\n### Worksheet pictures\n\n");
    output.push_str("| Anchor | Media part | Content type | Bytes | SHA-256 | Payload |\n");
    output.push_str("|---|---|---|---:|---|---|\n");
    for picture in &selection.images {
        let anchor = picture.to_cell.as_ref().map_or_else(
            || picture.from_cell.clone(),
            |to| format!("{}:{}", picture.from_cell, to),
        );
        let payload = if picture.data_uri.is_some() {
            "dataUri in JSON"
        } else if picture.payload_truncated {
            "metadata only (limit)"
        } else {
            "metadata only"
        };
        output.push_str(&format!(
            "| {} | {} | {} | {} | `{}` | {} |\n",
            markdown_escape(&anchor),
            markdown_escape(&picture.media_part),
            markdown_escape(&picture.content_type),
            picture.byte_size,
            picture.sha256,
            payload,
        ));
    }
    output.push_str("\n> Picture bytes are evidence only; Opsail does not OCR them or treat them as worksheet cell text.\n");
}

fn render_markdown_selection_overflow(output: &mut String, overflow: &WorkbookSelectionOverflow) {
    if let Some(left) = &overflow.left {
        output.push_str(&format!(
            "> Column overflow left: {} through {} ({} used cells).\n",
            column_name(left.min_column),
            column_name(left.max_column),
            left.cell_count
        ));
    }
    if let Some(right) = &overflow.right {
        output.push_str(&format!(
            "> Column overflow right: {} through {} ({} used cells).\n",
            column_name(right.min_column),
            column_name(right.max_column),
            right.cell_count
        ));
    }
    if let Some(above) = &overflow.above {
        output.push_str(&format!(
            "> Scanned row overflow above: {} through {} ({} used cells in the requested columns).\n",
            above.min_row, above.max_row, above.cell_count
        ));
    }
    if let Some(below) = &overflow.below {
        output.push_str(&format!(
            "> Scanned row overflow below: {} through {} ({} used cells in the requested columns).\n",
            below.min_row, below.max_row, below.cell_count
        ));
    }
}

fn has_print_evidence(print: &WorksheetPrintEvidence) -> bool {
    print.print_area.is_some()
        || print.print_titles.is_some()
        || print.page_setup.is_some()
        || print.print_options.is_some()
        || !print.row_breaks.is_empty()
        || !print.column_breaks.is_empty()
        || print.header_footer
}

fn page_setup_summary(page_setup: Option<&WorksheetPageSetup>) -> String {
    let Some(page_setup) = page_setup else {
        return String::new();
    };
    let mut fields = Vec::new();
    if let Some(value) = &page_setup.orientation {
        fields.push(format!("orientation={value}"));
    }
    if let Some(value) = page_setup.paper_size {
        fields.push(format!("paperSize={value}"));
    }
    if let Some(value) = page_setup.fit_to_page {
        fields.push(format!("fitToPage={value}"));
    }
    if let Some(value) = page_setup.fit_to_width {
        fields.push(format!("fitToWidth={value}"));
    }
    if let Some(value) = page_setup.fit_to_height {
        fields.push(format!("fitToHeight={value}"));
    }
    if let Some(value) = page_setup.scale {
        fields.push(format!("scale={value}"));
    }
    fields.join(", ")
}

fn join_u32(values: &[u32]) -> String {
    values
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Merge generated workbook blocks into an agent-maintained Markdown mirror.
/// Text outside generated blocks is preserved byte-for-byte. A malformed
/// existing mirror is rejected instead of being overwritten.
pub fn merge_markdown_mirror(
    existing: &str,
    update: &WorkbookReadResult,
) -> Result<String, ReadError> {
    if existing.is_empty() {
        return Ok(update.content.clone());
    }
    merge_generated_blocks(existing, &update.content)
}

fn merge_generated_blocks(existing: &str, update: &str) -> Result<String, ReadError> {
    let existing_blocks = parse_generated_blocks(existing)?;
    if existing_blocks.is_empty() {
        return Err(ReadError::InvalidMarkdownMirror(
            "existing Markdown has no Opsail generated blocks".to_owned(),
        ));
    }
    let update_blocks = parse_generated_blocks(update)?;
    let mut merged = existing.to_owned();
    let mut replacements = Vec::new();
    let mut additions = Vec::new();
    for (id, update_block) in update_blocks {
        let replacement = &update[update_block.start..update_block.end];
        if let Some(existing_block) = existing_blocks.get(&id) {
            replacements.push((existing_block.start, existing_block.end, replacement));
        } else {
            additions.push(replacement);
        }
    }
    replacements.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));
    for (start, end, replacement) in replacements {
        merged.replace_range(start..end, replacement);
    }
    for addition in additions {
        if !merged.ends_with('\n') {
            merged.push('\n');
        }
        merged.push('\n');
        merged.push_str(addition);
    }
    Ok(merged)
}

#[derive(Debug)]
struct GeneratedBlock {
    start: usize,
    end: usize,
}

fn parse_generated_blocks(markdown: &str) -> Result<BTreeMap<String, GeneratedBlock>, ReadError> {
    const PREFIX: &str = "<!-- opsail:xlsx-generated:";
    const START_SUFFIX: &str = ":start -->";
    const END_SUFFIX: &str = ":end -->";
    let mut blocks = BTreeMap::new();
    let mut open: Option<(String, usize)> = None;
    let mut offset = 0;
    for line in markdown.split_inclusive('\n') {
        let marker = line.trim_end_matches(['\r', '\n']);
        if let Some(id) = marker
            .strip_prefix(PREFIX)
            .and_then(|value| value.strip_suffix(START_SUFFIX))
        {
            if id.is_empty() || open.is_some() || blocks.contains_key(id) {
                return Err(ReadError::InvalidMarkdownMirror(
                    "generated Markdown blocks are nested, empty, or duplicated".to_owned(),
                ));
            }
            open = Some((id.to_owned(), offset));
        } else if let Some(id) = marker
            .strip_prefix(PREFIX)
            .and_then(|value| value.strip_suffix(END_SUFFIX))
        {
            let Some((open_id, start)) = open.take() else {
                return Err(ReadError::InvalidMarkdownMirror(
                    "generated Markdown block has an unmatched end marker".to_owned(),
                ));
            };
            if open_id != id {
                return Err(ReadError::InvalidMarkdownMirror(
                    "generated Markdown block markers do not match".to_owned(),
                ));
            }
            blocks.insert(
                open_id,
                GeneratedBlock {
                    start,
                    end: offset + line.len(),
                },
            );
        }
        offset += line.len();
    }
    if open.is_some() {
        return Err(ReadError::InvalidMarkdownMirror(
            "generated Markdown block has no end marker".to_owned(),
        ));
    }
    Ok(blocks)
}

fn selection_marker(requested: &str) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    fnv1a_update(&mut hash, requested.as_bytes());
    format!("selection-{hash:016x}")
}

fn render_markdown_grid(output: &mut String, selection: &WorkbookSelection) {
    let by_position: HashMap<(u32, u16), &WorkbookCell> = selection
        .cells
        .iter()
        .map(|cell| ((cell.row, cell.column), cell))
        .collect();
    output.push_str("| Row |");
    for column in selection.bounds.start_column..=selection.bounds.end_column {
        output.push_str(&format!(" {} |", column_name(column)));
    }
    output.push_str("\n|---:|");
    for _ in selection.bounds.start_column..=selection.bounds.end_column {
        output.push_str("---|");
    }
    output.push('\n');
    for row in selection.bounds.start_row..=selection.bounds.end_row {
        output.push_str(&format!("| {row} |"));
        for column in selection.bounds.start_column..=selection.bounds.end_column {
            let display = by_position
                .get(&(row, column))
                .map_or("", |cell| cell.display.as_str());
            output.push_str(&format!(" {} |", markdown_escape(display)));
        }
        output.push('\n');
    }
}

fn render_html(title: &str, workbook: &WorkbookInfo) -> String {
    let mut output = format!(
        "<article class=\"opsail-workbook\"><h1>Workbook: {}</h1>\n",
        html_escape(title)
    );
    render_html_manifest(&mut output, workbook);
    for selection in &workbook.selections {
        render_html_selection(&mut output, selection);
    }
    output.push_str("</article>");
    output
}

fn render_html_generated_update(workbook: &WorkbookInfo, selection_indexes: &[usize]) -> String {
    let mut output = String::new();
    render_html_manifest(&mut output, workbook);
    for index in selection_indexes {
        render_html_selection(&mut output, &workbook.selections[*index]);
    }
    output
}

fn render_html_manifest(output: &mut String, workbook: &WorkbookInfo) {
    output.push_str("<!-- opsail:xlsx-generated:manifest:start -->\n<ul>");
    for sheet in &workbook.sheets {
        output.push_str(&format!(
            "<li>{} ({:?}; {} worksheet pictures)</li>",
            html_escape(&sheet.name),
            sheet.state,
            sheet.pictures.len()
        ));
    }
    output.push_str("</ul>\n<!-- opsail:xlsx-generated:manifest:end -->\n");
}

fn render_html_selection(output: &mut String, selection: &WorkbookSelection) {
    let marker = selection_marker(&selection.requested);
    output.push_str(&format!(
        "<!-- opsail:xlsx-generated:{marker}:start -->\n<section><h2>{}!{}</h2>",
        html_escape(&selection.sheet),
        html_escape(&selection.range)
    ));
    if let Some(used_bounds) = &selection.used_bounds {
        output.push_str(&format!(
            "<p>Used cells on the requested rows span <code>{}</code>.</p>",
            html_escape(used_bounds)
        ));
    }
    if let Some(right) = &selection.overflow.right {
        output.push_str(&format!(
            "<p>Column overflow right: {} through {} ({} used cells).</p>",
            column_name(right.min_column),
            column_name(right.max_column),
            right.cell_count
        ));
    }
    if !selection.merged_ranges.is_empty() {
        output.push_str(&format!(
            "<p>Intersecting merges: {}.</p>",
            html_escape(&selection.merged_ranges.join(", "))
        ));
    }
    if !selection.images.is_empty() {
        output.push_str(&format!(
            "<p>{} intersecting worksheet pictures. Pixel payloads are bounded data URIs in JSON and are not OCR text.</p>",
            selection.images.len()
        ));
    }
    output.push_str("<table><thead><tr><th>Cell</th><th>Value</th><th>Type</th><th>Formula</th></tr></thead><tbody>");
    for cell in &selection.cells {
        output.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{:?}</td><td>{}</td></tr>",
            html_escape(&cell.reference),
            html_escape(&cell.display),
            cell.value_type,
            html_escape(cell.formula.as_deref().unwrap_or(""))
        ));
    }
    output.push_str("</tbody></table>");
    let formatted: Vec<_> = selection
        .cells
        .iter()
        .filter_map(formatting_summary)
        .collect();
    if !formatted.is_empty() {
        output.push_str("<h3>Text formatting</h3><ul>");
        for summary in formatted {
            output.push_str(&format!("<li>{}</li>", html_escape(&summary)));
        }
        output.push_str("</ul>");
    }
    if !selection.images.is_empty() {
        output.push_str("<h3>Worksheet pictures</h3><ul>");
        for picture in &selection.images {
            let anchor = picture.to_cell.as_ref().map_or_else(
                || picture.from_cell.clone(),
                |to| format!("{}:{}", picture.from_cell, to),
            );
            let payload = if picture.data_uri.is_some() {
                "dataUri in JSON"
            } else if picture.payload_truncated {
                "metadata only (limit)"
            } else {
                "metadata only"
            };
            output.push_str(&format!(
                "<li>{}: {} ({} bytes, {}, {})</li>",
                html_escape(&anchor),
                html_escape(&picture.media_part),
                picture.byte_size,
                html_escape(&picture.content_type),
                payload,
            ));
        }
        output.push_str("</ul>");
    }
    output.push_str(&format!(
        "</section>\n<!-- opsail:xlsx-generated:{marker}:end -->\n"
    ));
}

fn render_markdown_text_formatting(output: &mut String, selection: &WorkbookSelection) {
    let formatted: Vec<_> = selection
        .cells
        .iter()
        .filter_map(formatting_summary)
        .collect();
    if formatted.is_empty() {
        return;
    }
    output.push_str("\n### Text formatting\n\n");
    for summary in formatted {
        output.push_str(&format!("- {}\n", markdown_escape(&summary)));
    }
}

fn formatting_summary(cell: &WorkbookCell) -> Option<String> {
    let mut details = Vec::new();
    if cell.font_strike == Some(true) {
        details.push("cell strike=true".to_owned());
    }
    if let Some(color) = &cell.font_color {
        details.push(format!("cell color={}", color_summary(color)));
    }
    for (index, run) in cell.rich_text_runs.iter().enumerate() {
        if run.strike == Some(true) || run.font_color.is_some() {
            let mut run_details = Vec::new();
            if run.strike == Some(true) {
                run_details.push("strike=true".to_owned());
            }
            if let Some(color) = &run.font_color {
                run_details.push(format!("color={}", color_summary(color)));
            }
            details.push(format!(
                "run {} {:?}: {}",
                index + 1,
                run.text,
                run_details.join(", ")
            ));
        }
    }
    (!details.is_empty()).then(|| format!("{}: {}", cell.reference, details.join("; ")))
}

fn color_summary(color: &WorkbookFontColor) -> String {
    let mut fields = Vec::new();
    if let Some(value) = &color.rgb {
        fields.push(format!("rgb={value}"));
    }
    if let Some(value) = color.theme {
        fields.push(format!("theme={value}"));
    }
    if let Some(value) = color.indexed {
        fields.push(format!("indexed={value}"));
    }
    if let Some(value) = color.auto {
        fields.push(format!("auto={value}"));
    }
    if let Some(value) = color.tint {
        fields.push(format!("tint={value}"));
    }
    if let Some(value) = &color.resolved_rgb {
        fields.push(format!("resolvedRgb={value}"));
    }
    fields.join(",")
}

fn markdown_escape(value: &str) -> String {
    value
        .replace('|', "\\|")
        .replace("\r\n", "<br>")
        .replace(['\r', '\n'], "<br>")
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn format_excel_serial(
    serial: f64,
    date_system: DateSystem,
    number_format: Option<&str>,
) -> Option<String> {
    if !serial.is_finite() || serial < 0.0 {
        return None;
    }
    let mut whole = serial.floor() as i64;
    let mut seconds = ((serial - serial.floor()) * 86_400.0).round() as i64;
    if seconds >= 86_400 {
        whole += 1;
        seconds -= 86_400;
    }
    let has_date = number_format.is_some_and(format_has_date);
    let has_time = number_format.is_some_and(format_has_time);
    let elapsed_hours = number_format.is_some_and(format_has_elapsed_hours);
    let displayed_seconds = if elapsed_hours {
        (serial * 86_400.0).round() as i64
    } else {
        seconds
    };
    let time = format!(
        "{:02}:{:02}:{:02}",
        displayed_seconds / 3_600,
        (displayed_seconds % 3_600) / 60,
        displayed_seconds % 60
    );
    if !has_date {
        return Some(time);
    }
    let date = if matches!(date_system, DateSystem::Excel1900) && whole == 60 {
        "1900-02-29".to_owned()
    } else {
        let base = match date_system {
            DateSystem::Excel1900 => days_from_civil(1899, 12, 31),
            DateSystem::Excel1904 => days_from_civil(1904, 1, 1),
        };
        let adjusted = match date_system {
            DateSystem::Excel1900 if whole > 60 => whole - 1,
            _ => whole,
        };
        let (year, month, day) = civil_from_days(base + adjusted);
        format!("{year:04}-{month:02}-{day:02}")
    };
    if has_time {
        Some(format!("{date} {time}"))
    } else {
        Some(date)
    }
}

fn is_date_format(format: &str) -> bool {
    format_has_date(format) || format_has_time(format)
}

fn format_has_date(format: &str) -> bool {
    let format = normalized_number_format(format);
    format.contains('y') || format.contains('d')
}

fn format_has_time(format: &str) -> bool {
    let format = normalized_number_format(format);
    format.contains('h') || format.contains('s') || format.contains("[m]")
}

fn format_has_elapsed_hours(format: &str) -> bool {
    let format = normalized_number_format(format);
    format.contains("[h]") || format.contains("[hh]")
}

fn normalized_number_format(format: &str) -> String {
    let mut normalized = String::new();
    let mut quoted = false;
    let mut escaped = false;
    let mut bracketed: Option<String> = None;
    for character in format.to_ascii_lowercase().chars() {
        if let Some(directive) = bracketed.as_mut() {
            if character == ']' {
                if matches!(directive.as_str(), "h" | "hh" | "m" | "mm" | "s" | "ss") {
                    normalized.push('[');
                    normalized.push_str(directive);
                    normalized.push(']');
                }
                bracketed = None;
            } else {
                directive.push(character);
            }
            continue;
        }
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            '\\' | '_' | '*' if !quoted => escaped = true,
            '"' => quoted = !quoted,
            '[' if !quoted => bracketed = Some(String::new()),
            _ if !quoted => normalized.push(character),
            _ => {}
        }
    }
    normalized
}

fn builtin_number_format(id: u32) -> Option<&'static str> {
    match id {
        14 => Some("mm-dd-yy"),
        15 => Some("d-mmm-yy"),
        16 => Some("d-mmm"),
        17 => Some("mmm-yy"),
        18 => Some("h:mm AM/PM"),
        19 => Some("h:mm:ss AM/PM"),
        20 => Some("h:mm"),
        21 => Some("h:mm:ss"),
        22 => Some("m/d/yy h:mm"),
        45 => Some("mm:ss"),
        46 => Some("[h]:mm:ss"),
        47 => Some("mmss.0"),
        _ => None,
    }
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day = i64::from(day);
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month as u32, day as u32)
}

fn xml_reader(xml: &str) -> Reader<&[u8]> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    reader
}

fn attribute(element: &BytesStart<'_>, name: &[u8]) -> Result<Option<String>, ReadError> {
    for attribute in element.attributes().with_checks(false) {
        let attribute = attribute.map_err(|_| {
            ReadError::InvalidXlsx("OOXML element contains an invalid attribute".to_owned())
        })?;
        if local_name(attribute.key.as_ref()) == name {
            return attribute
                .unescape_value()
                .map(|value| Some(value.into_owned()))
                .map_err(|_| {
                    ReadError::InvalidXlsx(
                        "OOXML element contains an invalid attribute value".to_owned(),
                    )
                });
        }
    }
    Ok(None)
}

fn attribute_exact(element: &BytesStart<'_>, name: &[u8]) -> Result<Option<String>, ReadError> {
    for attribute in element.attributes().with_checks(false) {
        let attribute = attribute.map_err(|_| {
            ReadError::InvalidXlsx("OOXML element contains an invalid attribute".to_owned())
        })?;
        if attribute.key.as_ref() == name {
            return attribute
                .unescape_value()
                .map(|value| Some(value.into_owned()))
                .map_err(|_| {
                    ReadError::InvalidXlsx(
                        "OOXML element contains an invalid attribute value".to_owned(),
                    )
                });
        }
    }
    Ok(None)
}

fn local_name(name: &[u8]) -> &[u8] {
    name.rsplit(|byte| *byte == b':').next().unwrap_or(name)
}

fn normalize_workbook_target(target: &str) -> Result<String, ReadError> {
    let target = target.trim_start_matches('/');
    let candidate = if target.starts_with("xl/") {
        target.to_owned()
    } else {
        format!("xl/{target}")
    };
    if candidate
        .split('/')
        .any(|component| component.is_empty() || matches!(component, "." | ".."))
    {
        return Err(ReadError::InvalidXlsx(
            "worksheet relationship target is unsafe".to_owned(),
        ));
    }
    Ok(candidate)
}

fn worksheet_relationship_path(path: &str) -> Result<String, ReadError> {
    part_relationship_path(path)
}

fn part_relationship_path(path: &str) -> Result<String, ReadError> {
    let (directory, file) = path
        .rsplit_once('/')
        .ok_or_else(|| ReadError::InvalidXlsx("OOXML part path has no directory".to_owned()))?;
    if file.is_empty() || directory.is_empty() {
        return Err(ReadError::InvalidXlsx(
            "OOXML part path is invalid".to_owned(),
        ));
    }
    Ok(format!("{directory}/_rels/{file}.rels"))
}

fn resolve_part_target(source_part: &str, target: &str) -> Result<String, ReadError> {
    let mut components = if target.starts_with('/') {
        Vec::new()
    } else {
        source_part
            .rsplit_once('/')
            .map(|(directory, _)| {
                directory
                    .split('/')
                    .filter(|component| !component.is_empty())
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    for component in target.trim_start_matches('/').split('/') {
        match component {
            "" | "." => {}
            ".." => {
                if components.pop().is_none() {
                    return Err(ReadError::InvalidXlsx(
                        "OOXML relationship target escapes the package root".to_owned(),
                    ));
                }
            }
            component => components.push(component.to_owned()),
        }
    }
    if components.is_empty() {
        return Err(ReadError::InvalidXlsx(
            "OOXML relationship target is empty".to_owned(),
        ));
    }
    Ok(components.join("/"))
}

fn is_worksheet_part(name: &str) -> bool {
    name.starts_with("xl/worksheets/") && name.ends_with(".xml") && !name.contains("/_rels/")
}

fn worksheet_part_for_change(name: &str) -> Option<String> {
    if is_worksheet_part(name) {
        return Some(name.to_owned());
    }
    let file = name.strip_prefix("xl/worksheets/_rels/")?;
    let worksheet = file.strip_suffix(".rels")?;
    let part = format!("xl/worksheets/{worksheet}");
    is_worksheet_part(&part).then_some(part)
}

fn fnv1a_update(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

fn xml_truthy(value: &str) -> bool {
    matches!(value, "1" | "true" | "TRUE")
}

fn push_bounded(target: &mut String, value: &str) {
    if target.len() >= MAX_CELL_TEXT_BYTES {
        return;
    }
    let remaining = MAX_CELL_TEXT_BYTES - target.len();
    if value.len() <= remaining {
        target.push_str(value);
        return;
    }
    let mut boundary = remaining;
    while boundary > 0 && !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    target.push_str(&value[..boundary]);
}

fn invalid_range(selector: &str, reason: &str) -> ReadError {
    ReadError::InvalidSpreadsheetRange {
        selector: selector.to_owned(),
        reason: reason.to_owned(),
    }
}

fn invalid_zip(error: ZipError) -> ReadError {
    ReadError::InvalidXlsx(format!("invalid ZIP package: {error}"))
}

fn xml_error(error: quick_xml::Error) -> ReadError {
    ReadError::InvalidXlsx(format!("invalid OOXML: {error}"))
}

fn encoding_error(error: quick_xml::encoding::EncodingError) -> ReadError {
    ReadError::InvalidXlsx(format!("invalid OOXML text encoding: {error}"))
}

fn decode_reference(reference: &BytesRef<'_>) -> Result<String, ReadError> {
    if let Some(character) = reference.resolve_char_ref().map_err(xml_error)? {
        return Ok(character.to_string());
    }
    let name = reference.decode().map_err(encoding_error)?;
    quick_xml::escape::resolve_predefined_entity(&name)
        .map(str::to_owned)
        .ok_or_else(|| ReadError::InvalidXlsx(format!("unrecognized OOXML entity `&{name};`")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_quoted_sheet_ranges() {
        let (sheet, bounds) = parse_selector("'API Sheet'!$B$6:AY40").unwrap();
        assert_eq!(sheet, "API Sheet");
        assert_eq!(format_bounds(bounds), "B6:AY40");

        let (sheet, _) = parse_selector("'Owner''s Sheet'!A1").unwrap();
        assert_eq!(sheet, "Owner's Sheet");
    }

    #[test]
    fn rejects_out_of_bounds_references() {
        assert!(parse_selector("Sheet1!A1:XFD1048576").is_ok());
        assert!(parse_selector("Sheet1!A1:XFE2").is_err());
        assert!(parse_selector("Sheet1!A0:B2").is_err());
    }

    #[test]
    fn formats_excel_dates_without_recalculation() {
        assert_eq!(
            format_excel_serial(45_000.5, DateSystem::Excel1900, Some("yyyy-mm-dd hh:mm")),
            Some("2023-03-15 12:00:00".to_owned())
        );
        assert_eq!(
            format_excel_serial(0.5, DateSystem::Excel1904, Some("hh:mm")),
            Some("12:00:00".to_owned())
        );
        assert_eq!(
            format_excel_serial(1.5, DateSystem::Excel1900, Some("[h]:mm:ss")),
            Some("36:00:00".to_owned())
        );
    }

    #[test]
    fn strips_non_time_bracket_directives_before_date_detection() {
        assert!(!is_date_format("[Red]0.00"));
        assert!(!is_date_format("[Blue][<=100]0.00"));
        assert!(is_date_format("[h]:mm:ss"));
        assert!(is_date_format("[$-409]yyyy-mm-dd"));
    }

    #[test]
    fn parses_one_cell_picture_anchors_and_skips_charts() {
        let xml = r#"<xdr:wsDr xmlns:xdr="http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing"
          xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
          xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
          <xdr:oneCellAnchor><xdr:from><xdr:col>5</xdr:col><xdr:row>11</xdr:row></xdr:from>
            <xdr:ext cx="1" cy="1"/><xdr:pic><xdr:blipFill><a:blip r:embed="rIdImage"/></xdr:blipFill></xdr:pic><xdr:clientData/>
          </xdr:oneCellAnchor>
          <xdr:twoCellAnchor><xdr:from><xdr:col>0</xdr:col><xdr:row>0</xdr:row></xdr:from>
            <xdr:to><xdr:col>3</xdr:col><xdr:row>9</xdr:row></xdr:to><xdr:graphicFrame/><xdr:clientData/>
          </xdr:twoCellAnchor>
        </xdr:wsDr>"#;
        let relationships = HashMap::from([(
            "rIdImage".to_owned(),
            PartRelationship {
                relationship_type:
                    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image"
                        .to_owned(),
                target: "../media/image1.png".to_owned(),
                external: false,
            },
        )]);

        let pictures =
            parse_drawing_pictures(xml, "xl/drawings/drawing1.xml", &relationships).unwrap();
        assert_eq!(pictures.len(), 1);
        assert_eq!(pictures[0].from.row, Some(11));
        assert_eq!(pictures[0].from.column, Some(5));
        assert!(pictures[0].to.is_none());
        assert_eq!(pictures[0].media_part, "xl/media/image1.png");
    }
}
