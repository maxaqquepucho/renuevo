//! Lo que el contenedor dice además de los píxeles: CMYK con su perfil, gAMA/cHRM de PNG, alfa
//! premultiplicado, DPI, EXIF/GPS de TIFF, páginas extra y transformaciones de HEIC.

mod common;

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command;

use image::{DynamicImage, ImageBuffer, Rgba};
use moxcms::{ColorProfile, Layout, TransformOptions};
use renuevo_core::io::{EncodeOptions, OutputFormat, decode_bytes, encode};
use renuevo_core::verify::verify_encoded;
use renuevo_core::{ColorSpaceInfo, Dpi, export};
use tiff::encoder::{TiffEncoder, colortype};
use tiff::tags::Tag;

fn opts(format: OutputFormat, dpi: Option<Dpi>) -> EncodeOptions {
    EncodeOptions {
        format,
        dpi,
        ..Default::default()
    }
}

/// Convierte la imagen (con su ICC) a sRGB de 8 bits, como la vería un programa con gestión de
/// color.
fn to_srgb8(image: &DynamicImage, icc: Option<&[u8]>) -> Vec<u8> {
    let src = icc.map_or_else(ColorProfile::new_srgb, |b| {
        ColorProfile::new_from_slice(b).unwrap()
    });
    let rgb = image.to_rgb16();
    let transform = src
        .create_transform_16bit(
            Layout::Rgb,
            &ColorProfile::new_srgb(),
            Layout::Rgb,
            TransformOptions::default(),
        )
        .unwrap();
    let mut out = vec![0u16; rgb.as_raw().len()];
    transform.transform(rgb.as_raw(), &mut out).unwrap();
    out.iter()
        .map(|v| (f32::from(*v) / 257.0).round() as u8)
        .collect()
}

fn mean_max_diff(a: &[u8], b: &[u8]) -> (f64, u8) {
    assert_eq!(a.len(), b.len());
    let diffs = a.iter().zip(b).map(|(x, y)| x.abs_diff(*y));
    let max = diffs.clone().max().unwrap_or(0);
    let mean = diffs.map(f64::from).sum::<f64>() / a.len() as f64;
    (mean, max)
}

fn tool(name: &str) -> Option<PathBuf> {
    ["/usr/bin", "/opt/homebrew/bin", "/usr/local/bin"]
        .iter()
        .map(|dir| Path::new(dir).join(name))
        .find(|p| p.exists())
}

fn run(cmd: &mut Command) {
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "{cmd:?}: {out:?}");
}

// ---- CMYK ----

/// JPEG y TIFF CMYK creados por macOS con su perfil: deben verse igual que en macOS (antes,
/// la fórmula ingenua de `image` se desviaba hasta 109/255).
#[test]
fn cmyk_is_converted_with_its_profile() {
    let Some(sips) = tool("sips") else {
        eprintln!("sips no disponible: se omite el test de CMYK");
        return;
    };
    let cmyk_profile = "/System/Library/ColorSync/Profiles/Generic CMYK Profile.icc";
    if !Path::new(cmyk_profile).exists() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("in.png");
    common::quadrants(64, 48).save(&png).unwrap();
    for format in ["jpeg", "tiff"] {
        let cmyk = dir.path().join(format!("cmyk.{format}"));
        let reference = dir.path().join(format!("ref-{format}.png"));
        run(Command::new(&sips)
            .args(["-m", cmyk_profile, "-s", "format", format])
            .arg(&png)
            .arg("--out")
            .arg(&cmyk));
        run(Command::new(&sips)
            .args([
                "-m",
                "/System/Library/ColorSync/Profiles/sRGB Profile.icc",
                "-s",
                "format",
                "png",
            ])
            .arg(&cmyk)
            .arg("--out")
            .arg(&reference));

        let photo = renuevo_core::io::open(&cmyk).unwrap();
        assert!(
            matches!(photo.info.color, ColorSpaceInfo::Converted { .. }),
            "{format}: {:?}",
            photo.info.color
        );
        assert_eq!(photo.image.color(), image::ColorType::Rgb16);
        assert!(!photo.info.notes.is_empty());
        let ours = to_srgb8(&photo.image, photo.icc.as_deref());
        let macos = image::open(&reference).unwrap().to_rgb8();
        let (mean, max) = mean_max_diff(&ours, macos.as_raw());
        assert!(
            mean < 2.5 && max < 10,
            "{format}: media {mean:.2}, máx {max}"
        );

        // Y la copia (en Adobe RGB, 16 bits) se verifica píxel a píxel.
        let out = dir.path().join(format!("copia-{format}.png"));
        let report = export(
            &photo.image,
            photo.icc.as_deref(),
            &out,
            &opts(OutputFormat::Png, None),
        )
        .unwrap();
        assert!(report.verify.identical && report.verify.icc_preserved);
    }
}

// ---- gAMA / cHRM ----

fn png_with_chunks(gray: bool, gamma: f64, chrm: Option<png::SourceChromaticities>) -> Vec<u8> {
    let (w, h) = (16u32, 4u32);
    let channels = if gray { 1 } else { 3 };
    let data: Vec<u8> = (0..w * h * channels)
        .map(|i| (i * 16 % 256) as u8)
        .collect();
    let mut buf = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut buf, w, h);
        encoder.set_color(if gray {
            png::ColorType::Grayscale
        } else {
            png::ColorType::Rgb
        });
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_source_gamma(png::ScaledFloat::new(gamma as f32));
        if let Some(c) = chrm {
            encoder.set_source_chromaticities(c);
        }
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&data)
            .unwrap();
    }
    buf
}

/// gAMA = 1,0 (lineal): el gris medio lineal (128) es un sRGB ≈ 188, no 128.
#[test]
fn png_gamma_becomes_an_equivalent_icc() {
    let bytes = png_with_chunks(false, 1.0, None);
    let photo = decode_bytes(&bytes).unwrap();
    assert!(
        matches!(photo.info.color, ColorSpaceInfo::Converted { .. }),
        "{:?}",
        photo.info.color
    );
    let icc = photo.icc.as_deref().expect("ICC sintetizado");
    let mid = DynamicImage::ImageRgb8(ImageBuffer::from_pixel(1, 1, image::Rgb([128, 128, 128])));
    let srgb = to_srgb8(&mid, Some(icc));
    assert!(
        srgb.iter().all(|v| v.abs_diff(188) <= 2),
        "lineal 128 → sRGB {srgb:?}"
    );

    // Se exporta con ese ICC y se verifica.
    let encoded = encode(&photo.image, Some(icc), &opts(OutputFormat::Png, None)).unwrap();
    let report = verify_encoded(&photo.image, Some(icc), &encoded).unwrap();
    assert!(report.identical && report.icc_preserved);

    // Gris con gAMA → perfil de gris; un PNG sRGB normal sigue sin perfil.
    let gray = decode_bytes(&png_with_chunks(true, 1.0, None)).unwrap();
    assert!(gray.icc.is_some());
    assert!(renuevo_core::io::icc::check_compatible(gray.icc.as_deref().unwrap(), true).is_ok());
    let plain = decode_bytes(&png_with_chunks(false, 1.0 / 2.2, None)).unwrap();
    assert_eq!(plain.info.color, ColorSpaceInfo::AssumedSrgb);
    assert!(plain.icc.is_none());
}

/// cHRM con primarias de Adobe RGB: el verde puro no es el verde de sRGB.
#[test]
fn png_chromaticities_are_kept() {
    let adobe =
        png::SourceChromaticities::new((0.3127, 0.3290), (0.64, 0.33), (0.21, 0.71), (0.15, 0.06));
    let photo = decode_bytes(&png_with_chunks(false, 1.0 / 2.2, Some(adobe))).unwrap();
    let icc = photo.icc.as_deref().expect("ICC sintetizado");
    let green = DynamicImage::ImageRgb8(ImageBuffer::from_pixel(1, 1, image::Rgb([0, 255, 0])));
    let srgb = to_srgb8(&green, Some(icc));
    // El verde de Adobe RGB queda fuera de sRGB: rojo/azul recortados y verde al máximo.
    assert!(srgb[1] >= 250 && srgb[0] < 10, "{srgb:?}");
    let reference = to_srgb8(&green, None);
    assert_eq!(reference, vec![0, 255, 0]);
}

// ---- Alfa premultiplicado ----

fn tiff_premultiplied() -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut encoder = TiffEncoder::new(Cursor::new(&mut buf)).unwrap();
        let mut image = encoder.new_image::<colortype::RGBA8>(2, 1).unwrap();
        image.encoder().write_tag(Tag::ExtraSamples, 1u16).unwrap();
        image.write_data(&[64, 32, 16, 128, 9, 9, 9, 0]).unwrap();
    }
    buf
}

#[test]
fn premultiplied_tiff_alpha_is_made_straight() {
    let photo = decode_bytes(&tiff_premultiplied()).unwrap();
    let px: Vec<Rgba<u8>> = photo.image.to_rgba8().pixels().copied().collect();
    assert_eq!(px, vec![Rgba([128, 64, 32, 128]), Rgba([0, 0, 0, 0])]);
    assert!(
        photo
            .info
            .notes
            .iter()
            .any(|n| n.contains("premultiplicado"))
    );
}

#[cfg(feature = "heic")]
#[test]
fn premultiplied_heic_alpha_is_made_straight() {
    let Some(heif_enc) = tool("heif-enc") else {
        eprintln!("heif-enc no disponible: se omite");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("rgba.png");
    let heic = dir.path().join("premul.heic");
    DynamicImage::ImageRgba8(ImageBuffer::from_pixel(8, 4, Rgba([64, 32, 16, 128])))
        .save(&png)
        .unwrap();
    run(Command::new(heif_enc)
        .args(["-L", "--premultiplied-alpha"])
        .arg(&png)
        .arg("-o")
        .arg(&heic));
    let photo = renuevo_core::io::open(&heic).unwrap();
    let px = photo.image.to_rgba8().get_pixel(3, 2).0;
    assert_eq!(px, [128, 64, 32, 128]);
    assert!(
        photo
            .info
            .notes
            .iter()
            .any(|n| n.contains("premultiplicado"))
    );
}

// ---- DPI ----

#[test]
fn dpi_survives_png_and_tiff() {
    let image = common::rgba8(20, 10);
    let dpi = Dpi::new(300.0, 150.0);
    for format in [OutputFormat::Png, OutputFormat::Tiff] {
        let bytes = encode(&image, None, &opts(format, dpi)).unwrap();
        let report = verify_encoded(&image, None, &bytes).unwrap();
        assert!(report.identical, "{format:?}");
        let read = decode_bytes(&bytes).unwrap().info.dpi.expect("DPI");
        assert!(
            (read.x - 300.0).abs() < 0.01 && (read.y - 150.0).abs() < 0.01,
            "{format:?}: {read:?}"
        );
    }
    // Sin DPI no se inventa ninguno.
    let bytes = encode(&image, None, &opts(OutputFormat::Png, None)).unwrap();
    assert_eq!(decode_bytes(&bytes).unwrap().info.dpi, None);
}

/// El TIFF con alfa declara `ExtraSamples = 2` (alfa normal) para que los programas lo usen.
#[test]
fn rgba_tiff_declares_unassociated_alpha() {
    let bytes = encode(&common::rgba8(8, 8), None, &opts(OutputFormat::Tiff, None)).unwrap();
    let mut decoder = tiff::decoder::Decoder::new(Cursor::new(&bytes)).unwrap();
    let extra = decoder.get_tag_u16_vec(Tag::ExtraSamples).unwrap();
    assert_eq!(extra, vec![2]);
}

#[test]
fn upscaling_keeps_the_print_size() {
    use renuevo_core::PipelineConfig;
    use renuevo_core::upscale::UpscaleParams;
    let cfg = PipelineConfig {
        upscale: Some(UpscaleParams {
            factor: 2.0,
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(
        cfg.output_dpi(Dpi::new(300.0, 300.0)),
        Dpi::new(600.0, 600.0)
    );
    assert_eq!(cfg.output_dpi(None), None);
    assert_eq!(
        PipelineConfig::exact_copy().output_dpi(Dpi::new(72.0, 72.0)),
        Dpi::new(72.0, 72.0)
    );
}

// ---- TIFF: EXIF/GPS y páginas ----

#[test]
fn tiff_gps_and_extra_pages_are_reported() {
    let mut buf = Vec::new();
    {
        let mut encoder = TiffEncoder::new(Cursor::new(&mut buf)).unwrap();
        let mut first = encoder.new_image::<colortype::RGB8>(4, 4).unwrap();
        // Basta con que exista la etiqueta: apunta al propio IFD0 (offset 8).
        first.encoder().write_tag(Tag::GpsDirectory, 8u32).unwrap();
        first.write_data(&[100u8; 48]).unwrap();
        encoder
            .write_image::<colortype::RGB8>(4, 4, &[50u8; 48])
            .unwrap();
    }
    let photo = decode_bytes(&buf).unwrap();
    assert!(photo.info.had_exif && photo.info.had_gps);
    assert!(
        photo.info.notes.iter().any(|n| n.contains("2 páginas")),
        "{:?}",
        photo.info.notes
    );
    assert_eq!(photo.image.to_rgb8().get_pixel(0, 0).0, [100, 100, 100]);
}

// ---- HEIC ----

/// Un HEIC en blanco y negro se lee en gris (antes salía como RGB y se perdía un perfil gris).
#[cfg(feature = "heic")]
#[test]
fn monochrome_heic_stays_gray() {
    let Some(heif_enc) = tool("heif-enc") else {
        eprintln!("heif-enc no disponible: se omite");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("gray.png");
    let heic = dir.path().join("gray.heic");
    let gray = common::rgb8(32, 16).grayscale();
    gray.save(&png).unwrap();
    run(Command::new(heif_enc)
        .arg("-L")
        .arg(&png)
        .arg("-o")
        .arg(&heic));
    let photo = renuevo_core::io::open(&heic).unwrap();
    assert_eq!(photo.image.color(), image::ColorType::L8);
    assert_eq!(photo.image.to_luma8().as_raw(), gray.to_luma8().as_raw());
}

/// La orientación se lee de las propiedades `irot`/`imir`: un giro de 180° (mismo tamaño) también
/// cuenta, y un recorte sin giro no.
#[cfg(feature = "heic")]
#[test]
fn heic_rotation_is_detected_from_the_container() {
    let Some(heif_enc) = tool("heif-enc") else {
        eprintln!("heif-enc no disponible: se omite");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("in.png");
    common::quadrants(32, 16).save(&png).unwrap();
    for (degrees, expected) in [("0", false), ("90", true), ("180", true)] {
        let heic = dir.path().join(format!("r{degrees}.heic"));
        run(Command::new(&heif_enc)
            .args(["--rotate-cw", degrees])
            .arg(&png)
            .arg("-o")
            .arg(&heic));
        let photo = renuevo_core::io::open(&heic).unwrap();
        assert_eq!(photo.info.orientation_applied, expected, "giro {degrees}");
    }
}

// ---- Segunda revisión: CMYK ----

const GENERIC_CMYK: &str = "/System/Library/ColorSync/Profiles/Generic CMYK Profile.icc";

/// JPEG CMYK hecho por macOS (con segmento Adobe), o `None` si no hay `sips`.
fn sips_cmyk_jpeg(dir: &Path) -> Option<Vec<u8>> {
    let sips = tool("sips")?;
    if !Path::new(GENERIC_CMYK).exists() {
        return None;
    }
    let png = dir.join("in.png");
    let jpeg = dir.join("cmyk.jpg");
    common::quadrants(64, 48).save(&png).unwrap();
    run(Command::new(sips)
        .args(["-m", GENERIC_CMYK, "-s", "format", "jpeg"])
        .arg(&png)
        .arg("--out")
        .arg(&jpeg));
    Some(std::fs::read(jpeg).unwrap())
}

/// (posición de los datos, marcador, longitud) de cada segmento de cabecera.
fn segments(jpeg: &[u8]) -> Vec<(usize, u8, usize)> {
    let mut out = Vec::new();
    let mut i = 2;
    while i + 4 <= jpeg.len() && jpeg[i] == 0xFF && jpeg[i + 1] != 0xDA {
        let len = usize::from(u16::from_be_bytes([jpeg[i + 2], jpeg[i + 3]]));
        out.push((i + 4, jpeg[i + 1], len));
        i += 2 + len;
    }
    out
}

/// macOS (y libjpeg, y los navegadores) leen el CMYK de un JPEG siempre invertido, lleve o no
/// el segmento Adobe: sin él, antes salía el negativo.
#[test]
fn cmyk_jpeg_without_adobe_marker_is_read_like_macos() {
    let dir = tempfile::tempdir().unwrap();
    let Some(jpeg) = sips_cmyk_jpeg(dir.path()) else {
        eprintln!("sips no disponible: se omite");
        return;
    };
    let (at, _, len) = *segments(&jpeg)
        .iter()
        .find(|s| s.1 == 0xEE)
        .expect("sips escribe el segmento Adobe");
    let mut stripped = jpeg[..at - 4].to_vec();
    stripped.extend_from_slice(&jpeg[at - 2 + len..]);
    let with = decode_bytes(&jpeg).unwrap();
    let without = decode_bytes(&stripped).unwrap();
    assert!(matches!(
        without.info.color,
        ColorSpaceInfo::Converted { .. }
    ));
    assert_eq!(with.image.as_bytes(), without.image.as_bytes());
}

/// Un `transform` desconocido en el segmento Adobe hacía rechazar el archivo entero.
#[test]
fn unknown_adobe_transform_is_tolerated() {
    let dir = tempfile::tempdir().unwrap();
    let Some(jpeg) = sips_cmyk_jpeg(dir.path()) else {
        eprintln!("sips no disponible: se omite");
        return;
    };
    let (at, _, _) = *segments(&jpeg).iter().find(|s| s.1 == 0xEE).unwrap();
    let mut odd = jpeg.clone();
    odd[at + 11] = 3;
    let original = decode_bytes(&jpeg).unwrap();
    let tolerated = decode_bytes(&odd).expect("debe abrirse como hace macOS");
    assert_eq!(original.image.as_bytes(), tolerated.image.as_bytes());
}

/// Con la K a más resolución que C, M y Y, zune-jpeg devuelve colores erróneos sin avisar: se
/// rechaza con un mensaje claro en lugar de guardar algo mal.
#[test]
fn cmyk_jpeg_with_unsupported_subsampling_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let Some(jpeg) = sips_cmyk_jpeg(dir.path()) else {
        eprintln!("sips no disponible: se omite");
        return;
    };
    let (at, _, _) = *segments(&jpeg)
        .iter()
        .find(|s| s.1 == 0xC0 || s.1 == 0xC2)
        .unwrap();
    let mut odd = jpeg.clone();
    odd[at + 7] = 0x11; // C a 1×1
    odd[at + 6 + 3 * 3 + 1] = 0x22; // K a 2×2
    let err = decode_bytes(&odd).unwrap_err();
    assert!(matches!(err, renuevo_core::Error::Unsupported(_)), "{err}");
}

fn cmyk_tiff(icc: &[u8]) -> Vec<u8> {
    let (w, h) = (16u32, 8u32);
    let data: Vec<u8> = (0..w * h)
        .flat_map(|i| {
            let v = (i * 16 % 256) as u8;
            [v, 255 - v, v / 2, 40]
        })
        .collect();
    let mut buf = Vec::new();
    {
        let mut encoder = TiffEncoder::new(Cursor::new(&mut buf)).unwrap();
        let mut image = encoder.new_image::<colortype::CMYK8>(w, h).unwrap();
        image.encoder().write_tag(Tag::IccProfile, icc).unwrap();
        image.write_data(&data).unwrap();
    }
    buf
}

/// Como ColorSync, se usa la intención que declara el perfil: con un A2B1 (colorimétrico)
/// oscurecido, solo debe cambiar el resultado si la cabecera pide «colorimétrico relativo».
#[test]
fn cmyk_uses_the_rendering_intent_of_its_profile() {
    use moxcms::{LutStore, LutWarehouse, RenderingIntent};

    let Ok(bytes) = std::fs::read(GENERIC_CMYK) else {
        eprintln!("sin perfil CMYK del sistema: se omite");
        return;
    };
    let base = ColorProfile::new_from_slice(&bytes).unwrap();
    let mut dark = base.clone();
    let Some(lut) = dark.lut_a_to_b_colorimetric.as_mut() else {
        eprintln!("el perfil no trae A2B1: se omite");
        return;
    };
    let clut = match lut {
        LutWarehouse::Lut(l) => &mut l.clut_table,
        LutWarehouse::Multidimensional(m) => m.clut.as_mut().unwrap(),
    };
    // Primer canal de salida (L* o X) a la mitad: el A2B1 da colores más oscuros.
    match clut {
        LutStore::Store16(v) => v.iter_mut().step_by(3).for_each(|x| *x /= 2),
        LutStore::Store8(v) => v.iter_mut().step_by(3).for_each(|x| *x /= 2),
    }
    let mean = |icc: &[u8]| {
        let photo = decode_bytes(&cmyk_tiff(icc)).unwrap();
        assert!(matches!(photo.info.color, ColorSpaceInfo::Converted { .. }));
        let srgb = to_srgb8(&photo.image, photo.icc.as_deref());
        srgb.iter().map(|&v| f64::from(v)).sum::<f64>() / srgb.len() as f64
    };
    for (intent, darker) in [
        (RenderingIntent::Perceptual, false),
        (RenderingIntent::RelativeColorimetric, true),
    ] {
        let mut normal = base.clone();
        normal.rendering_intent = intent;
        dark.rendering_intent = intent;
        let (n, d) = (
            mean(&normal.encode().unwrap()),
            mean(&dark.encode().unwrap()),
        );
        if darker {
            assert!(
                d < n - 5.0,
                "{intent:?}: {d:.1} debería ser más oscuro que {n:.1}"
            );
        } else {
            assert!((d - n).abs() < 0.5, "{intent:?}: {d:.1} ≠ {n:.1}");
        }
    }
}

// ---- Segunda revisión: gAMA / cHRM como macOS ----

#[test]
fn png_chrm_without_gamma_is_ignored_and_degenerate_chrm_rejected() {
    let adobe =
        png::SourceChromaticities::new((0.3127, 0.3290), (0.64, 0.33), (0.21, 0.71), (0.15, 0.06));
    // Sin gAMA, macOS no aplica el cHRM.
    let (w, h) = (4u32, 4u32);
    let mut buf = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut buf, w, h);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_source_chromaticities(adobe);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&[128; 48])
            .unwrap();
    }
    let photo = decode_bytes(&buf).unwrap();
    assert_eq!(photo.info.color, ColorSpaceInfo::AssumedSrgb);
    assert!(photo.icc.is_none());

    // Primarias alineadas: no son un espacio de color.
    let line = png::SourceChromaticities::new((0.3127, 0.329), (0.2, 0.2), (0.3, 0.3), (0.4, 0.4));
    let photo = decode_bytes(&png_with_chunks(false, 1.0, Some(line))).unwrap();
    assert!(
        matches!(photo.info.color, ColorSpaceInfo::Discarded { .. }),
        "{:?}",
        photo.info.color
    );
}

/// gAMA 0,45455 con primarias que no son sRGB es una potencia 2,2 pura (así lo muestra macOS).
#[test]
fn png_gamma_with_wide_primaries_matches_macos() {
    let Some(sips) = tool("sips") else {
        eprintln!("sips no disponible: se omite");
        return;
    };
    let adobe =
        png::SourceChromaticities::new((0.3127, 0.3290), (0.64, 0.33), (0.21, 0.71), (0.15, 0.06));
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("g.png");
    let reference = dir.path().join("ref.png");
    std::fs::write(&src, png_with_chunks(false, 1.0 / 2.2, Some(adobe))).unwrap();
    run(Command::new(sips)
        .args([
            "-m",
            "/System/Library/ColorSync/Profiles/sRGB Profile.icc",
            "-s",
            "format",
            "png",
        ])
        .arg(&src)
        .arg("--out")
        .arg(&reference));
    let photo = renuevo_core::io::open(&src).unwrap();
    let ours = to_srgb8(&photo.image, photo.icc.as_deref());
    let macos = image::open(&reference).unwrap().to_rgb8();
    let (mean, max) = mean_max_diff(&ours, macos.as_raw());
    assert!(mean < 0.6 && max <= 4, "media {mean:.2}, máx {max}");
}

// ---- Segunda revisión: TIFF ----

/// Tipo de la etiqueta `tag` en el primer IFD de un TIFF little-endian.
fn tiff_tag_type(tiff: &[u8], tag: u16) -> Option<u16> {
    let u16_at = |i: usize| u16::from_le_bytes([tiff[i], tiff[i + 1]]);
    assert_eq!(&tiff[..2], b"II");
    let ifd = u32::from_le_bytes(tiff[4..8].try_into().unwrap()) as usize;
    (0..usize::from(u16_at(ifd)))
        .map(|i| ifd + 2 + 12 * i)
        .find(|&e| u16_at(e) == tag)
        .map(|e| u16_at(e + 2))
}

/// Sin DPI de origen, el TIFF lleva 72 ppp (antes, «1/1 sin unidad», que macOS lee como 1 ppp),
/// y el ICC va como `UNDEFINED`, que es lo que pide la especificación.
#[test]
fn tiff_has_72_dpi_by_default_and_undefined_icc() {
    let icc = ColorProfile::new_display_p3().encode().unwrap();
    let bytes = encode(
        &common::rgb8(8, 8),
        Some(&icc),
        &opts(OutputFormat::Tiff, None),
    )
    .unwrap();
    assert_eq!(decode_bytes(&bytes).unwrap().info.dpi, Dpi::new(72.0, 72.0));
    assert_eq!(tiff_tag_type(&bytes, 34675), Some(7), "ICC como UNDEFINED");
    let report = verify_encoded(&common::rgb8(8, 8), Some(&icc), &bytes).unwrap();
    assert!(report.identical && report.icc_preserved);
    if let Some(sips) = tool("sips") {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.tiff");
        std::fs::write(&path, &bytes).unwrap();
        let out = Command::new(sips)
            .args(["-g", "dpiWidth"])
            .arg(&path)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("72.000"), "{text}");
    }
}

// ---- Segunda revisión: JPEG truncado ----

#[test]
fn truncated_jpeg_is_reported() {
    let mut buf = Vec::new();
    common::rgb8(64, 64)
        .write_with_encoder(image::codecs::jpeg::JpegEncoder::new(&mut buf))
        .unwrap();
    let whole = decode_bytes(&buf).unwrap();
    assert!(whole.info.notes.is_empty(), "{:?}", whole.info.notes);
    buf.truncate(buf.len() * 2 / 3);
    let cut = decode_bytes(&buf).unwrap();
    assert!(
        cut.info.notes.iter().any(|n| n.contains("cortado")),
        "{:?}",
        cut.info.notes
    );
}

// ---- Segunda revisión: HEIC ----

/// Gris con alfa de distinta profundidad (8 + 10 o 10 + 8 bits): antes, el alfa se leía con la
/// profundidad del gris (basura en silencio) o el archivo no abría.
#[cfg(feature = "heic")]
#[test]
fn monochrome_heic_alpha_keeps_its_own_bit_depth() {
    use libheif_rs::{
        Channel, ColorSpace, CompressionFormat, EncoderQuality, HeifContext, Image, LibHeif,
    };

    fn fill(img: &mut Image, ch: Channel, w: u32, h: u32, bits: u8, f: impl Fn(u32) -> u32) {
        img.create_plane(ch, w, h, bits).unwrap();
        let mut planes = img.planes_mut();
        let p = match ch {
            Channel::Y => planes.y.as_mut().unwrap(),
            _ => planes.a.as_mut().unwrap(),
        };
        let stride = p.stride;
        for y in 0..h {
            for x in 0..w {
                let v = f(x);
                if bits > 8 {
                    let o = y as usize * stride + x as usize * 2;
                    p.data[o..o + 2].copy_from_slice(&(v as u16).to_le_bytes());
                } else {
                    p.data[y as usize * stride + x as usize] = v as u8;
                }
            }
        }
    }
    let lib = LibHeif::new();
    let Ok(mut encoder) = lib.encoder_for_format(CompressionFormat::Hevc) else {
        eprintln!("libheif sin codificador HEVC: se omite");
        return;
    };
    encoder.set_quality(EncoderQuality::LossLess).unwrap();
    let (w, h) = (48u32, 8u32);
    for (ybits, abits) in [(8u8, 10u8), (10, 8)] {
        let (ymax, amax) = ((1u32 << ybits) - 1, (1u32 << abits) - 1);
        let luma = |x: u32| x * ymax / (w - 1);
        let alpha = |x: u32| (w - 1 - x) * amax / (w - 1);
        let mut img = Image::new(w, h, ColorSpace::Monochrome).unwrap();
        fill(&mut img, Channel::Y, w, h, ybits, luma);
        fill(&mut img, Channel::Alpha, w, h, abits, alpha);
        let mut ctx = HeifContext::new().unwrap();
        ctx.encode_image(&img, &mut encoder, None).unwrap();
        let bytes = ctx.write_to_bytes().unwrap();

        let photo = decode_bytes(&bytes).unwrap();
        assert_eq!(
            photo.image.color(),
            image::ColorType::La16,
            "{ybits}+{abits}"
        );
        let la = photo.image.to_luma_alpha16();
        for x in 0..w {
            let [l, a] = la.get_pixel(x, 3).0;
            // Escalado reversible a 16 bits: los bits altos son el valor original.
            assert_eq!(
                u32::from(l >> (16 - ybits)),
                luma(x),
                "luma x={x} ({ybits}+{abits})"
            );
            assert_eq!(
                u32::from(a >> (16 - abits)),
                alpha(x),
                "alfa x={x} ({ybits}+{abits})"
            );
        }
    }
}

/// Un HEIC girado 90° intercambia también sus DPI (como ya se hacía con el EXIF de un JPEG).
#[cfg(feature = "heic")]
#[test]
fn heic_quarter_turn_swaps_dpi() {
    let Some(heif_enc) = tool("heif-enc") else {
        eprintln!("heif-enc no disponible: se omite");
        return;
    };
    let mut exif = exif::experimental::Writer::new();
    let x = exif::Field {
        tag: exif::Tag::XResolution,
        ifd_num: exif::In::PRIMARY,
        value: exif::Value::Rational(vec![exif::Rational { num: 300, denom: 1 }]),
    };
    let y = exif::Field {
        tag: exif::Tag::YResolution,
        ifd_num: exif::In::PRIMARY,
        value: exif::Value::Rational(vec![exif::Rational { num: 150, denom: 1 }]),
    };
    let unit = exif::Field {
        tag: exif::Tag::ResolutionUnit,
        ifd_num: exif::In::PRIMARY,
        value: exif::Value::Short(vec![2]),
    };
    exif.push_field(&x);
    exif.push_field(&y);
    exif.push_field(&unit);
    let mut raw = Cursor::new(Vec::new());
    exif.write(&mut raw, false).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let jpeg = dir.path().join("in.jpg");
    let mut buf = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 95);
    image::ImageEncoder::set_exif_metadata(&mut encoder, raw.into_inner()).unwrap();
    common::quadrants(32, 16)
        .write_with_encoder(encoder)
        .unwrap();
    std::fs::write(&jpeg, &buf).unwrap();
    assert_eq!(decode_bytes(&buf).unwrap().info.dpi, Dpi::new(300.0, 150.0));

    for (degrees, expected) in [("0", (300.0, 150.0)), ("90", (150.0, 300.0))] {
        let heic = dir.path().join(format!("r{degrees}.heic"));
        run(Command::new(&heif_enc)
            .args(["--rotate-cw", degrees])
            .arg(&jpeg)
            .arg("-o")
            .arg(&heic));
        let photo = renuevo_core::io::open(&heic).unwrap();
        let Some(dpi) = photo.info.dpi else {
            eprintln!("heif-enc no copió el EXIF: se omite");
            return;
        };
        assert_eq!((dpi.x, dpi.y), expected, "giro {degrees}");
    }
}
