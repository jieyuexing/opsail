//! Bounded, loss-conscious XLSX inspection and candidate editing.
//! This module models stored OOXML, not Excel's rendering/calculation engine.
mod package;
mod references;
mod styles;
#[cfg(test)]
mod tests;
mod workbook;
mod xml;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("operation {index} ({op} {sheet}!{target}): {message}")]
    Operation {
        index: usize,
        op: String,
        sheet: String,
        target: String,
        message: String,
    },
    #[error("invalid request: {0}")]
    Request(String),
    #[error("invalid or unsupported XLSX: {0}")]
    Xlsx(String),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("ZIP: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("JSON: {0}")]
    Json(#[from] serde_json::Error),
}
impl Error {
    /// Machine-readable details, retaining the complete human-readable message.
    pub fn details(&self) -> Value {
        let mut value = json!({"message": self.to_string()});
        if let Self::Operation {
            index,
            op,
            sheet,
            target,
            ..
        } = self
        {
            value["operationIndex"] = json!(index);
            value["op"] = json!(op);
            value["sheet"] = json!(sheet);
            value["target"] = json!(target);
        }
        value
    }
}
type Result<T> = std::result::Result<T, Error>;
fn invalid(message: impl Into<String>) -> Error {
    Error::Xlsx(message.into())
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    schema_version: u8,
    operation: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    ranges: Vec<String>,
    #[serde(default = "default_cells")]
    max_cells: usize,
    max_bytes: Option<u64>,
    max_expanded_bytes: Option<u64>,
    output: Option<String>,
    #[serde(default)]
    validate_only: bool,
    #[serde(default)]
    detail: Detail,
    include_parts: Option<bool>,
    expected_sha256: Option<String>,
    #[serde(default)]
    operations: Vec<Operation>,
    before: Option<String>,
    after: Option<String>,
}
#[derive(Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum Detail {
    #[default]
    Full,
    Compact,
}
fn default_cells() -> usize {
    200
}
#[derive(Deserialize)]
#[serde(
    tag = "op",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum Operation {
    InsertRows {
        sheet: String,
        before: u32,
        count: u32,
        #[serde(default)]
        style_from: RowStyle,
    },
    SetFormula {
        sheet: String,
        cell: String,
        expected_text: String,
        formula: String,
    },
    SetNumber {
        sheet: String,
        cell: String,
        expected_text: String,
        value: f64,
    },
    AppendText {
        sheet: String,
        cell: String,
        expected_text: String,
        value: String,
        font_color: Option<String>,
        bold: Option<bool>,
        strike: Option<bool>,
    },
    SetText {
        sheet: String,
        cell: String,
        expected_text: String,
        value: String,
    },
    SetStyle {
        sheet: String,
        range: String,
        style: Style,
    },
    CopyStyle {
        sheet: String,
        range: String,
        from_cell: String,
        components: Vec<String>,
    },
    RowHeight {
        sheet: String,
        row: u32,
        height: f64,
    },
    ColumnWidth {
        sheet: String,
        column: String,
        width: f64,
    },
    RowVisibility {
        sheet: String,
        row: u32,
        hidden: bool,
    },
}
#[derive(Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum RowStyle {
    #[default]
    Above,
    None,
}
impl Operation {
    fn sheet(&self) -> &str {
        match self {
            Self::InsertRows { sheet, .. }
            | Self::SetText { sheet, .. }
            | Self::SetFormula { sheet, .. }
            | Self::SetNumber { sheet, .. }
            | Self::AppendText { sheet, .. }
            | Self::SetStyle { sheet, .. }
            | Self::CopyStyle { sheet, .. }
            | Self::RowHeight { sheet, .. }
            | Self::ColumnWidth { sheet, .. }
            | Self::RowVisibility { sheet, .. } => sheet,
        }
    }
    fn location(&self) -> (&str, String) {
        match self {
            Self::InsertRows { before, .. } => ("insertRows", before.to_string()),
            Self::SetText { cell, .. } => ("setText", cell.clone()),
            Self::SetFormula { cell, .. } => ("setFormula", cell.clone()),
            Self::SetNumber { cell, .. } => ("setNumber", cell.clone()),
            Self::AppendText { cell, .. } => ("appendText", cell.clone()),
            Self::SetStyle { range, .. } => ("setStyle", range.clone()),
            Self::CopyStyle { range, .. } => ("copyStyle", range.clone()),
            Self::RowHeight { row, .. } => ("rowHeight", row.to_string()),
            Self::RowVisibility { row, .. } => ("rowVisibility", row.to_string()),
            Self::ColumnWidth { column, .. } => ("columnWidth", column.clone()),
        }
    }
    fn error(&self, index: usize, error: Error) -> Error {
        let (op, target) = self.location();
        Error::Operation {
            index,
            op: op.into(),
            sheet: self.sheet().into(),
            target,
            message: error.to_string(),
        }
    }
}
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Style {
    font_color: Option<String>,
    fill_color: Option<String>,
    bold: Option<bool>,
    strike: Option<bool>,
    wrap_text: Option<bool>,
    horizontal: Option<String>,
    vertical: Option<String>,
    indent: Option<u32>,
}
fn absolute(s: &str) -> Result<&Path> {
    if !Path::new(s).is_absolute() || s.contains('\0') {
        return Err(Error::Request("paths must be absolute local paths".into()));
    }
    Ok(Path::new(s))
}
/// Versioned interface shared by native CLI and thin Host adapters.
pub fn execute(value: Value) -> Result<Value> {
    if serde_json::to_vec(&value)?.len() > 1024 * 1024 {
        return Err(Error::Request("request exceeds 1 MiB".into()));
    }
    let req: Request = serde_json::from_value(value.clone())?;
    if req.schema_version != 1 {
        return Err(Error::Request("schemaVersion must be 1".into()));
    }
    let common = ["schemaVersion", "operation", "maxBytes", "maxExpandedBytes"];
    let extra: &[&str] = match req.operation.as_str() {
        "inspect" => &["source", "ranges", "maxCells", "detail", "includeParts"],
        "patch" => &[
            "source",
            "output",
            "expectedSha256",
            "operations",
            "validateOnly",
        ],
        "diff" => &["before", "after", "maxCells"],
        _ => {
            return Err(Error::Request(
                "operation must be inspect, patch or diff".into(),
            ));
        }
    };
    for key in value.as_object().unwrap().keys() {
        if !common.contains(&key.as_str()) && !extra.contains(&key.as_str()) {
            return Err(Error::Request(format!(
                "{key} is not valid for {}",
                req.operation
            )));
        }
    }
    let limits = package::Limits::new(req.max_bytes, req.max_expanded_bytes)?;
    if !(1..=2000).contains(&req.max_cells) {
        return Err(Error::Request("maxCells must be 1-2000".into()));
    }
    match req.operation.as_str() {
        "inspect" => {
            if !(1..=32).contains(&req.ranges.len()) || req.ranges.iter().any(|r| r.len() > 256) {
                return Err(Error::Request(
                    "inspect requires 1-32 bounded ranges".into(),
                ));
            }
            let pkg = package::Package::read(absolute(&req.source)?, limits)?;
            let book = workbook::Book::load(&pkg)?;
            let mut report =
                book.inspect(&req.ranges, req.max_cells, req.detail == Detail::Compact)?;
            report["sourceSha256"] = json!(pkg.sha());
            if req.include_parts.unwrap_or(req.detail == Detail::Full) {
                report["parts"] = json!(pkg.inventory());
            }
            envelope("inspect", report)
        }
        "diff" => {
            let before = package::Package::read(
                absolute(
                    req.before
                        .as_deref()
                        .ok_or_else(|| Error::Request("before is required".into()))?,
                )?,
                limits,
            )?;
            let after = package::Package::read(
                absolute(
                    req.after
                        .as_deref()
                        .ok_or_else(|| Error::Request("after is required".into()))?,
                )?,
                limits,
            )?;
            let mut report = workbook::Book::load(&before)?
                .diff(&workbook::Book::load(&after)?, req.max_cells)?;
            report["beforeSha256"] = json!(before.sha());
            report["afterSha256"] = json!(after.sha());
            report["changedParts"] = json!(before.changed_parts(&after));
            report["unmodeledChangedParts"] = json!(
                before
                    .changed_parts(&after)
                    .into_iter()
                    .filter(|p| !workbook::modeled_part(p))
                    .collect::<Vec<_>>()
            );
            envelope("diff", report)
        }
        "patch" => {
            if !(1..=256).contains(&req.operations.len()) {
                return Err(Error::Request("operations must contain 1-256 edits".into()));
            }
            let source = absolute(&req.source)?;
            let output = if req.validate_only {
                None
            } else {
                Some(absolute(req.output.as_deref().ok_or_else(|| {
                    Error::Request("output is required".into())
                })?)?)
            };
            if output.is_some_and(|path| path.symlink_metadata().is_ok()) {
                return Err(Error::Request(
                    "output already exists; use a new candidate path".into(),
                ));
            }
            let pkg = package::Package::read(source, limits)?;
            let hash = pkg.sha();
            if req
                .expected_sha256
                .as_ref()
                .map(|s| s.to_ascii_lowercase())
                .as_deref()
                != Some(hash.as_str())
            {
                return Err(Error::Request(
                    "expectedSha256 does not match source".into(),
                ));
            }
            let mut book = workbook::Book::load(&pkg)?;
            let mut count = 0;
            let mut violations = Vec::new();
            let mut rows_inserted = Vec::new();
            for (index, op) in req.operations.iter().enumerate() {
                match book.apply_bounded(op, 10000 - count, req.validate_only) {
                    Ok(processed) => {
                        count += processed;
                        if let Operation::InsertRows {
                            sheet,
                            before,
                            count,
                            ..
                        } = op
                        {
                            rows_inserted
                                .push(json!({"sheet":sheet,"before":before,"count":count}));
                        }
                    }
                    Err(error) => {
                        let error = op.error(index, error);
                        if !req.validate_only {
                            return Err(error);
                        }
                        violations.push(error.details());
                    }
                }
            }
            let updates = book.updates(&pkg)?;
            let changed: Vec<_> = updates.keys().cloned().collect();
            if req.validate_only {
                return envelope(
                    "patch",
                    json!({"validateOnly":true,"sourceSha256":hash,
                    "violations":violations,"operationsChecked":req.operations.len(),
                    "targetsProcessed":count,"wouldChangeParts":changed,"rowsInserted":rows_inserted}),
                );
            }
            let output = output.unwrap();
            // Validate even unusually large part-name metadata before writing
            // a candidate. After publication, substituting the fixed-width SHA
            // cannot turn a successful patch into an output-budget failure.
            let mut report = envelope(
                "patch",
                json!({"sourceSha256":hash,"candidateSha256":"0".repeat(64),"output":output,"changedParts":changed,"operationsApplied":req.operations.len(),"targetsProcessed":count,"rowsInserted":rows_inserted,"sourceUnchangedAtPublish":true}),
            )?;
            let candidate = pkg.publish(source, output, &updates, limits)?;
            report["candidateSha256"] = json!(candidate);
            Ok(report)
        }
        _ => unreachable!(),
    }
}
fn envelope(operation: &str, mut v: Value) -> Result<Value> {
    v["schemaVersion"] = json!(1);
    v["operation"] = json!(operation);
    v["protocolFeatures"] = json!([
        "createCells",
        "setNumber",
        "appendText",
        "copyStyleAdoptBase",
        "validateOnly",
        "compactInspect",
        "setFormula",
        "insertRows"
    ]);
    v["visualVerification"] = json!("pending");
    v["proofBoundary"] = json!(
        "Stored OOXML only; conditional formatting, native objects, layout, printing, formula recalculation and business acceptance require separate verification."
    );
    limit_details(operation, &mut v, 8 * 1024 * 1024)?;
    Ok(v)
}
fn encoded_len(value: &Value) -> Result<usize> {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Counter(0);
    serde_json::to_writer(&mut count, value)?;
    Ok(count.0)
}
fn limit_details(operation: &str, v: &mut Value, limit: usize) -> Result<()> {
    if encoded_len(v)? <= limit {
        return Ok(());
    }
    let pointer = match operation {
        "inspect" => "/cells",
        "diff" => "/cellChanges/details",
        _ => return Err(invalid("response metadata exceeds output byte limit")),
    };
    let details = std::mem::take(
        v.pointer_mut(pointer)
            .and_then(Value::as_array_mut)
            .ok_or_else(|| invalid("response details missing"))?,
    );
    if operation == "inspect" {
        v["truncated"] = json!(true)
    } else {
        v["cellChanges"]["truncated"] = json!(true)
    }
    v["outputTruncated"] = json!(true);
    let mut size = encoded_len(v)?;
    if size > limit {
        return Err(invalid(
            "response metadata exceeds output byte limit; use a smaller workbook",
        ));
    }
    let mut kept = Vec::new();
    for value in details {
        let addition = encoded_len(&value)? + usize::from(!kept.is_empty());
        if size + addition > limit {
            break;
        }
        kept.push(value);
        size += addition;
    }
    *v.pointer_mut(pointer).unwrap() = Value::Array(kept);
    Ok(())
}

#[cfg(test)]
mod output_tests {
    use super::*;
    #[test]
    fn output_budget_keeps_the_same_ordered_prefix_and_totals() {
        for operation in ["inspect", "diff"] {
            let cells: Vec<_> = (0..40)
                .map(|i| json!({"cell":i,"text":"中文\n\"".repeat(20)}))
                .collect();
            let mut response = if operation == "inspect" {
                json!({"cells":cells,"totalCells":40,"truncated":false})
            } else {
                json!({"cellChanges":{"total":40,"details":cells,"truncated":false}})
            };
            let mut old = response.clone();
            let pointer = if operation == "inspect" {
                "/cells"
            } else {
                "/cellChanges/details"
            };
            while serde_json::to_vec(&old).unwrap().len() > 1000 {
                old.pointer_mut(pointer)
                    .unwrap()
                    .as_array_mut()
                    .unwrap()
                    .pop();
                if operation == "inspect" {
                    old["truncated"] = json!(true)
                } else {
                    old["cellChanges"]["truncated"] = json!(true)
                }
                old["outputTruncated"] = json!(true);
            }
            limit_details(operation, &mut response, 1000).unwrap();
            assert_eq!(response, old);
            assert!(serde_json::to_vec(&response).unwrap().len() <= 1000);
            assert!(
                !response
                    .pointer(pointer)
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        }
    }
    #[test]
    fn metadata_alone_cannot_bypass_output_budget() {
        let mut response = json!({"cells":[],"totalCells":0,"styleContext":"x".repeat(2000)});
        assert!(limit_details("inspect", &mut response, 1000).is_err());
    }
}
