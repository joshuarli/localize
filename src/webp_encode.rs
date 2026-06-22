use std::fs;
use std::io::{BufReader, Cursor, Read};
use std::path::Path;
use std::sync::OnceLock;

use memmap2::Mmap;
use zenwebp::{EncodeRequest, LossyConfig, PixelLayout, Preset};

/// Result of attempting to convert an image to WebP.
pub enum ConvertResult {
    /// Successfully encoded to WebP — the output bytes are ready to write.
    Converted(Vec<u8>),
    /// File is already a WebP image (RIFF....WEBP detected). No re-encoding
    /// needed, but the caller should fix the file extension if it is wrong.
    AlreadyWebp,
}

/// Encoder config created once and reused across all conversions.
///
/// Method 3 gives ~1.5–2× throughput over the default (4) with negligible
/// quality loss.  Preset::Photo tunes SNS and loop-filter defaults for
/// natural images (the common case for JPEG sources).
static WEBP_CONFIG: OnceLock<LossyConfig> = OnceLock::new();

fn webp_config() -> &'static LossyConfig {
    WEBP_CONFIG.get_or_init(|| {
        LossyConfig::new()
            .with_quality(90.0)
            .with_method(3)
            .with_preset_value(Preset::Photo)
    })
}

/// Read the first 12 bytes and check for the RIFF....WEBP signature.
/// Returns `Ok(false)` for files too small to be valid WebP.
fn sniff_webp(path: &Path) -> Result<bool, String> {
    let mut file = fs::File::open(path).map_err(|e| format!("open for sniff: {e}"))?;
    let mut header = [0u8; 12];
    if file.read_exact(&mut header).is_err() {
        return Ok(false);
    }
    Ok(&header[0..4] == b"RIFF" && &header[8..12] == b"WEBP")
}

pub fn convert_to_webp(source_path: &Path) -> Result<ConvertResult, String> {
    let meta = fs::metadata(source_path).map_err(|e| format!("stat: {e}"))?;
    if meta.len() == 0 {
        return Err("empty file".into());
    }

    // Sniff for WebP before trusting the extension. Files are sometimes
    // misnamed (a WebP saved with a .jpg extension, etc.).
    if sniff_webp(source_path)? {
        return Ok(ConvertResult::AlreadyWebp);
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

    let config = webp_config();
    let webp = EncodeRequest::lossy(config, &pixels, layout, width, height)
        .encode()
        .map_err(|e| format!("webp encode: {e}"))?;
    Ok(ConvertResult::Converted(webp))
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

    let (pixels, layout) = match info.color_type {
        png::ColorType::Rgba => (buf, PixelLayout::Rgba8),
        png::ColorType::Rgb => (buf, PixelLayout::Rgb8),
        png::ColorType::Grayscale => {
            let gray_len = buf.len();
            let mut rgb = vec![0u8; gray_len * 3];
            for (i, &g) in buf.iter().enumerate() {
                let base = i * 3;
                rgb[base] = g;
                rgb[base + 1] = g;
                rgb[base + 2] = g;
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
    let file = fs::File::open(path).map_err(|e| format!("open jpeg: {e}"))?;
    // mmap avoids the file-sized Vec allocation that fs::read would incur.
    // SAFETY: the mmap lives for the duration of this function; the decoder
    // and cursor borrow from it and are dropped before it. The file is a
    // local image we just stat'd — truncation races are not a practical concern.
    let mmap = unsafe { Mmap::map(&file).map_err(|e| format!("mmap: {e}"))? };
    let cursor = Cursor::new(&mmap[..]);
    let mut decoder = zune_jpeg::JpegDecoder::new(cursor);
    let pixels = decoder.decode().map_err(|e| format!("jpeg decode: {e}"))?;
    let info = decoder.info().ok_or("jpeg: no image info")?;
    let width = info.width as u32;
    let height = info.height as u32;

    let (pixels, layout) = match info.components {
        3 => (pixels, PixelLayout::Rgb8),
        1 => {
            let gray_len = pixels.len();
            let mut rgb = vec![0u8; gray_len * 3];
            for (i, &g) in pixels.iter().enumerate() {
                let base = i * 3;
                rgb[base] = g;
                rgb[base + 1] = g;
                rgb[base + 2] = g;
            }
            (rgb, PixelLayout::Rgb8)
        }
        n => return Err(format!("unexpected jpeg components: {n}")),
    };

    Ok((pixels, layout, width, height))
}
