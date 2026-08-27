use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek};
use std::path::{Path, PathBuf};
use std::time::Instant;
use std::time::SystemTime;

use quick_xml::Reader;
use quick_xml::events::{BytesRef, BytesStart, Event};
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
    pub hidden_rows: usize,
    pub hidden_columns: usize,
    pub features: WorksheetFeatureInventory,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorksheetFeatureInventory {
    pub scanned: bool,
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
    pub cells: Vec<WorkbookCell>,
    pub truncated: bool,
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
    pub style_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number_format: Option<String>,
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
}

impl Styles {
    fn number_format(&self, style_index: Option<usize>) -> Option<String> {
        let id = *self.cell_formats.get(style_index?)?;
        self.custom_formats
            .get(&id)
            .cloned()
            .or_else(|| builtin_number_format(id).map(str::to_owned))
    }
}

#[derive(Debug, Clone)]
struct SharedString {
    text: String,
    rich: bool,
}

#[derive(Debug, Clone)]
struct PartRelationship {
    relationship_type: String,
    target: String,
    external: bool,
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
    hidden_rows: usize,
    hidden_columns: usize,
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
        String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| ReadError::InvalidXlsx(format!("OOXML part `{name}` is not UTF-8 XML")))
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
    let partial_warning = "incremental worksheet refresh is bounded to the cached selections; semantic bounds and worksheet feature inventory are partial";
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
        let scan = package.scan_worksheet(
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
        sheet.semantic_bounds_complete = patch.scan.features.complete;
        sheet.merged_ranges = patch.scan.merged_ranges;
        sheet.hidden_rows = patch.scan.hidden_rows;
        sheet.hidden_columns = patch.scan.hidden_columns;
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
    let content_types = package.read_required_xml("[Content_Types].xml")?;
    if !content_types.contains(XLSX_CONTENT_TYPE) {
        return Err(ReadError::InvalidXlsx(
            "package does not declare an XLSX workbook content type".to_owned(),
        ));
    }
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
        let shared_strings = package
            .read_xml("xl/sharedStrings.xml")?
            .map(|xml| parse_shared_strings(&xml))
            .transpose()?
            .unwrap_or_default();
        let styles = package
            .read_xml("xl/styles.xml")?
            .map(|xml| parse_styles(&xml))
            .transpose()?
            .unwrap_or_default();
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
            hidden_rows: 0,
            hidden_columns: 0,
            features: WorksheetFeatureInventory::default(),
        })
        .collect();
    let mut statistics = WorkbookStatistics {
        archive_entries,
        ..WorkbookStatistics::default()
    };
    let mut warnings = Vec::new();
    let mut remaining_cells = options.spreadsheet.max_cells;

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
        let scan = package.scan_worksheet(
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
        let sheet = &mut sheets[sheet_index];
        sheet.declared_dimension = scan.dimension;
        sheet.semantic_bounds = scan.semantic.map(format_bounds);
        sheet.semantic_bounds_complete = scan.features.complete;
        sheet.merged_ranges = scan.merged_ranges;
        sheet.hidden_rows = scan.hidden_rows;
        sheet.hidden_columns = scan.hidden_columns;
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
    if sheets
        .iter()
        .any(|sheet| sheet.features.scanned && !sheet.features.complete)
    {
        warnings.push(
            "targeted worksheet scan stopped after the requested rows; semantic bounds and worksheet feature inventory are partial"
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

fn parse_shared_strings(xml: &str) -> Result<Vec<SharedString>, ReadError> {
    let mut reader = xml_reader(xml);
    let mut strings = Vec::new();
    let mut current: Option<String> = None;
    let mut rich = false;
    let mut inside_text = false;
    let mut phonetic_depth = 0usize;
    loop {
        match reader.read_event().map_err(xml_error)? {
            Event::Start(element) if local_name(element.name().as_ref()) == b"si" => {
                current = Some(String::new());
                rich = false;
            }
            Event::Start(element) if local_name(element.name().as_ref()) == b"r" => {
                if current.is_some() {
                    rich = true;
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
                    push_bounded(value, &text.decode().map_err(encoding_error)?);
                }
            }
            Event::GeneralRef(entity) if inside_text => {
                if let Some(value) = current.as_mut() {
                    push_bounded(value, &decode_reference(&entity)?);
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
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(styles)
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
    scan.features.complete = true;
    let mut cell: Option<CellBuilder> = None;
    let allow_early_stop = !options.ranges.is_empty();
    let max_requested_row = plans.iter().map(|plan| plan.bounds.end_row).max();
    let mut declared_end_row = None;
    let mut current_row: Option<u32> = None;
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
                declared_end_row = scan.dimension.as_deref().and_then(|dimension| {
                    dimension.rsplit_once(':').map_or_else(
                        || parse_cell_reference(dimension).map(|(row, _)| row),
                        |(_, end)| parse_cell_reference(end).map(|(row, _)| row),
                    )
                });
            }
            Event::Start(element) | Event::Empty(element)
                if local_name(element.name().as_ref()) == b"mergeCell" =>
            {
                if let Some(reference) = attribute(&element, b"ref")? {
                    scan.merged_ranges.push(reference);
                }
            }
            Event::Start(element) | Event::Empty(element)
                if local_name(element.name().as_ref()) == b"row" =>
            {
                current_row = attribute(&element, b"r")?.and_then(|value| value.parse().ok());
                if allow_early_stop
                    && let (Some(row), Some(max_row), Some(sheet_end_row)) =
                        (current_row, max_requested_row, declared_end_row)
                    && row > max_row
                    && sheet_end_row > max_row
                {
                    scan.features.complete = false;
                    break;
                }
                if attribute(&element, b"hidden")?
                    .as_deref()
                    .is_some_and(xml_truthy)
                {
                    scan.hidden_rows += 1;
                }
                let outline_level = attribute(&element, b"outlineLevel")?
                    .and_then(|value| value.parse::<u8>().ok())
                    .unwrap_or(0);
                if outline_level > 0 {
                    scan.features.outlined_rows += 1;
                    scan.features.max_row_outline_level =
                        scan.features.max_row_outline_level.max(outline_level);
                }
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
            }
            Event::Start(element) | Event::Empty(element)
                if local_name(element.name().as_ref()) == b"legacyDrawing" =>
            {
                scan.features.comment_drawing_parts += 1;
            }
            Event::Start(element) | Event::Empty(element)
                if matches!(
                    local_name(element.name().as_ref()),
                    b"pageSetup" | b"pageMargins" | b"printOptions"
                ) =>
            {
                scan.features.page_setup = true;
            }
            Event::Start(element) | Event::Empty(element)
                if local_name(element.name().as_ref()) == b"headerFooter" =>
            {
                scan.features.header_footer = true;
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
            Event::End(element) if local_name(element.name().as_ref()) == b"row" => {
                if allow_early_stop
                    && let (Some(row), Some(max_row), Some(sheet_end_row)) =
                        (current_row, max_requested_row, declared_end_row)
                    && row >= max_row
                    && sheet_end_row > max_row
                {
                    scan.features.complete = false;
                    break;
                }
                current_row = None;
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    Ok(scan)
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
    let raw = if cell.has_inline {
        cell.inline
    } else {
        cell.value
    };
    let mut rich_text = cell.rich_text;
    let (value_type, display) = match cell.cell_type.as_deref() {
        Some("s") => {
            let index = raw.parse::<usize>().map_err(|_| {
                ReadError::InvalidXlsx("shared string cell has an invalid index".to_owned())
            })?;
            match shared_strings.get(index) {
                Some(value) => {
                    rich_text = value.rich;
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
    output.push_str("| # | Sheet | State | Declared dimension | Semantic bounds | Selected |\n");
    output.push_str("|---:|---|---|---|---|---|\n");
    for sheet in &workbook.sheets {
        output.push_str(&format!(
            "| {} | {} | {:?} | {} | {} | {} |\n",
            sheet.index + 1,
            markdown_escape(&sheet.name),
            sheet.state,
            sheet.declared_dimension.as_deref().unwrap_or(""),
            sheet.semantic_bounds.as_deref().unwrap_or(""),
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
    if selection.truncated {
        output.push_str("\n> Selection output was truncated by the cell limit.\n");
    }
    output.push_str(&format!("<!-- opsail:xlsx-generated:{marker}:end -->\n"));
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
            "<li>{} ({:?})</li>",
            html_escape(&sheet.name),
            sheet.state
        ));
    }
    output.push_str("</ul>\n<!-- opsail:xlsx-generated:manifest:end -->\n");
}

fn render_html_selection(output: &mut String, selection: &WorkbookSelection) {
    let marker = selection_marker(&selection.requested);
    output.push_str(&format!(
        "<!-- opsail:xlsx-generated:{marker}:start -->\n<section><h2>{}!{}</h2><table><thead><tr><th>Cell</th><th>Value</th><th>Type</th><th>Formula</th></tr></thead><tbody>",
        html_escape(&selection.sheet),
        html_escape(&selection.range)
    ));
    for cell in &selection.cells {
        output.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{:?}</td><td>{}</td></tr>",
            html_escape(&cell.reference),
            html_escape(&cell.display),
            cell.value_type,
            html_escape(cell.formula.as_deref().unwrap_or(""))
        ));
    }
    output.push_str(&format!(
        "</tbody></table></section>\n<!-- opsail:xlsx-generated:{marker}:end -->\n"
    ));
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
    let (directory, file) = path
        .rsplit_once('/')
        .ok_or_else(|| ReadError::InvalidXlsx("worksheet part path has no directory".to_owned()))?;
    if file.is_empty() || directory.is_empty() {
        return Err(ReadError::InvalidXlsx(
            "worksheet part path is invalid".to_owned(),
        ));
    }
    Ok(format!("{directory}/_rels/{file}.rels"))
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
}
