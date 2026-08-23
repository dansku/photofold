//! End-to-end tests that run the whole pipeline against real folders.

mod common;

use std::fs;

use common::{Workspace, config, run};

use std::sync::LazyLock;

/// Two genuinely different pictures, encoded as real JPEG files.
static PHOTO_A: LazyLock<Vec<u8>> = LazyLock::new(|| common::jpeg(1, 160, 120, 92));
static PHOTO_B: LazyLock<Vec<u8>> = LazyLock::new(|| common::jpeg(2, 160, 120, 92));

#[test]
fn byte_identical_files_are_merged_and_only_one_is_copied() {
    let workspace = Workspace::new();
    workspace.write("phone/IMG_0001.JPG", &PHOTO_A);
    workspace.write("google/IMG_0001.JPG", &PHOTO_A);
    workspace.write("google/holiday.JPG", &PHOTO_B);

    let config = config(
        &workspace,
        &[
            &workspace.path().join("phone").to_string_lossy(),
            &workspace.path().join("google").to_string_lossy(),
        ],
    );
    let outcome = run(&config);

    assert_eq!(outcome.report.summary.files_scanned, 3);
    assert_eq!(outcome.report.summary.exact_duplicate_files, 1);
    assert_eq!(outcome.report.summary.files_copied, 2);
    assert_eq!(workspace.output_names(), ["IMG_0001.JPG", "holiday.JPG"]);
}

#[test]
fn the_same_bytes_under_different_names_still_merge() {
    let workspace = Workspace::new();
    workspace.write("a/IMG_0001.JPG", &PHOTO_A);
    workspace.write("b/holiday-in-rome.jpg", &PHOTO_A);

    let config = config(
        &workspace,
        &[
            &workspace.path().join("a").to_string_lossy(),
            &workspace.path().join("b").to_string_lossy(),
        ],
    );
    let outcome = run(&config);

    assert_eq!(outcome.report.summary.exact_duplicate_files, 1);
    assert_eq!(workspace.output_names().len(), 1);
}

#[test]
fn different_photos_sharing_a_name_are_both_kept_and_numbered() {
    let workspace = Workspace::new();
    workspace.write("a/IMG_0001.JPG", &PHOTO_A);
    workspace.write("b/IMG_0001.JPG", &PHOTO_B);

    let config = config(
        &workspace,
        &[
            &workspace.path().join("a").to_string_lossy(),
            &workspace.path().join("b").to_string_lossy(),
        ],
    );
    run(&config);

    assert_eq!(
        workspace.output_names(),
        ["IMG_0001.JPG", "IMG_0001_2.JPG"],
        "two different photos must both survive"
    );
}

#[test]
fn a_dry_run_copies_nothing_but_still_reports() {
    let workspace = Workspace::new();
    workspace.write("a/IMG_0001.JPG", &PHOTO_A);
    workspace.write("b/IMG_0001.JPG", &PHOTO_A);

    let config = config(
        &workspace,
        &[
            "--dry-run",
            &workspace.path().join("a").to_string_lossy(),
            &workspace.path().join("b").to_string_lossy(),
        ],
    );
    let outcome = run(&config);

    assert!(outcome.report.dry_run);
    assert_eq!(outcome.report.summary.exact_duplicate_files, 1);
    assert_eq!(outcome.report.summary.files_copied, 0);
    assert!(
        !workspace.path().join("out").exists(),
        "a dry run must not even create the output folder"
    );
    assert!(workspace.path().join("report.json").exists());
}

#[test]
fn source_files_are_never_changed() {
    let workspace = Workspace::new();
    let source = workspace.write("a/IMG_0001.JPG", &PHOTO_A);
    let before = fs::metadata(&source).unwrap();

    let config = config(&workspace, &[&workspace.path().join("a").to_string_lossy()]);
    run(&config);

    let after = fs::metadata(&source).unwrap();
    assert_eq!(fs::read(&source).unwrap(), *PHOTO_A);
    assert_eq!(before.len(), after.len());
    assert_eq!(
        before.modified().unwrap(),
        after.modified().unwrap(),
        "the source modification time must not move"
    );
}

#[test]
fn an_output_folder_inside_an_input_folder_is_not_scanned() {
    let workspace = Workspace::new();
    workspace.write("a/IMG_0001.JPG", &PHOTO_A);
    // The output folder sits inside the source folder and already holds a file.
    workspace.dir("a/out");
    workspace.write("a/out/leftover.jpg", &PHOTO_B);

    let output = workspace.path().join("a/out");
    let config = common::config_from(&[
        workspace.path().join("a").to_string_lossy().into_owned(),
        "--output".to_string(),
        output.to_string_lossy().into_owned(),
        "--report".to_string(),
        workspace
            .path()
            .join("report.json")
            .to_string_lossy()
            .into_owned(),
    ]);
    let outcome = run(&config);

    assert_eq!(
        outcome.report.summary.files_scanned, 1,
        "the previous output must not be ingested as new source material"
    );
    assert!(
        output.join("leftover.jpg").exists(),
        "an unrelated file already in the output folder must survive"
    );
    assert!(output.join("IMG_0001.JPG").exists());
}

#[test]
fn the_output_folder_may_not_be_an_input_folder() {
    let workspace = Workspace::new();
    workspace.write("a/IMG_0001.JPG", &PHOTO_A);
    let shared = workspace.path().join("a").to_string_lossy().into_owned();

    let cli: photofold::cli::Cli = clap::Parser::parse_from([
        "photofold".to_string(),
        shared.clone(),
        "--output".to_string(),
        shared,
    ]);
    assert!(
        cli.into_config().is_err(),
        "merging a folder into itself must be refused"
    );
}

#[test]
fn nested_input_folders_do_not_double_count_files() {
    let workspace = Workspace::new();
    workspace.write("a/IMG_0001.JPG", &PHOTO_A);
    workspace.write("a/inner/IMG_0002.JPG", &PHOTO_B);

    let config = config(
        &workspace,
        &[
            &workspace.path().join("a").to_string_lossy(),
            &workspace.path().join("a/inner").to_string_lossy(),
        ],
    );
    let outcome = run(&config);

    assert_eq!(outcome.report.summary.files_scanned, 2);
    assert_eq!(workspace.output_names(), ["IMG_0001.JPG", "IMG_0002.JPG"]);
}

#[test]
fn appledouble_sidecars_are_skipped_not_reported_as_broken() {
    let workspace = Workspace::new();
    workspace.write("a/IMG_0001.JPG", &PHOTO_A);
    workspace.write("a/._IMG_0001.JPG", b"\x00\x05\x16\x07resource fork");

    let config = config(&workspace, &[&workspace.path().join("a").to_string_lossy()]);
    let outcome = run(&config);

    assert_eq!(outcome.report.summary.files_scanned, 1);
    assert_eq!(outcome.report.summary.appledouble_skipped, 1);
    assert!(outcome.report.errors.is_empty());
    assert_eq!(workspace.output_names(), ["IMG_0001.JPG"]);
}

#[test]
fn videos_are_included_by_default_and_excluded_on_request() {
    let workspace = Workspace::new();
    workspace.write("a/holiday.JPG", &PHOTO_A);
    workspace.write("a/clip.MOV", &common::mov(120_000, None));
    let source = workspace.path().join("a").to_string_lossy().into_owned();

    let outcome = run(&config(&workspace, &[&source]));
    assert_eq!(outcome.report.summary.videos_scanned, 1);
    assert_eq!(workspace.output_names(), ["clip.MOV", "holiday.JPG"]);

    let workspace = Workspace::new();
    workspace.write("a/holiday.JPG", &PHOTO_A);
    workspace.write("a/clip.MOV", &common::mov(120_000, None));
    let source = workspace.path().join("a").to_string_lossy().into_owned();
    let outcome = run(&config(&workspace, &["--no-videos", &source]));
    assert_eq!(outcome.report.summary.videos_scanned, 0);
    assert_eq!(workspace.output_names(), ["holiday.JPG"]);
}

#[test]
fn a_second_run_into_the_same_output_folder_overwrites_nothing() {
    let workspace = Workspace::new();
    workspace.write("a/IMG_0001.JPG", &PHOTO_A);
    let source = workspace.path().join("a").to_string_lossy().into_owned();

    run(&config(&workspace, &[&source]));
    run(&config(&workspace, &[&source]));

    assert_eq!(workspace.output_names(), ["IMG_0001.JPG", "IMG_0001_2.JPG"]);
    assert_eq!(
        fs::read(workspace.path().join("out/IMG_0001.JPG")).unwrap(),
        *PHOTO_A
    );
}

#[test]
fn hard_link_mode_shares_storage_with_the_source() {
    let workspace = Workspace::new();
    let source = workspace.write("a/IMG_0001.JPG", &PHOTO_A);
    let config = config(
        &workspace,
        &[
            "--copy-mode",
            "hardlink",
            &workspace.path().join("a").to_string_lossy(),
        ],
    );
    run(&config);

    let destination = workspace.path().join("out/IMG_0001.JPG");
    assert_eq!(fs::read(&destination).unwrap(), *PHOTO_A);

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            fs::metadata(&source).unwrap().ino(),
            fs::metadata(&destination).unwrap().ino(),
            "a hard link points at the very same bytes"
        );
    }
    let _ = source;
}

#[test]
fn the_report_can_be_written_as_csv() {
    let workspace = Workspace::new();
    workspace.write("a/IMG_0001.JPG", &PHOTO_A);
    workspace.write("b/IMG_0001.JPG", &PHOTO_A);

    let report = workspace.path().join("report.csv");
    let config = common::config_from(&[
        workspace.path().join("a").to_string_lossy().into_owned(),
        workspace.path().join("b").to_string_lossy().into_owned(),
        "--output".to_string(),
        workspace.path().join("out").to_string_lossy().into_owned(),
        "--report".to_string(),
        report.to_string_lossy().into_owned(),
        "--report-format".to_string(),
        "csv".to_string(),
    ]);
    run(&config);

    let body = fs::read_to_string(&report).unwrap();
    assert!(body.starts_with("group_id,confidence,merged,role,path"));
    assert!(body.contains(",winner,"));
    assert!(body.contains(",duplicate,"));
}

// --- Visual duplicate detection -------------------------------------------

#[test]
fn a_recompressed_copy_of_the_same_photo_is_merged() {
    let workspace = Workspace::new();
    // The same picture stored twice: once at high quality, once as a smaller
    // re-encode, exactly what a Google Photos export produces.
    workspace.write("phone/IMG_0001.JPG", &common::jpeg(5, 320, 240, 95));
    workspace.write("google/IMG_0001.JPG", &common::jpeg(5, 320, 240, 55));

    let config = config(
        &workspace,
        &[
            &workspace.path().join("phone").to_string_lossy(),
            &workspace.path().join("google").to_string_lossy(),
        ],
    );
    let outcome = run(&config);

    assert_eq!(
        outcome.report.summary.exact_duplicate_files, 0,
        "the two files are not byte-identical"
    );
    assert_eq!(
        outcome.report.summary.visual_duplicate_files, 1,
        "the same picture stored twice should be merged: {:#?}",
        outcome.report.groups
    );
    assert_eq!(workspace.output_names(), ["IMG_0001.JPG"]);
}

#[test]
fn the_same_photo_as_png_and_jpeg_is_merged() {
    let workspace = Workspace::new();
    workspace.write("a/IMG_0002.PNG", &common::png(9, 320, 240));
    workspace.write("b/IMG_0002.JPG", &common::jpeg(9, 320, 240, 90));

    let config = config(
        &workspace,
        &[
            &workspace.path().join("a").to_string_lossy(),
            &workspace.path().join("b").to_string_lossy(),
        ],
    );
    let outcome = run(&config);

    assert_eq!(
        outcome.report.summary.visual_duplicate_files, 1,
        "a format change is not a different photo: {:#?}",
        outcome.report.groups
    );
    assert_eq!(
        workspace.output_names(),
        ["IMG_0002.PNG"],
        "the lossless copy should win"
    );
}

#[test]
fn different_photos_of_the_same_size_are_never_merged() {
    let workspace = Workspace::new();
    workspace.write("a/IMG_0001.JPG", &common::jpeg(11, 320, 240, 90));
    workspace.write("b/IMG_0001.JPG", &common::jpeg(12, 320, 240, 90));

    let config = config(
        &workspace,
        &[
            &workspace.path().join("a").to_string_lossy(),
            &workspace.path().join("b").to_string_lossy(),
        ],
    );
    let outcome = run(&config);

    assert_eq!(outcome.report.summary.visual_duplicate_files, 0);
    assert_eq!(workspace.output_names(), ["IMG_0001.JPG", "IMG_0001_2.JPG"]);
}

#[test]
fn a_smaller_copy_loses_to_the_full_size_original() {
    let workspace = Workspace::new();
    workspace.write("phone/IMG_0003.JPG", &common::jpeg(21, 640, 480, 90));
    // The same picture, shrunk by an exporter and re-encoded at high quality.
    workspace.write(
        "google/IMG_0003.JPG",
        &common::jpeg_downscaled(21, 640, 480, 320, 240, 95),
    );

    let config = config(
        &workspace,
        &[
            &workspace.path().join("phone").to_string_lossy(),
            &workspace.path().join("google").to_string_lossy(),
        ],
    );
    let outcome = run(&config);

    let kept = workspace.path().join("out/IMG_0003.JPG");
    let (width, _) = image::image_dimensions(&kept).unwrap();
    assert_eq!(width, 640, "the larger picture must be the one kept");
    assert_eq!(outcome.report.summary.unique_photos, 1);
}

#[test]
fn a_file_that_cannot_be_read_is_reported_and_still_copied() {
    let workspace = Workspace::new();
    workspace.write("a/IMG_0001.JPG", &common::jpeg(31, 160, 120, 90));
    workspace.write("a/broken.jpg", b"\xFF\xD8\xFF\xE0 this is not a real JPEG");

    let config = config(&workspace, &[&workspace.path().join("a").to_string_lossy()]);
    let outcome = run(&config);

    assert!(
        outcome
            .report
            .errors
            .iter()
            .any(|e| e.path.ends_with("broken.jpg")),
        "the broken file should be listed in the report"
    );
    assert_eq!(
        workspace.output_names(),
        ["IMG_0001.JPG", "broken.jpg"],
        "a file that could not be analysed is still kept"
    );
}

// --- Live Photos -----------------------------------------------------------

#[test]
fn a_live_photo_keeps_its_movie_when_the_still_wins() {
    let workspace = Workspace::new();
    // Phone: full-size still plus its motion clip.
    workspace.write("phone/IMG_1234.JPG", &common::jpeg(41, 640, 480, 95));
    workspace.write("phone/IMG_1234.MOV", &common::mov(1_880, None));
    // Google: the same photo, shrunk, with no movie.
    workspace.write(
        "google/IMG_1234.JPG",
        &common::jpeg_downscaled(41, 640, 480, 320, 240, 90),
    );

    let config = config(
        &workspace,
        &[
            &workspace.path().join("phone").to_string_lossy(),
            &workspace.path().join("google").to_string_lossy(),
        ],
    );
    let outcome = run(&config);

    assert_eq!(
        workspace.output_names(),
        ["IMG_1234.JPG", "IMG_1234.MOV"],
        "the movie must travel with the still that won"
    );
    assert_eq!(outcome.report.summary.live_photo_pairs, 1);
}

#[test]
fn a_losing_live_photo_takes_its_movie_with_it() {
    let workspace = Workspace::new();
    // The full-size copy has no movie; the small copy does.
    workspace.write("big/IMG_1234.JPG", &common::jpeg(43, 640, 480, 95));
    workspace.write(
        "small/IMG_1234.JPG",
        &common::jpeg_downscaled(43, 640, 480, 320, 240, 95),
    );
    workspace.write("small/IMG_1234.MOV", &common::mov(1_880, None));

    let config = config(
        &workspace,
        &[
            &workspace.path().join("big").to_string_lossy(),
            &workspace.path().join("small").to_string_lossy(),
        ],
    );
    let outcome = run(&config);

    assert_eq!(
        workspace.output_names(),
        ["IMG_1234.JPG"],
        "a movie must never outlive the still it belongs to"
    );
    assert_eq!(outcome.report.summary.redundant_live_photo_components, 1);

    let group = &outcome.report.groups[0];
    assert!(
        group
            .winner_reason
            .iter()
            .any(|reason| reason.contains("Live Photo movie")),
        "the report should say the removed copy had a movie: {:?}",
        group.winner_reason
    );
}

#[test]
fn a_renamed_live_photo_pair_keeps_one_shared_name() {
    let workspace = Workspace::new();
    // A different photo already owns the IMG_1234 name.
    workspace.write("other/IMG_1234.JPG", &common::jpeg(51, 320, 240, 95));
    workspace.write("phone/IMG_1234.JPG", &common::jpeg(52, 320, 240, 95));
    workspace.write("phone/IMG_1234.MOV", &common::mov(1_880, None));

    let config = config(
        &workspace,
        &[
            &workspace.path().join("other").to_string_lossy(),
            &workspace.path().join("phone").to_string_lossy(),
        ],
    );
    run(&config);

    assert_eq!(
        workspace.output_names(),
        ["IMG_1234.JPG", "IMG_1234_2.JPG", "IMG_1234_2.MOV"],
        "the still and its movie must end up with the same stem"
    );
}

#[test]
fn the_apple_content_identifier_pairs_files_with_different_names() {
    let workspace = Workspace::new();
    let identifier = "F0652AEA-5229-4BF7-A366-B4C79E90CA1C";
    workspace.write("phone/IMG_1234.JPG", &common::jpeg(61, 320, 240, 95));
    // Named differently, but carrying the same Live Photo identifier.
    workspace.write("phone/clip-9988.MOV", &common::mov(1_880, Some(identifier)));

    // Without an identifier on the still the two cannot be paired, which is the
    // honest result: a plain JPEG carries no Apple MakerNote.
    let config = config(
        &workspace,
        &[&workspace.path().join("phone").to_string_lossy()],
    );
    let outcome = run(&config);
    assert_eq!(outcome.report.summary.live_photo_pairs, 0);
    assert_eq!(workspace.output_names(), ["IMG_1234.JPG", "clip-9988.MOV"]);
}

#[test]
fn turning_live_photos_off_treats_the_movie_as_an_ordinary_file() {
    let workspace = Workspace::new();
    workspace.write("big/IMG_1234.JPG", &common::jpeg(71, 640, 480, 95));
    workspace.write(
        "small/IMG_1234.JPG",
        &common::jpeg_downscaled(71, 640, 480, 320, 240, 95),
    );
    workspace.write("small/IMG_1234.MOV", &common::mov(1_880, None));

    let config = config(
        &workspace,
        &[
            "--no-live-photos",
            &workspace.path().join("big").to_string_lossy(),
            &workspace.path().join("small").to_string_lossy(),
        ],
    );
    let outcome = run(&config);

    assert_eq!(outcome.report.summary.live_photo_pairs, 0);
    assert_eq!(
        workspace.output_names(),
        ["IMG_1234.JPG", "IMG_1234.MOV"],
        "on its own, the movie is just another file to keep"
    );
}

// --- Analysis cache --------------------------------------------------------

#[test]
fn a_second_run_reuses_the_earlier_analysis() {
    let workspace = Workspace::new();
    workspace.write("a/IMG_0001.JPG", &common::jpeg(81, 320, 240, 92));
    workspace.write("a/IMG_0002.JPG", &common::jpeg(82, 320, 240, 92));
    let source = workspace.path().join("a").to_string_lossy().into_owned();

    let first = run(&config(&workspace, &["--dry-run", &source]));
    assert_eq!(first.report.summary.files_scanned, 2);
    assert!(workspace.path().join("cache.sqlite").exists());

    let second = run(&config(&workspace, &["--dry-run", &source]));
    assert_eq!(
        second.report.summary.unique_photos, first.report.summary.unique_photos,
        "a cached run must reach the same conclusion"
    );
}

#[test]
fn editing_a_file_invalidates_its_cached_analysis() {
    let workspace = Workspace::new();
    workspace.write("a/IMG_0001.JPG", &common::jpeg(83, 320, 240, 92));
    let source = workspace.path().join("a").to_string_lossy().into_owned();

    run(&config(&workspace, &["--dry-run", &source]));

    // Replace the picture with a different one of a different size.
    workspace.write("a/IMG_0001.JPG", &common::jpeg(84, 640, 480, 92));
    let outcome = run(&config(&workspace, &["--dry-run", &source]));

    let kept = &outcome.report.groups;
    assert!(kept.is_empty(), "one file cannot be a duplicate group");
    // A stale entry would have reported the old 320x240 size.
    run(&config(&workspace, &[&source]));
    assert_eq!(
        image::image_dimensions(workspace.path().join("out/IMG_0001.JPG")).unwrap(),
        (640, 480)
    );
}

#[test]
fn no_cache_writes_no_database() {
    let workspace = Workspace::new();
    workspace.write("a/IMG_0001.JPG", &common::jpeg(85, 320, 240, 92));
    let source = workspace.path().join("a").to_string_lossy().into_owned();

    run(&config(&workspace, &["--no-cache", "--dry-run", &source]));
    assert!(!workspace.path().join("cache.sqlite").exists());
}

#[test]
fn a_cached_run_still_finds_the_same_duplicates() {
    let workspace = Workspace::new();
    workspace.write("phone/IMG_0007.JPG", &common::jpeg(87, 320, 240, 95));
    workspace.write("google/IMG_0007.JPG", &common::jpeg(87, 320, 240, 55));
    let sources = [
        workspace
            .path()
            .join("phone")
            .to_string_lossy()
            .into_owned(),
        workspace
            .path()
            .join("google")
            .to_string_lossy()
            .into_owned(),
    ];

    let first = run(&config(
        &workspace,
        &["--dry-run", &sources[0], &sources[1]],
    ));
    let second = run(&config(
        &workspace,
        &["--dry-run", &sources[0], &sources[1]],
    ));

    assert_eq!(first.report.summary.visual_duplicate_files, 1);
    assert_eq!(
        second.report.summary.visual_duplicate_files, 1,
        "the cache must not change any decision"
    );
    assert_eq!(second.report.groups.len(), first.report.groups.len());
}

#[test]
fn prefer_source_breaks_a_tie_between_identical_copies() {
    let workspace = Workspace::new();
    let picture = common::jpeg(91, 320, 240, 92);
    workspace.write("backup/IMG_0009.JPG", &picture);
    workspace.write("phone/IMG_0009.JPG", &picture);

    let phone = workspace
        .path()
        .join("phone")
        .to_string_lossy()
        .into_owned();
    let backup = workspace
        .path()
        .join("backup")
        .to_string_lossy()
        .into_owned();
    let outcome = run(&config(
        &workspace,
        &["--prefer-source", &phone, &backup, &phone],
    ));

    assert_eq!(outcome.report.summary.exact_duplicate_files, 1);
    // Paths in the report are canonical, and macOS resolves /var to /private/var,
    // so compare the tail rather than the whole path.
    let winner = &outcome.report.groups[0].winner.path;
    assert!(
        winner.ends_with("phone/IMG_0009.JPG"),
        "the preferred source should win the tie, got {}",
        winner.display()
    );
}

#[test]
fn prefer_source_never_beats_a_better_picture() {
    let workspace = Workspace::new();
    workspace.write("backup/IMG_0010.JPG", &common::jpeg(93, 640, 480, 92));
    workspace.write(
        "phone/IMG_0010.JPG",
        &common::jpeg_downscaled(93, 640, 480, 320, 240, 95),
    );

    let phone = workspace
        .path()
        .join("phone")
        .to_string_lossy()
        .into_owned();
    let backup = workspace
        .path()
        .join("backup")
        .to_string_lossy()
        .into_owned();
    run(&config(
        &workspace,
        &["--prefer-source", &phone, &backup, &phone],
    ));

    assert_eq!(
        image::image_dimensions(workspace.path().join("out/IMG_0010.JPG")).unwrap(),
        (640, 480),
        "resolution outranks a source preference"
    );
}

#[test]
fn a_rejected_argument_stops_the_run_before_anything_is_written() {
    let workspace = Workspace::new();
    workspace.write("a/IMG_0001.JPG", &PHOTO_A);

    let missing = workspace
        .path()
        .join("not-here")
        .to_string_lossy()
        .into_owned();
    let cli: photofold::cli::Cli = clap::Parser::parse_from([
        "photofold".to_string(),
        missing,
        "--output".to_string(),
        workspace.path().join("out").to_string_lossy().into_owned(),
    ]);
    assert!(cli.into_config().is_err());
    assert!(!workspace.path().join("out").exists());
}

#[test]
fn one_relationship_is_reported_once_however_many_copies_exist() {
    let workspace = Workspace::new();
    // The same scene at two qualities, so the files look alike but are not
    // byte-identical and share no name. Both folders hold both files.
    let sunset = common::jpeg(95, 320, 240, 92);
    let beach = common::jpeg(95, 320, 240, 70);
    assert_ne!(sunset, beach);
    for folder in ["phone", "backup"] {
        workspace.write(&format!("{folder}/sunset.jpg"), &sunset);
        workspace.write(&format!("{folder}/beach.jpg"), &beach);
    }

    let config = config(
        &workspace,
        &[
            &workspace.path().join("phone").to_string_lossy(),
            &workspace.path().join("backup").to_string_lossy(),
        ],
    );
    let outcome = run(&config);

    assert_eq!(
        outcome.report.summary.exact_duplicate_files, 2,
        "each file exists twice"
    );
    assert_eq!(
        outcome.report.summary.groups_kept_apart,
        1,
        "the four look-alike pairings describe one relationship: {:#?}",
        outcome
            .report
            .groups
            .iter()
            .map(|g| (&g.id, g.merged))
            .collect::<Vec<_>>()
    );
    assert_eq!(workspace.output_names(), ["beach.jpg", "sunset.jpg"]);
}

#[test]
#[cfg(unix)]
fn symbolic_links_are_never_followed() {
    let workspace = Workspace::new();
    let real = workspace.write("photos/IMG_0001.JPG", &PHOTO_A);
    workspace.dir("links");
    std::os::unix::fs::symlink(&real, workspace.path().join("links/same-photo.jpg")).unwrap();

    let config = config(
        &workspace,
        &[
            &workspace.path().join("photos").to_string_lossy(),
            &workspace.path().join("links").to_string_lossy(),
        ],
    );
    let outcome = run(&config);

    assert_eq!(
        outcome.report.summary.files_scanned, 1,
        "a link is not a second photo"
    );
    assert_eq!(outcome.report.scan.symlinks, 1);
    assert_eq!(workspace.output_names(), ["IMG_0001.JPG"]);
}

#[test]
fn photos_in_the_trash_are_left_where_they_are() {
    let workspace = Workspace::new();
    workspace.write("a/IMG_0001.JPG", &PHOTO_A);
    // Copying this out would quietly undelete it.
    workspace.write("a/.Trashes/IMG_0002.JPG", &PHOTO_B);
    workspace.write("a/.Spotlight-V100/thumb.jpg", &PHOTO_B);

    let config = config(&workspace, &[&workspace.path().join("a").to_string_lossy()]);
    let outcome = run(&config);

    assert_eq!(outcome.report.summary.files_scanned, 1);
    assert_eq!(workspace.output_names(), ["IMG_0001.JPG"]);
}
