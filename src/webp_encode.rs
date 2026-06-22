use std::fs;
use std::io::{BufReader, Cursor};
use std::path::Path;

use zenwebp::{EncodeRequest, LossyConfig, PixelLayout};

pub fn convert_to_webp(source_path: &Path) -> Result<Vec<u8>, String> {
    let meta = fs::metadata(source_path).map_err(|e| format!("stat: {e}"))?;
    if meta.len() == 0 {
        return Err("empty file".into());
    }
    let ext = source_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();

    // Try the extension-based decoder first; if it fails, try the other format.
    // Files are sometimes misnamed (a JPEG with a .png extension, etc.).
    let (pixels, layout, width, height) = match ext.as_str() {
        "png" => decode_png(source_path).or_else(|_e| decode_jpeg(source_path))?,
        "jpg" | "jpeg" => decode_jpeg(source_path).or_else(|_e| decode_png(source_path))?,
        other => return Err(format!("unsupported image format: {other}")),
    };

    let config = LossyConfig::new().with_quality(90.0);
    let webp = EncodeRequest::lossy(&config, &pixels, layout, width, height)
        .encode()
        .map_err(|e| format!("webp encode: {e}"))?;
    Ok(webp)
}

fn decode_png(path: &Path) -> Result<(Vec<u8>, PixelLayout, u32, u32), String> {
    let file = fs::File::open(path).map_err(|e| format!("open png: {e}"))?;
    let reader = BufReader::new(file);
    let mut decoder = png::Decoder::new(reader);
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(|e| format!("png info: {e}"))?;
    let mut buf = vec![0; reader.output_buffer_size().unwrap_or(0)];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| format!("png decode: {e}"))?;
    buf.truncate(info.buffer_size());

    // Convert grayscale to RGB(A) since zenwebp doesn't accept grayscale directly.
    let (pixels, layout) = match info.color_type {
        png::ColorType::Rgba => (buf, PixelLayout::Rgba8),
        png::ColorType::Rgb => (buf, PixelLayout::Rgb8),
        png::ColorType::Grayscale => {
            let mut rgb = Vec::with_capacity(buf.len() * 3);
            for &g in &buf {
                rgb.extend_from_slice(&[g, g, g]);
            }
            (rgb, PixelLayout::Rgb8)
        }
        png::ColorType::GrayscaleAlpha => {
            let mut rgba = Vec::with_capacity(buf.len() * 2);
            for chunk in buf.chunks_exact(2) {
                let g = chunk[0];
                let a = chunk[1];
                rgba.extend_from_slice(&[g, g, g, a]);
            }
            (rgba, PixelLayout::Rgba8)
        }
        png::ColorType::Indexed => unreachable!("EXPAND transform should have converted indexed"),
    };

    Ok((pixels, layout, info.width, info.height))
}

fn decode_jpeg(path: &Path) -> Result<(Vec<u8>, PixelLayout, u32, u32), String> {
    let data = fs::read(path).map_err(|e| format!("read jpeg: {e}"))?;
    let cursor = Cursor::new(data);
    let mut decoder = zune_jpeg::JpegDecoder::new(cursor);
    let pixels = decoder.decode().map_err(|e| format!("jpeg decode: {e}"))?;
    let info = decoder.info().ok_or("jpeg: no image info")?;
    let width = info.width as u32;
    let height = info.height as u32;

    // Grayscale JPEG: expand each 1-byte pixel to 3-byte RGB so zenwebp can encode it.
    let (pixels, layout) = match info.components {
        3 => (pixels, PixelLayout::Rgb8),
        1 => {
            let mut rgb = Vec::with_capacity(pixels.len() * 3);
            for &g in &pixels {
                rgb.extend_from_slice(&[g, g, g]);
            }
            (rgb, PixelLayout::Rgb8)
        }
        n => return Err(format!("unexpected jpeg components: {n}")),
    };

    Ok((pixels, layout, width, height))
}
