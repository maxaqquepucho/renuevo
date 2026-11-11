//! Procesar carpetas enteras. Las fotos van de una en una (cada una paralelizada por dentro)
//! para no agotar la memoria.
//!
//! **Nunca sobrescribe un original**: ninguna salida puede ser (por inodo) una de las fotos de
//! entrada, y dos entradas con el mismo nombre base (`a.jpg`, `a.png`) reciben salidas distintas.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::io::{EncodeOptions, OutputFormat, file_id, open};
use crate::pipeline::{PipelineConfig, process};
use crate::{Error, Progress, Result, export};

pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "webp", "tif", "tiff", "heic", "heif", "hif",
];

pub fn is_supported(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| SUPPORTED_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Fotos compatibles de una carpeta (sin recursión), ordenadas por nombre.
pub fn list_images(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && is_supported(p))
        .collect();
    files.sort();
    Ok(files)
}

fn stem_of(input: &Path) -> String {
    input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "foto".to_owned())
}

/// Nombre de salida de una foto suelta: `<nombre><sufijo>.<formato>`.
pub fn output_path(input: &Path, output_dir: &Path, suffix: &str, format: OutputFormat) -> PathBuf {
    output_dir.join(format!("{}{suffix}.{}", stem_of(input), format.extension()))
}

/// Nombres de salida de un lote, sin colisiones entre sí. Si varias entradas comparten nombre
/// base (sin distinguir mayúsculas), se añade su extensión (`a_jpg_renuevo.png`) y, si aún
/// coincidieran, un número.
pub fn plan_outputs(
    inputs: &[PathBuf],
    output_dir: &Path,
    suffix: &str,
    format: OutputFormat,
) -> Vec<PathBuf> {
    let mut stem_counts: HashMap<String, usize> = HashMap::new();
    for input in inputs {
        *stem_counts
            .entry(stem_of(input).to_lowercase())
            .or_default() += 1;
    }
    let mut taken: HashSet<String> = HashSet::new();
    inputs
        .iter()
        .map(|input| {
            let stem = stem_of(input);
            let base = if stem_counts[&stem.to_lowercase()] > 1 {
                let ext = input
                    .extension()
                    .map(|e| e.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                format!("{stem}_{ext}")
            } else {
                stem
            };
            let mut name = format!("{base}{suffix}.{}", format.extension());
            let mut n = 2;
            while !taken.insert(name.to_lowercase()) {
                name = format!("{base}{suffix}_{n}.{}", format.extension());
                n += 1;
            }
            output_dir.join(name)
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct BatchJob {
    pub inputs: Vec<PathBuf>,
    pub output_dir: PathBuf,
    pub suffix: String,
    pub skip_existing: bool,
    pub pipeline: PipelineConfig,
    pub encode: EncodeOptions,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ItemStatus {
    Pending,
    Running,
    Done { output: PathBuf, bytes: u64 },
    Skipped(String),
    Failed(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BatchSummary {
    pub done: usize,
    pub skipped: usize,
    pub failed: usize,
    pub cancelled: bool,
}

pub fn run_batch(
    job: &BatchJob,
    progress: &Progress,
    mut on_status: impl FnMut(usize, ItemStatus),
) -> BatchSummary {
    let mut summary = BatchSummary::default();
    let pipeline = job.pipeline.for_format(job.encode.format);
    let outputs = plan_outputs(&job.inputs, &job.output_dir, &job.suffix, job.encode.format);
    // Identidad de todas las fotos de entrada: ninguna salida puede caer sobre una de ellas.
    let originals: HashSet<_> = job.inputs.iter().filter_map(|p| file_id(p)).collect();
    for (i, (input, output)) in job.inputs.iter().zip(outputs).enumerate() {
        if progress.is_cancelled() {
            summary.cancelled = true;
            break;
        }
        if file_id(&output).is_some_and(|id| originals.contains(&id)) {
            summary.skipped += 1;
            on_status(
                i,
                ItemStatus::Skipped("sobrescribiría un original".to_owned()),
            );
            continue;
        }
        if job.skip_existing && output.exists() {
            summary.skipped += 1;
            on_status(i, ItemStatus::Skipped("ya existe".to_owned()));
            continue;
        }
        on_status(i, ItemStatus::Running);
        let result = (|| -> Result<u64> {
            let photo = open(input)?;
            let encode = EncodeOptions {
                dpi: pipeline.output_dpi(photo.info.dpi),
                ..job.encode
            };
            let report = if pipeline.is_exact_copy() {
                // Copia exacta: sin clonar la imagen (fotos grandes = mucha memoria).
                export(&photo.image, photo.icc.as_deref(), &output, &encode)?
            } else {
                let processed = process(&photo.image, &pipeline, progress)?;
                export(&processed.image, photo.icc.as_deref(), &output, &encode)?
            };
            Ok(report.bytes_written)
        })();
        match result {
            Ok(bytes) => {
                summary.done += 1;
                on_status(i, ItemStatus::Done { output, bytes });
            }
            Err(Error::Cancelled) => {
                summary.cancelled = true;
                on_status(i, ItemStatus::Failed("cancelado".to_owned()));
                break;
            }
            Err(e) => {
                summary.failed += 1;
                on_status(i, ItemStatus::Failed(e.to_string()));
            }
        }
    }
    summary
}
