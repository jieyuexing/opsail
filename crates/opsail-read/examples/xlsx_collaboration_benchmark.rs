use std::error::Error;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use opsail_read::{
    CellValueType, ReadOptions, SpreadsheetReadOptions, WorkbookReadResult, WorkbookSession,
    merge_markdown_mirror,
};
use quick_xml::events::{BytesText, Event};
use quick_xml::{Reader, Writer};
use serde_json::{Value, json};
use tempfile::tempdir;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

const DEFAULT_MD_ROUNDS: usize = 4;
const DEFAULT_EDIT_SAMPLES: usize = 100;
const HIGH_EFFICIENCY_THRESHOLD: f64 = 0.8;

#[derive(Debug)]
struct Arguments {
    root: PathBuf,
    md_rounds: usize,
    edit_samples: usize,
    max_files: Option<usize>,
}

#[derive(Debug)]
struct AgentRefreshResult {
    cold_micros: u64,
    cold_bytes: usize,
    refresh_micros: u64,
    refresh_bytes: usize,
    byte_saving: f64,
    time_saving: f64,
    markdown_preserved: bool,
    semantic_equivalent: bool,
}

#[derive(Debug)]
struct HumanEditResult {
    path: PathBuf,
    cold_micros: u64,
    refresh_micros: u64,
    probe_micros: u64,
    extraction_micros: u64,
    direct_byte_saving: f64,
    direct_time_saving: f64,
    cycle_byte_saving: f64,
    cycle_time_saving: f64,
    semantic_equivalent: bool,
    markdown_preserved: bool,
    full_refresh: bool,
    changed_parts: Vec<String>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let arguments = parse_arguments()?;
    let mut workbooks = find_workbooks(&arguments.root)?;
    if let Some(max_files) = arguments.max_files {
        workbooks.truncate(max_files);
    }
    if workbooks.is_empty() {
        return Err("no non-temporary XLSX workbooks found".into());
    }

    let options = benchmark_options();
    let started = Instant::now();
    let mut valid_paths = Vec::new();
    let mut agent_results = Vec::new();
    let mut failures = Vec::new();
    for path in &workbooks {
        match benchmark_agent_refresh(path, &options, arguments.md_rounds) {
            Ok(result) => {
                valid_paths.push(path.clone());
                agent_results.push(result);
            }
            Err(error) => failures.push(json!({
                "path": relative_display(&arguments.root, path),
                "stage": "agent-md-refresh",
                "diagnostic": error.to_string(),
            })),
        }
    }

    let human_paths = evenly_spaced_by_size(&valid_paths, arguments.edit_samples)?;
    let mut human_results = Vec::new();
    let mut human_skips = Vec::new();
    for path in human_paths {
        match benchmark_human_edit(&path, &options, arguments.md_rounds) {
            Ok(Some(result)) => human_results.push(result),
            Ok(None) => human_skips.push(json!({
                "path": relative_display(&arguments.root, &path),
                "reason": "visible preview contains no editable numeric non-formula cell",
            })),
            Err(error) => failures.push(json!({
                "path": relative_display(&arguments.root, &path),
                "stage": "human-xlsx-edit",
                "diagnostic": error.to_string(),
            })),
        }
    }

    let output = build_output(
        &arguments,
        &workbooks,
        &agent_results,
        &human_results,
        &human_skips,
        &failures,
        started.elapsed(),
    );
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn parse_arguments() -> Result<Arguments, Box<dyn Error>> {
    let mut values = std::env::args().skip(1).collect::<Vec<_>>();
    let mut md_rounds = DEFAULT_MD_ROUNDS;
    let mut edit_samples = DEFAULT_EDIT_SAMPLES;
    let mut max_files = None;
    let mut index = 0;
    while index < values.len() {
        let parsed = match values[index].as_str() {
            "--md-rounds" => Some((&mut md_rounds, "--md-rounds")),
            "--edit-samples" => Some((&mut edit_samples, "--edit-samples")),
            "--max-files" => {
                let value = values
                    .get(index + 1)
                    .ok_or("--max-files requires a value")?
                    .parse::<usize>()?;
                if value == 0 {
                    return Err("--max-files must be greater than zero".into());
                }
                max_files = Some(value);
                values.drain(index..=index + 1);
                continue;
            }
            _ => None,
        };
        if let Some((destination, flag)) = parsed {
            let value = values
                .get(index + 1)
                .ok_or_else(|| format!("{flag} requires a value"))?
                .parse::<usize>()?;
            if value == 0 {
                return Err(format!("{flag} must be greater than zero").into());
            }
            *destination = value;
            values.drain(index..=index + 1);
        } else {
            index += 1;
        }
    }
    if values.len() != 1 {
        return Err(
            "usage: xlsx_collaboration_benchmark [--md-rounds N] [--edit-samples N] [--max-files N] ROOT"
                .into(),
        );
    }
    Ok(Arguments {
        root: PathBuf::from(values.remove(0)).canonicalize()?,
        md_rounds,
        edit_samples,
        max_files,
    })
}

fn benchmark_options() -> ReadOptions {
    ReadOptions {
        max_bytes: 64 * 1024 * 1024,
        spreadsheet: SpreadsheetReadOptions {
            max_expanded_bytes: 512 * 1024 * 1024,
            max_cells: 1_000,
            preview_rows: 24,
            preview_columns: 16,
            ..SpreadsheetReadOptions::default()
        },
        ..ReadOptions::default()
    }
}

fn find_workbooks(root: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let mut directories = vec![root.to_path_buf()];
    let mut workbooks = Vec::new();
    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                directories.push(entry.path());
            } else if file_type.is_file() && is_workbook(&entry.path()) {
                workbooks.push(entry.path());
            }
        }
    }
    workbooks.sort();
    Ok(workbooks)
}

fn is_workbook(path: &Path) -> bool {
    let temporary = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("~$"));
    !temporary
        && path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("xlsx"))
}

fn benchmark_agent_refresh(
    path: &Path,
    options: &ReadOptions,
    md_rounds: usize,
) -> Result<AgentRefreshResult, Box<dyn Error>> {
    let cold_started = Instant::now();
    let mut session = WorkbookSession::open(path.to_path_buf(), options.clone())?;
    let cold_micros = elapsed_micros(cold_started.elapsed());
    let cold_bytes = session.result().workbook.statistics.expanded_bytes_read;
    let expected_selections = selection_json(session.result())?;
    let mut markdown = session.result().content.clone();
    let mut refresh_micros = 0_u64;
    let mut refresh_bytes = 0_usize;
    let mut markdown_preserved = true;
    let mut semantic_equivalent = true;
    for round in 0..md_rounds {
        let note = format!("agent-manual-note-{round}");
        markdown.push_str(&format!("\n\n## Agent note {round}\n\n{note}\n"));
        let refresh_started = Instant::now();
        let refresh = session.refresh()?;
        refresh_micros = refresh_micros.saturating_add(elapsed_micros(refresh_started.elapsed()));
        refresh_bytes = refresh_bytes.saturating_add(refresh.metrics.expanded_bytes_read);
        markdown = merge_markdown_mirror(&markdown, refresh.result)?;
        markdown_preserved &= markdown.contains(&note);
        semantic_equivalent &= selection_json(refresh.result)? == expected_selections;
        if refresh.metrics.changed || !refresh.diff.unchanged {
            return Err("unchanged workbook produced a changed revision".into());
        }
    }
    let baseline_bytes = cold_bytes.saturating_mul(md_rounds);
    let baseline_micros = cold_micros.saturating_mul(md_rounds as u64);
    Ok(AgentRefreshResult {
        cold_micros,
        cold_bytes,
        refresh_micros,
        refresh_bytes,
        byte_saving: saving(baseline_bytes as f64, refresh_bytes as f64),
        time_saving: saving(baseline_micros as f64, refresh_micros as f64),
        markdown_preserved,
        semantic_equivalent,
    })
}

fn evenly_spaced_by_size(
    paths: &[PathBuf],
    requested: usize,
) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    if paths.is_empty() || requested == 0 {
        return Ok(Vec::new());
    }
    let mut sized = paths
        .iter()
        .map(|path| Ok((std::fs::metadata(path)?.len(), path.clone())))
        .collect::<Result<Vec<_>, std::io::Error>>()?;
    sized.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    let count = requested.min(sized.len());
    let mut selected = Vec::with_capacity(count);
    for index in 0..count {
        let position = ((2 * index + 1) * sized.len()) / (2 * count);
        selected.push(sized[position.min(sized.len() - 1)].1.clone());
    }
    Ok(selected)
}

fn benchmark_human_edit(
    source: &Path,
    options: &ReadOptions,
    md_rounds: usize,
) -> Result<Option<HumanEditResult>, Box<dyn Error>> {
    let directory = tempdir()?;
    let path = directory.path().join("collaboration.xlsx");
    std::fs::copy(source, &path)?;
    let cold_started = Instant::now();
    let mut session = WorkbookSession::open(path.clone(), options.clone())?;
    let initial_cold_micros = elapsed_micros(cold_started.elapsed());
    let initial_cold_bytes = session.result().workbook.statistics.expanded_bytes_read;
    let Some((sheet_part, cell_reference, replacement)) = editable_numeric_cell(session.result())
    else {
        return Ok(None);
    };
    let mut markdown = format!(
        "{}\n\n## Agent analysis\n\nmanual-analysis-must-survive\n",
        session.result().content
    );
    let mut agent_refresh_micros = 0_u64;
    let mut agent_refresh_bytes = 0_usize;
    for round in 0..md_rounds {
        let note = format!("human-cycle-agent-note-{round}");
        markdown.push_str(&format!("\n{note}\n"));
        let started = Instant::now();
        let refresh = session.refresh()?;
        agent_refresh_micros =
            agent_refresh_micros.saturating_add(elapsed_micros(started.elapsed()));
        agent_refresh_bytes =
            agent_refresh_bytes.saturating_add(refresh.metrics.expanded_bytes_read);
        markdown = merge_markdown_mirror(&markdown, refresh.result)?;
        if !markdown.contains(&note) {
            return Err("agent-authored Markdown was lost before the human edit".into());
        }
    }

    rewrite_numeric_cell(&path, &sheet_part, &cell_reference, &replacement)?;
    let refresh_started = Instant::now();
    let refresh = session.refresh()?;
    let refresh_micros = elapsed_micros(refresh_started.elapsed());
    markdown = merge_markdown_mirror(&markdown, refresh.result)?;

    let cold_after_started = Instant::now();
    let cold_after = WorkbookSession::open(path, options.clone())?;
    let cold_after_micros = elapsed_micros(cold_after_started.elapsed());
    let cold_after_bytes = cold_after.result().workbook.statistics.expanded_bytes_read;
    let semantic_equivalent =
        selection_json(refresh.result)? == selection_json(cold_after.result())?;
    let markdown_preserved = markdown.contains("manual-analysis-must-survive")
        && (0..md_rounds)
            .all(|round| markdown.contains(&format!("human-cycle-agent-note-{round}")))
        && refresh
            .result
            .workbook
            .selections
            .iter()
            .flat_map(|selection| &selection.cells)
            .any(|cell| cell.reference == cell_reference && cell.value == replacement);

    let cycle_baseline_bytes = initial_cold_bytes
        .saturating_mul(md_rounds)
        .saturating_add(cold_after_bytes);
    let cycle_incremental_bytes =
        agent_refresh_bytes.saturating_add(refresh.metrics.expanded_bytes_read);
    let cycle_baseline_micros = initial_cold_micros
        .saturating_mul(md_rounds as u64)
        .saturating_add(cold_after_micros);
    let cycle_incremental_micros = agent_refresh_micros.saturating_add(refresh_micros);
    Ok(Some(HumanEditResult {
        path: source.to_path_buf(),
        cold_micros: cold_after_micros,
        refresh_micros,
        probe_micros: refresh.metrics.probe_duration_micros,
        extraction_micros: refresh.result.extraction.duration_micros,
        direct_byte_saving: saving(
            cold_after_bytes as f64,
            refresh.metrics.expanded_bytes_read as f64,
        ),
        direct_time_saving: saving(cold_after_micros as f64, refresh_micros as f64),
        cycle_byte_saving: saving(cycle_baseline_bytes as f64, cycle_incremental_bytes as f64),
        cycle_time_saving: saving(
            cycle_baseline_micros as f64,
            cycle_incremental_micros as f64,
        ),
        semantic_equivalent,
        markdown_preserved,
        full_refresh: refresh.metrics.full_refresh,
        changed_parts: refresh.diff.changed_parts,
    }))
}

fn editable_numeric_cell(result: &WorkbookReadResult) -> Option<(String, String, String)> {
    for selection in &result.workbook.selections {
        let sheet = result
            .workbook
            .sheets
            .iter()
            .find(|sheet| sheet.name == selection.sheet)?;
        for cell in &selection.cells {
            if matches!(cell.value_type, CellValueType::Number)
                && cell.formula_kind.is_none()
                && let Ok(value) = cell.value.parse::<f64>()
                && value.is_finite()
            {
                return Some((
                    sheet.part.clone(),
                    cell.reference.clone(),
                    format_numeric_replacement(value + 1.0),
                ));
            }
        }
    }
    None
}

fn format_numeric_replacement(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        value.to_string()
    }
}

fn rewrite_numeric_cell(
    path: &Path,
    target_part: &str,
    cell_reference: &str,
    replacement: &str,
) -> Result<(), Box<dyn Error>> {
    let file = File::open(path)?;
    let mut archive = ZipArchive::new(file)?;
    let rewritten_path = path.with_extension("opsail-rewrite.tmp");
    let rewritten_file = File::create(&rewritten_path)?;
    let mut writer = ZipWriter::new(rewritten_file);
    let mut replaced = false;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let name = entry.name().to_owned();
        let is_directory = entry.is_dir();
        let compression = entry.compression();
        if !matches!(
            compression,
            CompressionMethod::Stored | CompressionMethod::Deflated
        ) {
            return Err(format!("unsupported ZIP compression method {compression:?}").into());
        }
        let modified = entry.last_modified();
        let permissions = entry.unix_mode();
        let mut bytes = Vec::with_capacity(usize::try_from(entry.size())?);
        entry.read_to_end(&mut bytes)?;
        if name == target_part {
            bytes = replace_cell_value(&bytes, cell_reference, replacement)?;
            replaced = true;
        }
        let mut options = SimpleFileOptions::default().compression_method(compression);
        if let Some(modified) = modified {
            options = options.last_modified_time(modified);
        }
        if let Some(permissions) = permissions {
            options = options.unix_permissions(permissions);
        }
        if is_directory {
            writer.add_directory(name, options)?;
        } else {
            writer.start_file(name, options)?;
            writer.write_all(&bytes)?;
        }
    }
    writer.finish()?;
    if !replaced {
        return Err(format!("worksheet part `{target_part}` was not found").into());
    }
    std::fs::rename(rewritten_path, path)?;
    Ok(())
}

fn replace_cell_value(
    xml: &[u8],
    cell_reference: &str,
    replacement: &str,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut reader = Reader::from_reader(xml);
    let mut writer = Writer::new(Vec::with_capacity(xml.len()));
    let mut buffer = Vec::new();
    let mut in_target_cell = false;
    let mut in_value = false;
    let mut replaced = false;
    loop {
        let event = reader.read_event_into(&mut buffer)?;
        match &event {
            Event::Start(start) if start.local_name().as_ref() == b"c" => {
                in_target_cell = start.attributes().filter_map(Result::ok).any(|attribute| {
                    attribute.key.local_name().as_ref() == b"r"
                        && attribute.value.as_ref() == cell_reference.as_bytes()
                });
                writer.write_event(event.into_owned())?;
            }
            Event::Start(start) if in_target_cell && start.local_name().as_ref() == b"v" => {
                in_value = true;
                writer.write_event(event.into_owned())?;
            }
            Event::Text(_) if in_target_cell && in_value => {
                writer.write_event(Event::Text(BytesText::new(replacement)))?;
                replaced = true;
            }
            Event::End(end) if end.local_name().as_ref() == b"v" => {
                in_value = false;
                writer.write_event(event.into_owned())?;
            }
            Event::End(end) if end.local_name().as_ref() == b"c" => {
                in_target_cell = false;
                writer.write_event(event.into_owned())?;
            }
            Event::Eof => break,
            _ => writer.write_event(event.into_owned())?,
        }
        buffer.clear();
    }
    if !replaced {
        return Err(format!("cell `{cell_reference}` did not contain a numeric value").into());
    }
    Ok(writer.into_inner())
}

fn selection_json(result: &WorkbookReadResult) -> Result<String, serde_json::Error> {
    serde_json::to_string(&result.workbook.selections)
}

fn build_output(
    arguments: &Arguments,
    workbooks: &[PathBuf],
    agent_results: &[AgentRefreshResult],
    human_results: &[HumanEditResult],
    human_skips: &[Value],
    failures: &[Value],
    wall: Duration,
) -> Value {
    let agent_efficient = agent_results
        .iter()
        .filter(|result| {
            result.byte_saving >= HIGH_EFFICIENCY_THRESHOLD
                && result.time_saving >= HIGH_EFFICIENCY_THRESHOLD
                && result.markdown_preserved
                && result.semantic_equivalent
        })
        .count();
    let direct_human_efficient = human_results
        .iter()
        .filter(|result| {
            result.direct_byte_saving >= HIGH_EFFICIENCY_THRESHOLD
                && result.direct_time_saving >= HIGH_EFFICIENCY_THRESHOLD
                && result.markdown_preserved
                && result.semantic_equivalent
        })
        .count();
    let cycle_efficient = human_results
        .iter()
        .filter(|result| {
            result.cycle_byte_saving >= HIGH_EFFICIENCY_THRESHOLD
                && result.cycle_time_saving >= HIGH_EFFICIENCY_THRESHOLD
                && result.markdown_preserved
                && result.semantic_equivalent
        })
        .count();
    let agent_times = agent_results
        .iter()
        .map(|result| result.time_saving)
        .collect::<Vec<_>>();
    let agent_bytes = agent_results
        .iter()
        .map(|result| result.byte_saving)
        .collect::<Vec<_>>();
    let human_direct_times = human_results
        .iter()
        .map(|result| result.direct_time_saving)
        .collect::<Vec<_>>();
    let human_direct_bytes = human_results
        .iter()
        .map(|result| result.direct_byte_saving)
        .collect::<Vec<_>>();
    let cycle_times = human_results
        .iter()
        .map(|result| result.cycle_time_saving)
        .collect::<Vec<_>>();
    let cycle_bytes = human_results
        .iter()
        .map(|result| result.cycle_byte_saving)
        .collect::<Vec<_>>();
    json!({
        "schemaVersion": 1,
        "benchmark": "opsail-xlsx-human-agent-collaboration",
        "corpusRoot": arguments.root,
        "workload": {
            "agentMarkdownRoundsPerHumanXlsxEdit": arguments.md_rounds,
            "humanEditRequestedSamples": arguments.edit_samples,
            "humanEditActualSamples": human_results.len(),
            "humanEditSkippedSamples": human_skips.len(),
            "humanEditMutation": "one visible numeric non-formula cell in a temporary workbook copy",
        },
        "acceptance": {
            "thresholdPercent": HIGH_EFFICIENCY_THRESHOLD * 100.0,
            "compatibility": {
                "eligibleWorkbooks": workbooks.len(),
                "successfulWorkbooks": agent_results.len(),
                "actualPercent": percent(agent_results.len(), workbooks.len()),
            },
            "agentMarkdownRefresh": {
                "metric": "workbooks preserving Markdown and selections with both byte and time savings >= 80%",
                "actualPercent": percent(agent_efficient, agent_results.len()),
                "passed": percent(agent_efficient, agent_results.len()) >= 80.0,
            },
            "directHumanXlsxRefresh": {
                "metric": "edited samples equivalent to cold read with both byte and time savings >= 80%",
                "actualPercent": percent(direct_human_efficient, human_results.len()),
                "passed": percent(direct_human_efficient, human_results.len()) >= 80.0,
            },
            "collaborationCycle": {
                "metric": "declared repeated-MD plus one-XLSX-edit cycles with both byte and time savings >= 80%",
                "actualPercent": percent(cycle_efficient, human_results.len()),
                "passed": percent(cycle_efficient, human_results.len()) >= 80.0,
            },
        },
        "performance": {
            "wallMs": wall.as_secs_f64() * 1000.0,
            "agentMarkdownRefresh": distribution(&agent_bytes, &agent_times),
            "directHumanXlsxRefresh": distribution(&human_direct_bytes, &human_direct_times),
            "collaborationCycle": distribution(&cycle_bytes, &cycle_times),
            "agentColdReadMicros": duration_distribution(agent_results.iter().map(|result| result.cold_micros)),
            "agentRefreshMicros": duration_distribution(agent_results.iter().map(|result| result.refresh_micros)),
            "agentColdExpandedBytes": duration_distribution(agent_results.iter().map(|result| result.cold_bytes as u64)),
            "agentRefreshExpandedBytes": duration_distribution(agent_results.iter().map(|result| result.refresh_bytes as u64)),
        },
        "correctness": {
            "agentMarkdownPreserved": agent_results.iter().filter(|result| result.markdown_preserved).count(),
            "agentSemanticEquivalent": agent_results.iter().filter(|result| result.semantic_equivalent).count(),
            "humanMarkdownPreserved": human_results.iter().filter(|result| result.markdown_preserved).count(),
            "humanSemanticEquivalent": human_results.iter().filter(|result| result.semantic_equivalent).count(),
            "humanFullRefreshes": human_results.iter().filter(|result| result.full_refresh).count(),
        },
        "humanSamples": human_results.iter().map(|result| json!({
            "path": relative_display(&arguments.root, &result.path),
            "coldMicros": result.cold_micros,
            "refreshMicros": result.refresh_micros,
            "probeMicros": result.probe_micros,
            "extractionMicros": result.extraction_micros,
            "directByteSavingPercent": result.direct_byte_saving * 100.0,
            "directTimeSavingPercent": result.direct_time_saving * 100.0,
            "cycleByteSavingPercent": result.cycle_byte_saving * 100.0,
            "cycleTimeSavingPercent": result.cycle_time_saving * 100.0,
            "semanticEquivalent": result.semantic_equivalent,
            "markdownPreserved": result.markdown_preserved,
            "fullRefresh": result.full_refresh,
            "changedParts": result.changed_parts,
        })).collect::<Vec<_>>(),
        "humanSkips": human_skips,
        "failures": failures.iter().take(20).collect::<Vec<_>>(),
        "failureCount": failures.len(),
    })
}

fn distribution(byte_savings: &[f64], time_savings: &[f64]) -> Value {
    json!({
        "expandedByteSavingPercent": {
            "p50": percentile(byte_savings, 0.50) * 100.0,
            "p95": percentile(byte_savings, 0.95) * 100.0,
        },
        "timeSavingPercent": {
            "p50": percentile(time_savings, 0.50) * 100.0,
            "p95": percentile(time_savings, 0.95) * 100.0,
        },
    })
}

fn duration_distribution(values: impl Iterator<Item = u64>) -> Value {
    let values = values.map(|value| value as f64).collect::<Vec<_>>();
    json!({
        "p50": percentile(&values, 0.50),
        "p95": percentile(&values, 0.95),
    })
}

fn percentile(values: &[f64], fraction: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let index = ((sorted.len() as f64 * fraction).ceil() as usize)
        .saturating_sub(1)
        .min(sorted.len() - 1);
    sorted[index]
}

fn saving(baseline: f64, incremental: f64) -> f64 {
    if baseline <= 0.0 {
        0.0
    } else {
        1.0 - incremental / baseline
    }
}

fn percent(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64 * 100.0
    }
}

fn elapsed_micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}
