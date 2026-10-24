//! Rasterize the vector mark at its actual on-screen pixel size, including DPI.
use gpui::{IntoElement, RenderImage, Styled, canvas, px};
use resvg::{tiny_skia, usvg};
use std::{
    cell::RefCell,
    collections::VecDeque,
    sync::{Arc, LazyLock},
};

const SVG: &[u8] = include_bytes!("../assets/icon.svg");
const CACHE_LIMIT: usize = 16;

thread_local! {
    static IMAGES: RefCell<VecDeque<(u32, Arc<RenderImage>)>> = const { RefCell::new(VecDeque::new()) };
}

fn pixel_size(side: f32, scale_factor: f32) -> u32 {
    // paint_image rounds its output bounds up to whole device pixels.
    (side * scale_factor).ceil().clamp(1., 256.) as u32
}

fn rasterize(side: u32) -> Arc<RenderImage> {
    static TREE: LazyLock<usvg::Tree> = LazyLock::new(|| {
        usvg::Tree::from_data(SVG, &usvg::Options::default())
            .expect("the embedded Wayline vector mark is valid")
    });
    let mut pixmap = tiny_skia::Pixmap::new(side, side).expect("bounded icon dimensions");
    let scale = side as f32 / TREE.size().width();
    resvg::render(
        &TREE,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let mut pixels = image::RgbaImage::new(side, side);
    for (out, source) in pixels.pixels_mut().zip(pixmap.pixels()) {
        // resvg returns premultiplied RGBA. GPUI expects straight-alpha BGRA;
        // unpremultiply before swapping channels to avoid dark edge fringes.
        let color = source.demultiply();
        out.0 = [color.blue(), color.green(), color.red(), color.alpha()];
    }
    Arc::new(RenderImage::new(vec![image::Frame::new(pixels)]))
}

pub fn mark(side: f32) -> impl IntoElement {
    canvas(
        move |bounds, window, _| {
            let physical = pixel_size(bounds.size.width.into(), window.scale_factor());
            IMAGES.with(|images| {
                let mut images = images.borrow_mut();
                let entry = images
                    .iter()
                    .position(|(size, _)| *size == physical)
                    .and_then(|index| images.remove(index))
                    .unwrap_or_else(|| (physical, rasterize(physical)));
                let image = entry.1.clone();
                images.push_back(entry);
                if images.len() > CACHE_LIMIT
                    && let Some((_, old)) = images.pop_front()
                {
                    let _ = window.drop_image(old);
                }
                image
            })
        },
        |bounds, image, window, _| {
            let _ = window.paint_image(bounds, Default::default(), image, 0, false);
        },
    )
    .size(px(side))
    .flex_shrink_0()
}

#[cfg(test)]
mod tests {
    use super::{pixel_size, rasterize};

    #[test]
    fn logo_uses_device_pixels_and_keeps_antialiased_edges_at_common_dpis() {
        for (side, scale, expected) in [
            (25., 1., 25),
            (25., 1.25, 32),
            (30., 1.5, 45),
            (30., 2., 60),
        ] {
            let pixels = pixel_size(side, scale);
            assert_eq!(pixels, expected);
            let image = rasterize(pixels);
            let bytes = image.as_bytes(0).unwrap();
            assert_eq!(bytes.len(), pixels as usize * pixels as usize * 4);
            let colors = bytes.as_chunks::<4>().0;
            assert!(colors.iter().filter(|p| p[3] > 0 && p[3] < 255).count() > 16);
            assert!(colors.iter().any(|p| p[3] > 240 && p[0] > 240 && p[2] < 30));
            for x in 0..pixels as usize {
                assert_eq!(colors[x][3], 0);
                assert_eq!(colors[(pixels as usize - 1) * pixels as usize + x][3], 0);
            }
        }
    }
}
