//! Fase 1: copia exacta sin pérdida, perfiles de color, orientación y EXIF.

mod common;

use std::io::Cursor;

use image::codecs::jpeg::JpegEncoder;
use image::{ColorType, DynamicImage, ImageEncoder};
use moxcms::ColorProfile;
use renuevo_core::io::{EncodeOptions, OutputFormat, decode_bytes, encode, icc};
use renuevo_core::verify::verify_encoded;
use renuevo_core::{ColorSpaceInfo, Error, SourceFormat, export};

fn opts(format: OutputFormat) -> EncodeOptions {
    EncodeOptions {
        format,
        ..Default::default()
    }
}

fn assert_roundtrip(image: &DynamicImage, icc: Option<&[u8]>, format: OutputFormat) {
    let bytes = encode(image, icc, &opts(format)).expect("codifica");
    let report = verify_encoded(image, icc, &bytes).expect("verifica");
    assert!(
        report.identical,
        "{format:?} {:?}: {:?}",
        image.color(),
        report.mismatch
    );
    assert!(report.icc_preserved, "{format:?}: ICC no conservado");
    assert!(!report.exif_present);
    assert_eq!(report.pixels, u64::from(image.width() * image.height()));
}

#[test]
fn png_is_bit_exact_for_8_and_16_bits() {
    assert_roundtrip(&common::rgb8(97, 61), None, OutputFormat::Png);
    assert_roundtrip(&common::rgb16(97, 61), None, OutputFormat::Png);
    assert_roundtrip(&common::rgba8(64, 33), None, OutputFormat::Png);
    assert_roundtrip(&common::luma16(50, 40), None, OutputFormat::Png);
}

#[test]
fn tiff_is_bit_exact_for_16_bits() {
    assert_roundtrip(&common::rgb8(80, 45), None, OutputFormat::Tiff);
    assert_roundtrip(&common::rgb16(80, 45), None, OutputFormat::Tiff);
}

#[test]
fn webp_lossless_is_bit_exact_for_8_bits() {
    assert_roundtrip(&common::rgb8(77, 51), None, OutputFormat::WebP);
    assert_roundtrip(&common::rgba8(64, 64), None, OutputFormat::WebP);
}

#[test]
fn webp_refuses_what_it_cannot_return_identically() {
    // 16 bits perdería precisión; el gris volvería como RGB.
    for image in [
        common::rgb16(16, 16),
        common::luma16(16, 16),
        common::rgb8(16, 16).grayscale(),
    ] {
        let err = encode(&image, None, &opts(OutputFormat::WebP)).unwrap_err();
        assert!(
            matches!(err, Error::Unsupported(_)),
            "{:?}: {err}",
            image.color()
        );
    }
}

#[test]
fn icc_profile_is_preserved_byte_for_byte() {
    let p3 = ColorProfile::new_display_p3().encode().unwrap();
    for format in OutputFormat::ALL {
        assert_roundtrip(&common::rgb8(40, 30), Some(&p3), format);
    }
    assert_roundtrip(&common::rgb16(40, 30), Some(&p3), OutputFormat::Png);
}

/// Regresión: `image` 0.25.10 pierde el ICC de TIFF pequeños; nuestro decodificador no.
#[test]
fn small_tiff_keeps_icc_when_decoded() {
    let p3 = ColorProfile::new_display_p3().encode().unwrap();
    let bytes = encode(&common::rgb8(12, 9), Some(&p3), &opts(OutputFormat::Tiff)).unwrap();
    let photo = decode_bytes(&bytes).unwrap();
    assert_eq!(photo.info.format, SourceFormat::Tiff);
    assert_eq!(photo.icc.as_deref(), Some(p3.as_slice()));
}

#[test]
fn sixteen_bit_png_decodes_as_sixteen_bits() {
    let image = common::rgb16(33, 21);
    let bytes = encode(&image, None, &opts(OutputFormat::Png)).unwrap();
    let photo = decode_bytes(&bytes).unwrap();
    assert_eq!(photo.image.color(), ColorType::Rgb16);
    assert_eq!(photo.info.bits_per_channel, 16);
    assert_eq!(photo.info.format, SourceFormat::Png);
    assert_eq!(photo.image.as_bytes(), image.as_bytes());
}

/// EXIF mínimo con orientación 6 ("girar 90° a la derecha") y latitud GPS.
fn exif_orientation_6_with_gps() -> Vec<u8> {
    use exif::{Field, In, Rational, Tag, Value};
    let orientation = Field {
        tag: Tag::Orientation,
        ifd_num: In::PRIMARY,
        value: Value::Short(vec![6]),
    };
    let lat_ref = Field {
        tag: Tag::GPSLatitudeRef,
        ifd_num: In::PRIMARY,
        value: Value::Ascii(vec![b"N".to_vec()]),
    };
    let lat = Field {
        tag: Tag::GPSLatitude,
        ifd_num: In::PRIMARY,
        value: Value::Rational(vec![
            Rational { num: 40, denom: 1 },
            Rational { num: 25, denom: 1 },
            Rational { num: 0, denom: 1 },
        ]),
    };
    let mut writer = exif::experimental::Writer::new();
    writer.push_field(&orientation);
    writer.push_field(&lat_ref);
    writer.push_field(&lat);
    let mut out = Cursor::new(Vec::new());
    writer.write(&mut out, false).unwrap();
    out.into_inner()
}

fn jpeg_with_exif(image: &DynamicImage, exif: Vec<u8>) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut buf, 95);
    encoder.set_exif_metadata(exif).unwrap();
    image.write_with_encoder(encoder).unwrap();
    buf
}

#[test]
fn jpeg_orientation_is_applied_and_exif_gps_is_dropped() {
    // 64x32: arriba-izquierda rojo, arriba-derecha verde, abajo-izq azul, abajo-der amarillo.
    let original = common::quadrants(64, 32);
    let jpeg = jpeg_with_exif(&original, exif_orientation_6_with_gps());

    let photo = decode_bytes(&jpeg).unwrap();
    assert_eq!(photo.info.format, SourceFormat::Jpeg);
    assert!(photo.info.had_exif);
    assert!(photo.info.had_gps, "debería detectar el GPS");
    assert!(photo.info.orientation_applied);
    // Girada 90° a la derecha: 64x32 → 32x64.
    assert_eq!((photo.width(), photo.height()), (32, 64));
    // Tras girar a la derecha, el rojo (arriba-izq) queda arriba-derecha y el azul arriba-izquierda.
    let rgb = photo.image.to_rgb8();
    let top_right = rgb.get_pixel(28, 4).0;
    let top_left = rgb.get_pixel(3, 4).0;
    assert!(top_right[0] > 180 && top_right[1] < 70, "{top_right:?}");
    assert!(top_left[2] > 180 && top_left[0] < 70, "{top_left:?}");

    // Exportar: la copia no lleva EXIF y es idéntica a los píxeles ya orientados.
    let bytes = encode(&photo.image, photo.icc.as_deref(), &opts(OutputFormat::Png)).unwrap();
    let report = verify_encoded(&photo.image, photo.icc.as_deref(), &bytes).unwrap();
    assert!(report.identical);
    assert!(!report.exif_present, "el EXIF (con GPS) no debe copiarse");
    let reopened = decode_bytes(&bytes).unwrap();
    assert!(!reopened.info.had_exif);
    assert!(!reopened.info.orientation_applied);
}

#[test]
fn export_writes_verified_file_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("copia.png");
    let image = common::rgb16(120, 80);
    let p3 = ColorProfile::new_display_p3().encode().unwrap();

    let report = export(&image, Some(&p3), &path, &opts(OutputFormat::Png)).unwrap();
    assert!(report.verify.identical);
    assert_eq!(
        report.bytes_written,
        std::fs::metadata(&path).unwrap().len()
    );
    // No quedan temporales.
    let leftovers: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
    assert_eq!(leftovers.len(), 1);

    let reopened = renuevo_core::io::open(&path).unwrap();
    assert_eq!(reopened.image.as_bytes(), image.as_bytes());
    assert_eq!(reopened.icc.as_deref(), Some(p3.as_slice()));
    assert!(matches!(reopened.info.color, ColorSpaceInfo::Icc { .. }));
}

#[test]
fn incompatible_icc_is_rejected() {
    let gray = ColorProfile::new_gray_with_gamma(2.2).encode().unwrap();
    assert!(icc::check_compatible(&gray, false).is_err());
    assert!(icc::check_compatible(&gray, true).is_ok());
    let p3 = ColorProfile::new_display_p3().encode().unwrap();
    assert!(icc::check_compatible(&p3, false).is_ok());
    assert_eq!(
        icc::describe(&p3).as_deref().map(|d| d.contains("P3")),
        Some(true)
    );
}

#[test]
fn nclx_display_p3_gets_an_icc_and_srgb_does_not() {
    let (p3, info) = icc::from_cicp(12, 13);
    assert!(p3.is_some());
    assert_eq!(
        info,
        ColorSpaceInfo::Nclx {
            description: "Display P3".into()
        }
    );
    let (srgb, _) = icc::from_cicp(1, 13);
    assert!(srgb.is_none());
}

/// HEIC real generado con `sips` (viene con macOS). Se omite en otros sistemas.
#[cfg(feature = "heic")]
#[test]
fn heic_from_sips_decodes_correctly() {
    let Ok(sips) = which_sips() else {
        eprintln!("sips no disponible: se omite el test de HEIC");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("in.png");
    let heic = dir.path().join("out.heic");
    let original = common::quadrants(96, 64);
    original.save(&png).unwrap();
    let status = std::process::Command::new(sips)
        .args(["-s", "format", "heic"])
        .arg(&png)
        .arg("--out")
        .arg(&heic)
        .output()
        .unwrap();
    assert!(status.status.success(), "{status:?}");

    let photo = renuevo_core::io::open(&heic).unwrap();
    assert_eq!(photo.info.format, SourceFormat::Heif);
    assert_eq!((photo.width(), photo.height()), (96, 64));
    let diff = common::mean_abs_diff_8(&original, &photo.image);
    assert!(diff < 6.0, "HEIC demasiado distinto del original: {diff}");

    // Y se puede exportar como copia exacta verificada.
    let out = dir.path().join("copia.png");
    let report = export(
        &photo.image,
        photo.icc.as_deref(),
        &out,
        &opts(OutputFormat::Png),
    )
    .unwrap();
    assert!(report.verify.identical);
}

#[cfg(feature = "heic")]
fn which_sips() -> Result<&'static str, ()> {
    let path = "/usr/bin/sips";
    if std::path::Path::new(path).exists() {
        Ok(path)
    } else {
        Err(())
    }
}
