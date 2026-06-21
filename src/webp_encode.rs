use std::fs;
use std::io::{BufReader, Cursor};
use std::path::Path;

use zenwebp::{EncodeRequest, LossyConfig, PixelLayout};

pub fn convert_to_webp(source_path: &Path) -> Result<Vec<u8>, String> {
    let ext = source_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();

    let (pixels, layout, width, height) = match ext.as_str() {
        "png" => decode_png(source_path)?,
        "jpg" | "jpeg" => decode_jpeg(source_path)?,
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
    let info = reader.next_frame(&mut buf).map_err(|e| format!("png decode: {e}"))?;
    buf.truncate(info.buffer_size());

    let (layout, bytes_per_pixel) = match info.color_type {
        png::ColorType::Rgba => (PixelLayout::Rgba8, 4),
        png::ColorType::Rgb => (PixelLayout::Rgb8, 3),
        png::ColorType::Grayscale | png::ColorType::GrayscaleAlpha => {
            return Err("grayscale PNG: not supported".into());
        }
        png::ColorType::Indexed => unreachable!("EXPAND transform should have converted indexed"),
    };

    let expected = info.width as usize * info.height as usize * bytes_per_pixel;
    if buf.len() != expected {
        return Err(format!(
            "png size mismatch: got {} bytes, expected {expected}",
            buf.len(),
        ));
    }

    Ok((buf, layout, info.width, info.height))
}

fn decode_jpeg(path: &Path) -> Result<(Vec<u8>, PixelLayout, u32, u32), String> {
    let data = fs::read(path).map_err(|e| format!("read jpeg: {e}"))?;
    let cursor = Cursor::new(data);
    let mut decoder = zune_jpeg::JpegDecoder::new(cursor);
    let pixels = decoder.decode().map_err(|e| format!("jpeg decode: {e}"))?;
    let info = decoder.info().ok_or("jpeg: no image info")?;
    let width = info.width as u32;
    let height = info.height as u32;

    let layout = match info.components {
        3 => PixelLayout::Rgb8,
        1 => return Err("grayscale JPEG: not supported".into()),
        n => return Err(format!("unexpected jpeg components: {n}")),
    };

    Ok((pixels, layout, width, height))
}
