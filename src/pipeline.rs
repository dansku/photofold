//! Stage-by-stage orchestration of one run.
//!
//! This module holds the order of operations and nothing else. Each stage lives
//! in its own module and is called once from here, which makes the whole program
//! readable from top to bottom in about a page.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use rayon::prelude::*;

use crate::cache::Cache;
use crate::cli::Config;
use crate::error::{ErrorLog, Result};
use crate::grouping::UnionFind;
use crate::live_photo::Pairing;
use crate::matching::Verdict;
use crate::models::{DuplicateGroup, FileId, Library, MatchConfidence, MediaKind};
use crate::planner::{self, CopyPlan, KeepUnit};
use crate::progress::Progress;
use crate::report::{self, Report};
use crate::timing::Timings;
use crate::{analyze, bucketing, copier, exact, live_photo, matching, quality, scanner};

/// What a finished run produced. Tests read this instead of parsing output.
#[derive(Debug)]
pub struct RunOutcome {
    pub report: Report,
}

pub fn run(config: &Config, progress: &Progress) -> Result<RunOutcome> {
    // One bounded pool for the whole run. `install` scopes it to this call, so
    // running the pipeline twice in one process (as the tests do) still works.
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(config.workers)
        .build()
        .expect("failed to start the worker pool");
    pool.install(|| run_inner(config, progress))
}

fn run_inner(config: &Config, progress: &Progress) -> Result<RunOutcome> {
    let errors = ErrorLog::new();
    let timings = Timings::new();

    // 1. Scan.
    let scan_started = Instant::now();
    let (mut library, scan_stats) = scanner::scan(config, &errors, progress)?;
    timings.record(
        "scan",
        scan_started.elapsed(),
        Some(scan_stats.entries_seen),
        None,
    );
    progress.println(format!(
        "Scanned {} files ({} photos, {} videos)",
        report::group_digits(library.files.len() as u64),
        report::group_digits(library.count_kind(MediaKind::Image) as u64),
        report::group_digits(library.count_kind(MediaKind::Video) as u64),
    ));

    // 2. Replay what earlier runs already measured about unchanged files.
    let mut cache = match &config.cache_path {
        Some(path) => Cache::open(path),
        None => Cache::disabled(),
    };
    cache.apply(&mut library);
    if cache.hits > 0 {
        progress.println(format!(
            "Reused earlier analysis for {} of {} files",
            report::group_digits(cache.hits as u64),
            report::group_digits(library.files.len() as u64),
        ));
    }

    // 3. Byte-for-byte duplicates. Cheap, and it needs no decoding at all.
    let exact_groups = exact::detect(&mut library, &errors, progress, &timings);
    let exact_removed: u64 = exact_groups.iter().map(|g| g.len() as u64 - 1).sum();
    progress.println(format!(
        "Exact duplicates: {} file(s)",
        report::group_digits(exact_removed)
    ));

    // 4. Metadata and visual fingerprints, in parallel.
    let analysis = analyze::run(&mut library, &errors, progress, &timings);
    if analysis.misnamed > 0 {
        progress.println(format!(
            "Note: {} file(s) do not match their extension, for example a JPEG named .HEIC. \
             Their real format was used.",
            report::group_digits(analysis.misnamed)
        ));
    }

    // 5. Find Live Photos before any deduplication, so a still and its movie are
    //    treated as one thing from here on.
    let pairing = if config.live_photos {
        live_photo::pair(&library)
    } else {
        Pairing::default()
    };
    if !pairing.is_empty() {
        progress.println(format!(
            "Live Photos: {} pair(s)",
            report::group_digits(pairing.len() as u64)
        ));
    }

    // A movie that belongs to a still never gets its own duplicate decision.
    let exact_groups: Vec<Vec<FileId>> = exact_groups
        .into_iter()
        .map(|group| {
            group
                .into_iter()
                .filter(|id| !pairing.is_paired_movie(*id))
                .collect::<Vec<FileId>>()
        })
        .filter(|group| group.len() > 1)
        .collect();

    // 6. Narrow the comparison down to plausible pairs, then judge each one.
    let verdicts = find_visual_matches(&library, config, progress, &timings);

    // 7. Combine the evidence into groups.
    let groups = build_groups(&library, &exact_groups, &verdicts, &pairing, config);
    let merged: u64 = groups
        .iter()
        .filter(|g| g.confidence == MatchConfidence::VeryHigh)
        .map(|g| g.files.len() as u64 - 1)
        .sum();
    progress.println(format!(
        "Visual duplicates: {} file(s)",
        report::group_digits(merged)
    ));

    // 8. Store what was measured, before the slow part, so an interrupted copy
    //    does not throw the analysis away.
    cache.save(&library);

    // 9. Decide what to keep and where it goes.
    let (plan, dropped_movies) = plan_keepers(&library, &groups, &pairing, config);

    // 10. Copy, unless this is a dry run.
    let outcome = copier::execute(&plan, &library, config, &errors, progress, &timings)?;

    // 11. Report.
    let mut summary = report::summarize(&library, &groups, &scan_stats);
    summary.unique_photos = count_kept(&library, &plan, MediaKind::Image);
    summary.unique_videos = count_kept(&library, &plan, MediaKind::Video);
    summary.live_photo_pairs = kept_pairs(&plan, &pairing) as u64;
    summary.redundant_live_photo_components = dropped_movies as u64;
    summary.bytes_output = plan.total_bytes(&library);
    summary.bytes_saved = summary.bytes_input.saturating_sub(summary.bytes_output);
    summary.files_copied = outcome.copied as u64;
    summary.unreadable_files = errors.len() as u64;

    let distances = distance_index(&verdicts);
    let report = report::build(report::ReportInput {
        library: &library,
        groups: &groups,
        distances: &|a, b| distances.get(&(a, b)).copied(),
        summary,
        scan: scan_stats,
        errors: errors.into_sorted_vec(),
        timings: timings.snapshot(),
        config,
    });
    report::write(&report, &config.report_path, config.report_format)?;

    Ok(RunOutcome { report })
}

/// A judged pair.
type Judged = (FileId, FileId, Verdict);

/// Bucket the images, then compare only the pairs that share a bucket.
fn find_visual_matches(
    library: &Library,
    config: &Config,
    progress: &Progress,
    timings: &Timings,
) -> Vec<Judged> {
    let (pairs, stats) = bucketing::candidates(library, config);
    if stats.oversized_buckets > 0 {
        progress.println(format!(
            "Note: {} group(s) of look-alike files were too large to compare in full, \
             so {} file(s) may keep a duplicate.",
            report::group_digits(stats.oversized_buckets),
            report::group_digits(stats.files_in_oversized_buckets),
        ));
    }

    let started = Instant::now();
    let bar = progress.bar(pairs.len() as u64, "Comparing");
    let verdicts: Vec<Judged> = pairs
        .par_iter()
        .filter_map(|&(a, b)| {
            let verdict = matching::compare(library.get(a), library.get(b), config);
            bar.inc(1);
            verdict.map(|verdict| (a, b, verdict))
        })
        .collect();
    bar.finish_and_clear();

    timings.record("match", started.elapsed(), Some(pairs.len() as u64), None);
    tracing::debug!(
        pairs = pairs.len(),
        related = verdicts.len(),
        "compared candidate pairs"
    );
    verdicts
}

/// Combine exact matches and visual matches into groups.
///
/// Groups are only formed from evidence strong enough to act on. Pairs that
/// looked related but were not certain enough become report-only groups, so the
/// user can see what was left alone and why.
fn build_groups(
    library: &Library,
    exact_groups: &[Vec<FileId>],
    verdicts: &[Judged],
    pairing: &Pairing,
    config: &Config,
) -> Vec<DuplicateGroup> {
    let mut union = UnionFind::new(library.files.len());

    for members in exact_groups {
        for pair in members.windows(2) {
            union.union(pair[0], pair[1]);
        }
    }
    for (a, b, verdict) in verdicts {
        if verdict.confidence.is_mergeable() {
            union.union(*a, *b);
        }
    }

    // A group is only as trustworthy as its weakest accepted link.
    let mut weakest: HashMap<FileId, MatchConfidence> = HashMap::new();
    for members in exact_groups {
        let root = union.find(members[0]);
        weakest
            .entry(root)
            .and_modify(|c| *c = (*c).min(MatchConfidence::Exact))
            .or_insert(MatchConfidence::Exact);
    }
    for (a, _, verdict) in verdicts {
        if verdict.confidence.is_mergeable() {
            let root = union.find(*a);
            weakest
                .entry(root)
                .and_modify(|c| *c = (*c).min(verdict.confidence))
                .or_insert(verdict.confidence);
        }
    }

    let mut groups: Vec<DuplicateGroup> = union
        .groups()
        .into_iter()
        .map(|files| {
            let root = union.find(files[0]);
            let confidence = weakest
                .get(&root)
                .copied()
                .unwrap_or(MatchConfidence::VeryHigh);
            make_group(library, files, confidence, None, pairing, config)
        })
        .collect();

    // Pairs worth reporting but not worth acting on. Only mention a pair whose
    // two files really did end up apart, so a merged group is not listed twice.
    //
    // One relationship is reported once. Merging two sources that both hold the
    // same two look-alike photos produces four such pairs, and listing all four
    // would suggest four separate findings, so they are collapsed by the groups
    // their members ended up in.
    let mut reported: HashSet<(FileId, FileId)> = HashSet::new();
    for (a, b, verdict) in verdicts {
        if verdict.confidence.is_mergeable() {
            continue;
        }
        let (root_a, root_b) = (union.find(*a), union.find(*b));
        if root_a == root_b {
            continue;
        }
        let key = (root_a.min(root_b), root_a.max(root_b));
        if !reported.insert(key) {
            continue;
        }
        groups.push(make_group(
            library,
            vec![*a, *b],
            verdict.confidence,
            Some(verdict.rule),
            pairing,
            config,
        ));
    }

    groups.sort_by(|a, b| a.id.cmp(&b.id));
    groups
}

fn make_group(
    library: &Library,
    files: Vec<FileId>,
    confidence: MatchConfidence,
    rule: Option<&'static str>,
    pairing: &Pairing,
    config: &Config,
) -> DuplicateGroup {
    let (winner, mut winner_reason) = quality::pick_winner(&files, library, config);
    if let Some(rule) = rule {
        winner_reason.insert(0, format!("kept both: {rule}"));
    }
    // Say so when the winner is a plain photo but a copy that lost was a Live
    // Photo. The winner keeps its own movie only; it never adopts another's.
    if confidence.is_mergeable()
        && pairing.movie_of(winner).is_none()
        && files
            .iter()
            .any(|id| *id != winner && pairing.movie_of(*id).is_some())
    {
        winner_reason.push(
            "warning: a copy that was removed had a Live Photo movie, and this one does not"
                .to_string(),
        );
    }
    DuplicateGroup {
        // The lowest member id is stable across runs, because the scanner sorts
        // files by path before assigning ids.
        id: format!("g{:06}-{}", files[0], confidence.as_str()),
        files,
        winner,
        confidence,
        winner_reason,
    }
}

/// Perceptual distances, looked up from either direction.
fn distance_index(verdicts: &[Judged]) -> HashMap<(FileId, FileId), u32> {
    let mut index = HashMap::new();
    for (a, b, verdict) in verdicts {
        if let Some(distance) = verdict.distance {
            index.insert((*a, *b), distance);
            index.insert((*b, *a), distance);
        }
    }
    index
}

/// Everything that is not a losing member of a merged group.
///
/// A movie paired with a still is never its own unit. It travels as the still's
/// companion, so it shares the still's fate and its output name.
///
/// Returns the plan and how many movies were dropped along with their stills.
fn plan_keepers(
    library: &Library,
    groups: &[DuplicateGroup],
    pairing: &Pairing,
    config: &Config,
) -> (CopyPlan, usize) {
    let dropped: HashSet<FileId> = groups
        .iter()
        .filter(|group| group.confidence.is_mergeable())
        .flat_map(|group| group.losers())
        .collect();

    let dropped_movies = dropped
        .iter()
        .filter(|id| pairing.movie_of(**id).is_some())
        .count();

    let units: Vec<KeepUnit> = library
        .files
        .iter()
        .filter(|file| !dropped.contains(&file.id) && !pairing.is_paired_movie(file.id))
        .map(|file| KeepUnit {
            primary: file.id,
            companions: pairing.movie_of(file.id).into_iter().collect(),
        })
        .collect();

    (planner::plan(library, &units, config), dropped_movies)
}

/// How many Live Photo pairs made it into the output.
fn kept_pairs(plan: &CopyPlan, pairing: &Pairing) -> usize {
    plan.entries
        .iter()
        .filter(|entry| pairing.movie_of(entry.file).is_some())
        .count()
}

fn count_kept(library: &Library, plan: &CopyPlan, kind: MediaKind) -> u64 {
    plan.entries
        .iter()
        .filter(|entry| library.get(entry.file).kind == kind)
        .count() as u64
}
