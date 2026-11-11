//! Exportar con garantías: codificar → verificar en memoria → escribir atómico → releer y comparar.

use std::path::{Path, PathBuf};

use image::DynamicImage;

use crate::io::{EncodeOptions, encode, write_atomic};
use crate::verify::{VerifyReport, verify_encoded};
use crate::{Error, Result};

#[derive(Debug, Clone)]
pub struct ExportReport {
    pub path: PathBuf,
    pub bytes_written: u64,
    pub verify: VerifyReport,
}

/// Guarda `image` con su `icc` y comprueba que el archivo se decodifica a exactamente los
/// mismos píxeles. Si no, no escribe nada y devuelve error.
pub fn export(
    image: &DynamicImage,
    icc: Option<&[u8]>,
    path: &Path,
    options: &EncodeOptions,
) -> Result<ExportReport> {
    let bytes = encode(image, icc, options)?;
    let verify = verify_encoded(image, icc, &bytes)?;
    if let Some(m) = &verify.mismatch {
        return Err(Error::VerificationFailed(m.clone()));
    }
    if !verify.icc_preserved {
        return Err(Error::VerificationFailed(
            "el perfil de color no se conservó".to_owned(),
        ));
    }

    write_atomic(path, &bytes)?;
    // Comprobación final de lo que realmente quedó en disco.
    if !file_matches(path, &bytes)? {
        return Err(Error::VerificationFailed(
            "el archivo en disco no coincide con lo codificado".to_owned(),
        ));
    }

    Ok(ExportReport {
        path: path.to_path_buf(),
        bytes_written: bytes.len() as u64,
        verify,
    })
}

/// Compara el archivo con `expected` por bloques, sin cargar otra copia entera en memoria.
fn file_matches(path: &Path, expected: &[u8]) -> Result<bool> {
    use std::io::Read;

    let mut file = std::fs::File::open(path)?;
    if file.metadata()?.len() != expected.len() as u64 {
        return Ok(false);
    }
    let mut chunk = vec![0u8; 1 << 20];
    let mut offset = 0;
    loop {
        let n = file.read(&mut chunk)?;
        if n == 0 {
            return Ok(offset == expected.len());
        }
        if expected.get(offset..offset + n) != Some(&chunk[..n]) {
            return Ok(false);
        }
        offset += n;
    }
}
