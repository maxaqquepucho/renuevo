//! Alfa premultiplicado (asociado) → alfa normal, que es lo que guardan PNG, TIFF y WebP.

use image::DynamicImage;
use rayon::prelude::*;

/// Divide cada color por su alfa. Con alfa 0 el color no existe y queda en 0.
pub(crate) fn unpremultiply(image: &mut DynamicImage) {
    match image {
        DynamicImage::ImageRgba8(b) => b.par_chunks_mut(4).for_each(|p| un_int(p, 255)),
        DynamicImage::ImageLumaA8(b) => b.par_chunks_mut(2).for_each(|p| un_int(p, 255)),
        DynamicImage::ImageRgba16(b) => b.par_chunks_mut(4).for_each(|p| un_int(p, 65535)),
        DynamicImage::ImageLumaA16(b) => b.par_chunks_mut(2).for_each(|p| un_int(p, 65535)),
        DynamicImage::ImageRgba32F(b) => b.par_chunks_mut(4).for_each(|p| {
            let (color, alpha) = p.split_at_mut(3);
            let a = alpha[0];
            for c in color {
                *c = if a > 0.0 { *c / a } else { 0.0 };
            }
        }),
        _ => {}
    }
}

fn un_int<T>(pixel: &mut [T], max: u32)
where
    T: Copy + Into<u32> + TryFrom<u32>,
{
    let Some((alpha, color)) = pixel.split_last_mut() else {
        return;
    };
    let a: u32 = (*alpha).into();
    if a == max {
        return;
    }
    for c in color {
        let v = if a == 0 {
            0
        } else {
            ((u64::from((*c).into()) * u64::from(max) + u64::from(a / 2)) / u64::from(a))
                .min(u64::from(max)) as u32
        };
        if let Ok(v) = T::try_from(v) {
            *c = v;
        }
    }
}

#[cfg(test)]
mod tests {
    use image::{DynamicImage, ImageBuffer, Rgba};

    use super::unpremultiply;

    #[test]
    fn premultiplied_rgba8_becomes_straight() {
        let mut img = DynamicImage::ImageRgba8(
            ImageBuffer::from_vec(3, 1, vec![64, 32, 16, 128, 10, 20, 30, 0, 1, 2, 3, 255])
                .unwrap(),
        );
        unpremultiply(&mut img);
        let px: Vec<Rgba<u8>> = img.to_rgba8().pixels().copied().collect();
        assert_eq!(px[0], Rgba([128, 64, 32, 128]));
        assert_eq!(px[1], Rgba([0, 0, 0, 0]));
        assert_eq!(px[2], Rgba([1, 2, 3, 255]));
    }
}
