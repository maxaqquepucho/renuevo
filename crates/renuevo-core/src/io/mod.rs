//! Entrada/salida de fotos.

mod alpha;
mod cmyk;
mod decode;
pub mod encode;
mod files;
#[cfg(feature = "heic")]
mod heic;
#[cfg(feature = "heic")]
mod heif_boxes;
pub mod icc;
mod meta;

pub use decode::{decode_bytes, open};
pub(crate) use decode::{decode_limits, tiff_icc};
pub use encode::{EncodeOptions, OutputFormat, PngCompression, encode, write_atomic};
pub(crate) use files::file_id;
pub use files::same_file;
