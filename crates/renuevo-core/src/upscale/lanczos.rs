//! Lanczos3 por plano con `fast_image_resize` (SIMD, paralelo con rayon).

use fast_image_resize::images::{Image, ImageRef};
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};

use crate::{Error, Result};

pub fn resize_plane(src: &[f32], w: usize, h: usize, nw: usize, nh: usize) -> Result<Vec<f32>> {
    let err = |e: &dyn std::fmt::Display| Error::Resize(e.to_string());
    let src_image = ImageRef::new(
        w as u32,
        h as u32,
        bytemuck::cast_slice(src),
        PixelType::F32,
    )
    .map_err(|e| err(&e))?;
    let mut out = vec![0.0f32; nw * nh];
    let mut dst = Image::from_slice_u8(
        nw as u32,
        nh as u32,
        bytemuck::cast_slice_mut(&mut out),
        PixelType::F32,
    )
    .map_err(|e| err(&e))?;
    let options = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Lanczos3));
    Resizer::new()
        .resize(&src_image, &mut dst, &options)
        .map_err(|e| err(&e))?;
    drop(dst);
    Ok(out)
}
