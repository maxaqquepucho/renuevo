//! # renuevo-core
//!
//! Núcleo de la app: abre fotos (JPEG, PNG, WebP, TIFF, HEIC), las regenera sin pérdida
//! verificando cada píxel, y ofrece restauración y ampliación con algoritmos clásicos.
//!
//! No depende de ninguna GUI: todo es testeable y se puede usar desde otra interfaz.

pub mod batch;
pub mod color;
pub mod error;
pub mod export;
pub mod filters;
pub mod io;
pub mod metrics;
pub mod photo;
pub mod pipeline;
pub mod planar;
pub mod progress;
pub mod restore;
pub mod upscale;
pub mod verify;

pub use error::{Error, Result};
pub use export::{ExportReport, export};
pub use photo::{ColorSpaceInfo, Dpi, Photo, SourceFormat, SourceInfo};
pub use pipeline::{PipelineConfig, Processed, process, process_region};
pub use planar::BitDepth;
pub use progress::Progress;
