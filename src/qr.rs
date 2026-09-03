// Renders 1 pixel per QR module (plus quiet zone); the Slint side scales
// it up with image-rendering: pixelated for crisp edges.
use qrcode::{EcLevel, QrCode};
use slint::{Rgba8Pixel, SharedPixelBuffer};

pub fn qr_image(text: &str) -> slint::Image {
    match QrCode::with_error_correction_level(text, EcLevel::M) {
        Ok(code) => {
            let size = code.width();
            let margin = 4usize;
            let dim = size + margin * 2;
            let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(dim as u32, dim as u32);
            let pixels = buf.make_mut_slice();
            pixels.fill(Rgba8Pixel { r: 255, g: 255, b: 255, a: 255 });
            for y in 0..size {
                for x in 0..size {
                    if code[(x, y)] == qrcode::Color::Dark {
                        let i = (y + margin) * dim + (x + margin);
                        pixels[i] = Rgba8Pixel { r: 0, g: 0, b: 0, a: 255 };
                    }
                }
            }
            slint::Image::from_rgba8(buf)
        }
        Err(_) => empty_image(),
    }
}

pub fn empty_image() -> slint::Image {
    slint::Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::new(1, 1))
}
