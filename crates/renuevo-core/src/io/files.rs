//! Identidad de archivos: comparar por inodo, no por nombre.
//!
//! Así `foto.PNG` y `foto.png` en un disco que no distingue mayúsculas (APFS por defecto), una
//! ruta relativa y su absoluta, o un enlace simbólico, se reconocen como el mismo archivo.

use std::path::Path;

#[cfg(unix)]
pub(crate) type FileId = (u64, u64);
#[cfg(not(unix))]
pub(crate) type FileId = std::path::PathBuf;

/// Identidad del archivo si existe.
#[cfg(unix)]
pub(crate) fn file_id(path: &Path) -> Option<FileId> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

#[cfg(not(unix))]
pub(crate) fn file_id(path: &Path) -> Option<FileId> {
    path.canonicalize().ok()
}

/// ¿Son `a` y `b` el mismo archivo existente? Si alguno no existe, no lo son.
pub fn same_file(a: &Path, b: &Path) -> bool {
    match (file_id(a), file_id(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}
