//! Builders that make in-memory libraries for unit tests.
//!
//! Ranking and matching are pure functions over [`MediaFile`] values, so they
//! can be tested without writing a single file to disk.

use std::path::PathBuf;

use crate::cli::{Config, CopyMode, ReportFormat};
use crate::models::{
    FileFormat, GpsCoord, Library, MediaFile, MediaMetadata, SourceRoot, VisualFingerprint,
};

#[derive(Debug, Clone)]
pub struct FileSpec {
    path: PathBuf,
    size: u64,
    metadata: MediaMetadata,
    fingerprint: Option<VisualFingerprint>,
}

/// Start describing a test file. The format comes from the extension.
pub fn file(path: &str) -> FileSpec {
    FileSpec {
        path: PathBuf::from(path),
        size: 1_000_000,
        metadata: MediaMetadata::default(),
        fingerprint: None,
    }
}

impl FileSpec {
    pub fn dimensions(mut self, width: u32, height: u32) -> Self {
        self.metadata.width = Some(width);
        self.metadata.height = Some(height);
        self
    }

    pub fn size(mut self, size: u64) -> Self {
        self.size = size;
        self
    }

    pub fn camera(mut self, make: &str, model: &str) -> Self {
        self.metadata.camera_make = Some(make.to_string());
        self.metadata.camera_model = Some(model.to_string());
        self
    }

    pub fn tags(mut self, count: u32) -> Self {
        self.metadata.tag_count = count;
        self
    }

    pub fn software(mut self, software: &str) -> Self {
        self.metadata.software = Some(software.to_string());
        self
    }

    pub fn captured_at(mut self, unix_seconds: i64) -> Self {
        self.metadata.capture_time = Some(unix_seconds);
        self
    }

    pub fn gps(mut self, latitude: f64, longitude: f64) -> Self {
        self.metadata.gps = Some(GpsCoord {
            latitude,
            longitude,
        });
        self
    }

    /// Give a video a duration, which is what marks it as a Live Photo movie.
    pub fn duration_ms(mut self, duration_ms: u64) -> Self {
        let video = self.metadata.video.get_or_insert_with(Default::default);
        video.duration_ms = Some(duration_ms);
        self
    }

    pub fn live_photo_id(mut self, id: &str) -> Self {
        self.metadata.live_photo_id = Some(id.to_string());
        self
    }

    pub fn fingerprint(mut self, dhash: u64) -> Self {
        self.fingerprint = Some(VisualFingerprint {
            dhash,
            blockhash: [0; 32],
        });
        self
    }

    pub fn blockhash(mut self, blockhash: [u8; 32]) -> Self {
        let dhash = self.fingerprint.map(|f| f.dhash).unwrap_or(0);
        self.fingerprint = Some(VisualFingerprint { dhash, blockhash });
        self
    }
}

/// Build a library whose sources are the parent folders of the given files.
pub fn library(specs: &[FileSpec]) -> (Library, Config) {
    build(specs, None)
}

/// Same as [`library`], but marks one source folder as preferred.
pub fn library_with_preference(specs: &[FileSpec], preferred: &str) -> (Library, Config) {
    build(specs, Some(PathBuf::from(preferred)))
}

fn build(specs: &[FileSpec], preferred: Option<PathBuf>) -> (Library, Config) {
    let mut sources: Vec<SourceRoot> = Vec::new();
    let mut files: Vec<MediaFile> = Vec::new();

    for spec in specs {
        let root = spec
            .path
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));

        let source = match sources.iter().position(|s| s.path == root) {
            Some(index) => index,
            None => {
                let preference = u32::from(preferred.as_deref() == Some(root.as_path()));
                sources.push(SourceRoot {
                    id: sources.len(),
                    path: root,
                    preference,
                });
                sources.len() - 1
            }
        };

        let format = FileFormat::from_path(&spec.path).unwrap_or(FileFormat::Jpeg);
        files.push(MediaFile {
            id: files.len(),
            path: spec.path.clone(),
            source,
            kind: format.kind(),
            format,
            size: spec.size,
            modified_at: Some(1_700_000_000),
            modified_nanos: Some(0),
            exact_hash: None,
            metadata: spec.metadata.clone(),
            fingerprint: spec.fingerprint,
            analysis_failed: false,
            from_cache: false,
        });
    }

    let config = Config {
        sources: sources.clone(),
        output: PathBuf::from("/out"),
        dry_run: true,
        verbose: 0,
        workers: 1,
        hash_distance: 8,
        report_path: PathBuf::from("/out/report.json"),
        report_format: ReportFormat::Json,
        copy_mode: CopyMode::Copy,
        include_videos: true,
        live_photos: true,
        cache_path: None,
    };

    let library = Library {
        sources,
        files,
        skipped_appledouble: 0,
    };
    (library, config)
}
