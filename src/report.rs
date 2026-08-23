//! The run report: JSON, CSV, and the summary printed at the end.
//!
//! Every merge decision appears here, including the ones that were *not* acted
//! on. A group that was only `probable` is reported with `merged: false`, so it
//! is possible to see what the program declined to combine.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::cli::{Config, ReportFormat};
use crate::error::{Error, FileError, Result};
use crate::models::{DuplicateGroup, FileId, Library, MatchConfidence, MediaKind};
use crate::scanner::ScanStats;
use crate::timing::StageTiming;

#[derive(Debug, Default, Clone, Serialize)]
pub struct Summary {
    pub files_scanned: u64,
    pub images_scanned: u64,
    pub videos_scanned: u64,
    pub exact_duplicate_files: u64,
    pub visual_duplicate_files: u64,
    pub redundant_live_photo_components: u64,
    pub unique_photos: u64,
    pub unique_videos: u64,
    pub live_photo_pairs: u64,
    /// Groups the program found but chose not to merge, because it was not sure.
    pub groups_kept_apart: u64,
    pub unreadable_files: u64,
    pub appledouble_skipped: u64,
    pub files_copied: u64,
    pub bytes_input: u64,
    pub bytes_output: u64,
    pub bytes_saved: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileRef {
    pub path: PathBuf,
    pub format: &'static str,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DuplicateRef {
    #[serde(flatten)]
    pub file: FileRef,
    /// Hamming distance between the two visual fingerprints, when both had one.
    /// Absent for byte-identical files, which need no visual comparison.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub perceptual_distance: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GroupReport {
    pub id: String,
    pub confidence: &'static str,
    /// False when the group was reported but both files were still copied.
    pub merged: bool,
    pub winner: FileRef,
    pub winner_reason: Vec<String>,
    pub duplicates: Vec<DuplicateRef>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub tool: &'static str,
    pub version: &'static str,
    pub generated_at: String,
    pub dry_run: bool,
    pub sources: Vec<PathBuf>,
    pub output: PathBuf,
    pub summary: Summary,
    pub scan: ScanStats,
    pub groups: Vec<GroupReport>,
    pub errors: Vec<FileError>,
    pub timings: Vec<StageTiming>,
}

/// Everything the report needs, gathered in one value.
pub struct ReportInput<'a> {
    pub library: &'a Library,
    pub groups: &'a [DuplicateGroup],
    /// Visual distance between two files, when both had a fingerprint.
    pub distances: &'a dyn Fn(FileId, FileId) -> Option<u32>,
    pub summary: Summary,
    pub scan: ScanStats,
    pub errors: Vec<FileError>,
    pub timings: Vec<StageTiming>,
    pub config: &'a Config,
}

/// Build the report body from the finished pipeline state.
pub fn build(input: ReportInput<'_>) -> Report {
    let ReportInput {
        library,
        groups,
        distances,
        summary,
        scan,
        errors,
        timings,
        config,
    } = input;

    let group_reports = groups
        .iter()
        .map(|group| GroupReport {
            id: group.id.clone(),
            confidence: group.confidence.as_str(),
            merged: group.confidence.is_mergeable(),
            winner: file_ref(library, group.winner),
            winner_reason: group.winner_reason.clone(),
            duplicates: group
                .losers()
                .map(|id| DuplicateRef {
                    file: file_ref(library, id),
                    perceptual_distance: distances(group.winner, id),
                })
                .collect(),
        })
        .collect();

    Report {
        tool: "photofold",
        version: env!("CARGO_PKG_VERSION"),
        generated_at: now_rfc3339(),
        dry_run: config.dry_run,
        sources: config.sources.iter().map(|s| s.path.clone()).collect(),
        output: config.output.clone(),
        summary,
        scan,
        groups: group_reports,
        errors,
        timings,
    }
}

fn file_ref(library: &Library, id: FileId) -> FileRef {
    let file = library.get(id);
    FileRef {
        path: file.path.clone(),
        format: file.format.as_str(),
        width: file.metadata.width,
        height: file.metadata.height,
        size: file.size,
    }
}

/// Write the report to disk in the requested format.
pub fn write(report: &Report, path: &Path, format: ReportFormat) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|source| Error::WriteReport {
            path: path.to_path_buf(),
            source,
        })?;
    }

    match format {
        ReportFormat::Json => {
            let file = std::fs::File::create(path).map_err(|source| Error::WriteReport {
                path: path.to_path_buf(),
                source,
            })?;
            serde_json::to_writer_pretty(std::io::BufWriter::new(file), report)?;
        }
        ReportFormat::Csv => write_csv(report, path)?,
    }
    Ok(())
}

/// One row per file, so the CSV opens cleanly in a spreadsheet.
fn write_csv(report: &Report, path: &Path) -> Result<()> {
    let mut writer = csv::Writer::from_path(path)?;
    writer.write_record([
        "group_id",
        "confidence",
        "merged",
        "role",
        "path",
        "format",
        "width",
        "height",
        "size_bytes",
        "perceptual_distance",
        "reason",
    ])?;

    for group in &report.groups {
        let merged = group.merged.to_string();
        writer.write_record([
            group.id.as_str(),
            group.confidence,
            merged.as_str(),
            "winner",
            &group.winner.path.to_string_lossy(),
            group.winner.format,
            &optional(group.winner.width),
            &optional(group.winner.height),
            &group.winner.size.to_string(),
            "",
            &group.winner_reason.join("; "),
        ])?;

        for duplicate in &group.duplicates {
            writer.write_record([
                group.id.as_str(),
                group.confidence,
                merged.as_str(),
                if group.merged {
                    "duplicate"
                } else {
                    "kept_too"
                },
                &duplicate.file.path.to_string_lossy(),
                duplicate.file.format,
                &optional(duplicate.file.width),
                &optional(duplicate.file.height),
                &duplicate.file.size.to_string(),
                &optional(duplicate.perceptual_distance),
                "",
            ])?;
        }
    }

    writer.flush().map_err(|source| Error::WriteReport {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

fn optional<T: std::fmt::Display>(value: Option<T>) -> String {
    value.map(|v| v.to_string()).unwrap_or_default()
}

/// Collect the numbers that go in the summary.
pub fn summarize(library: &Library, groups: &[DuplicateGroup], scan: &ScanStats) -> Summary {
    let mut summary = Summary {
        files_scanned: library.files.len() as u64,
        images_scanned: library.count_kind(MediaKind::Image) as u64,
        videos_scanned: library.count_kind(MediaKind::Video) as u64,
        appledouble_skipped: scan.appledouble,
        bytes_input: library.total_bytes(),
        ..Summary::default()
    };

    for group in groups {
        let removed = group.files.len() as u64 - 1;
        match group.confidence {
            MatchConfidence::Exact => summary.exact_duplicate_files += removed,
            MatchConfidence::VeryHigh => summary.visual_duplicate_files += removed,
            _ => summary.groups_kept_apart += 1,
        }
    }

    summary
}

/// The block printed to the terminal when the run finishes.
pub fn print_summary(
    out: &mut impl Write,
    report: &Report,
    config: &Config,
) -> std::io::Result<()> {
    let s = &report.summary;

    if config.dry_run {
        writeln!(out, "\nDRY RUN — nothing was copied\n")?;
    } else {
        writeln!(out, "\nPhoto Merge Complete\n")?;
    }

    writeln!(out, "Sources")?;
    for source in &report.sources {
        writeln!(out, "  {}", source.display())?;
    }

    writeln!(out, "\nScanned")?;
    writeln!(out, "  {:>12} files", group_digits(s.files_scanned))?;
    writeln!(out, "  {:>12} photos", group_digits(s.images_scanned))?;
    writeln!(out, "  {:>12} videos", group_digits(s.videos_scanned))?;

    writeln!(out, "\nDuplicates")?;
    writeln!(out, "  {:>12} exact", group_digits(s.exact_duplicate_files))?;
    writeln!(
        out,
        "  {:>12} visual",
        group_digits(s.visual_duplicate_files)
    )?;
    if s.redundant_live_photo_components > 0 {
        writeln!(
            out,
            "  {:>12} redundant Live Photo parts",
            group_digits(s.redundant_live_photo_components)
        )?;
    }
    if s.groups_kept_apart > 0 {
        writeln!(
            out,
            "  {:>12} unsure, kept both (see the report)",
            group_digits(s.groups_kept_apart)
        )?;
    }

    writeln!(out, "\nResult")?;
    writeln!(out, "  {:>12} photos", group_digits(s.unique_photos))?;
    writeln!(out, "  {:>12} videos", group_digits(s.unique_videos))?;
    if s.live_photo_pairs > 0 {
        writeln!(
            out,
            "  {:>12} Live Photos",
            group_digits(s.live_photo_pairs)
        )?;
    }

    writeln!(out, "\nStorage")?;
    writeln!(out, "  Input:  {:>10}", human_bytes(s.bytes_input))?;
    writeln!(out, "  Output: {:>10}", human_bytes(s.bytes_output))?;
    writeln!(out, "  Saved:  {:>10}", human_bytes(s.bytes_saved))?;

    if !config.dry_run {
        writeln!(out, "\nOutput\n  {}", report.output.display())?;
    }
    writeln!(out, "\nReport\n  {}", config.report_path.display())?;

    if config.verbose > 0 && !report.timings.is_empty() {
        writeln!(out, "\nTime spent")?;
        for stage in &report.timings {
            let detail = stage.throughput.as_deref().unwrap_or("");
            writeln!(
                out,
                "  {:<12} {:>7.2}s  {detail}",
                stage.stage, stage.seconds
            )?;
        }
    }

    if s.unreadable_files > 0 {
        writeln!(
            out,
            "\nCould not analyse {} file(s). They are listed in the report.",
            s.unreadable_files
        )?;
    }
    if config.dry_run {
        writeln!(out, "\nNo files were changed.")?;
    }
    writeln!(out)?;
    Ok(())
}

/// `82431` becomes `82,431`.
pub fn group_digits(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| String::from("unknown"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digits_are_grouped_in_threes() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(82_431), "82,431");
        assert_eq!(group_digits(1_234_567), "1,234,567");
    }

    #[test]
    fn bytes_are_shown_in_readable_units() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KB");
        assert_eq!(human_bytes(3 * 1024 * 1024 * 1024), "3.0 GB");
    }
}
