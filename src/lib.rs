//! fisheye-flatten: shared geometry, lens model, and remap engine.
//!
//! The CLI (`src/main.rs`) is a thin layer of subcommands over the primitives
//! here. Every operation is the same backward remap — *for each output pixel,
//! find the source pixel and sample it* — so the parallel engine ([`render`])
//! and the lens/projection helpers are shared across subcommands.

use image::{DynamicImage, GenericImageView, ImageBuffer, Rgb, RgbImage};
use image::codecs::jpeg::JpegEncoder;
use little_exif::exif_tag::ExifTag;
use little_exif::metadata::Metadata;
use rayon::prelude::*;
use std::path::Path;
use std::fs::File;
use std::io::{BufReader, BufWriter};

/// Output projection for the flatten operation.
#[derive(Debug, Clone, PartialEq)]
pub enum Projection { Rectilinear, Cylindrical }

impl std::str::FromStr for Projection {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "rect" | "rectilinear" => Ok(Projection::Rectilinear),
            "cyl"  | "cylindrical" => Ok(Projection::Cylindrical),
            _ => Err(format!("Unknown projection '{}'. Use rect or cyl.", s)),
        }
    }
}

// ---------------------------------------------------------------------------
// EXIF / body identification
// ---------------------------------------------------------------------------

pub fn read_exif_focal_length(path: &Path) -> Option<f64> {
    let file = File::open(path).ok()?;
    let mut buf = BufReader::new(file);
    let exif = exif::Reader::new().read_from_container(&mut buf).ok()?;
    let f = exif.get_field(exif::Tag::FocalLength, exif::In::PRIMARY)?;
    match &f.value {
        exif::Value::Rational(v) if !v.is_empty() =>
            Some(v[0].num as f64 / v[0].denom as f64),
        _ => None,
    }
}

pub fn read_exif_model(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    let mut buf = BufReader::new(file);
    let exif = exif::Reader::new().read_from_container(&mut buf).ok()?;
    Some(exif.get_field(exif::Tag::Model, exif::In::PRIMARY)?
        .display_value().to_string())
}

pub fn read_exif_image_description(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    let mut buf = BufReader::new(file);
    let exif = exif::Reader::new().read_from_container(&mut buf).ok()?;
    Some(exif.get_field(exif::Tag::ImageDescription, exif::In::PRIMARY)?
        .display_value().to_string())
}

/// The exact geometry `refish` used, stamped into the output's ImageDescription
/// so a bare `flatten` can read it back and invert that refish precisely.
pub struct RefishStamp {
    pub theta_max: f64,   // radians
    pub proj: Projection,
    pub r_circle: f64,    // px
}

/// Format the stamp token embedded at the start of refish's ImageDescription.
pub fn refish_stamp_token(theta_max: f64, proj: &Projection, r_circle: f64) -> String {
    let p = match proj { Projection::Cylindrical => "cyl", Projection::Rectilinear => "rect" };
    format!("[ff-refish theta={:.6} proj={} r={:.2}]", theta_max, p, r_circle)
}

/// Parse a refish stamp out of an image's ImageDescription, if present.
pub fn read_refish_stamp(path: &Path) -> Option<RefishStamp> {
    let desc = read_exif_image_description(path)?;
    let start = desc.find("[ff-refish ")?;
    let body = &desc[start + "[ff-refish ".len()..];
    let body = &body[..body.find(']')?];
    let (mut theta, mut proj, mut r) = (None, None, None);
    for tok in body.split_whitespace() {
        if let Some((k, v)) = tok.split_once('=') {
            match k {
                "theta" => theta = v.parse::<f64>().ok(),
                "proj" => proj = v.parse::<Projection>().ok(),
                "r" => r = v.parse::<f64>().ok(),
                _ => {}
            }
        }
    }
    Some(RefishStamp { theta_max: theta?, proj: proj?, r_circle: r? })
}

pub fn detect_crop_factor(model: &str) -> f64 {
    let m = model.to_lowercase();
    if m.contains("gfx") { 0.790 }
    else if ["z 8","z8","z 9","z9","z6","z7","d850","d800","d810",
              "a7","a1","a9","r5","r3","r6"].iter().any(|s| m.contains(s)) { 1.0 }
    else { eprintln!("Note: unrecognised camera '{}'; assuming full-frame.", model); 1.0 }
}

/// Sensor pixel pitch in mm, read from EXIF FocalPlaneXResolution if present.
/// This is the on-sensor pixel spacing and is what converts a focal length in
/// mm to a focal length in pixels (f_pix = f_mm / pitch_mm).
fn exif_pixel_pitch_mm(path: &Path) -> Option<f64> {
    let file = File::open(path).ok()?;
    let mut buf = BufReader::new(file);
    let exif = exif::Reader::new().read_from_container(&mut buf).ok()?;
    let xres = exif.get_field(exif::Tag::FocalPlaneXResolution, exif::In::PRIMARY)?;
    let res = match &xres.value {
        exif::Value::Rational(v) if !v.is_empty() && v[0].num != 0 =>
            v[0].num as f64 / v[0].denom as f64,
        _ => return None,
    };
    // FocalPlaneResolutionUnit: 2=inch (default), 3=cm, 4=mm, 5=µm
    let unit = exif.get_field(exif::Tag::FocalPlaneResolutionUnit, exif::In::PRIMARY)
        .and_then(|f| f.value.get_uint(0)).unwrap_or(2);
    let unit_mm = match unit { 3 => 10.0, 4 => 1.0, 5 => 0.001, _ => 25.4 };
    Some(unit_mm / res) // res is pixels-per-unit → mm-per-pixel
}

/// Physical sensor width in mm for known bodies (fallback when EXIF lacks
/// FocalPlaneXResolution). Used as sensor_width / image_width = pixel pitch.
fn sensor_width_mm(model: &str) -> Option<f64> {
    let m = model.to_lowercase();
    if m.contains("gfx") { Some(43.8) } // Fuji GFX medium format
    else if ["z 8","z8","z 9","z9","z6","z7","d850","d810","d800",
             "a7","a1","a9","r5","r3","r6"].iter().any(|s| m.contains(s)) { Some(35.9) }
    else { None }
}

/// Best estimate of pixel pitch (mm/px): EXIF first, then a per-model sensor
/// width, then a crop-factor-derived 35mm-equivalent width as a last resort.
pub fn sensor_pixel_pitch_mm(path: &Path, model: Option<&str>, crop_factor: f64, img_w: u32) -> f64 {
    if let Some(p) = exif_pixel_pitch_mm(path) { return p; }
    if let Some(w) = model.and_then(sensor_width_mm) { return w / img_w as f64; }
    (36.0 / crop_factor) / img_w as f64 // full-frame width / crop = sensor width
}

// ---------------------------------------------------------------------------
// Laowa 8-15mm lens model + focal-length recovery
// ---------------------------------------------------------------------------

pub fn laowa_half_aov_deg(focal_mm_ff: f64) -> f64 {
    const TABLE: &[(f64, f64)] = &[
        (8.0, 90.0), (9.0, 77.0), (10.0, 65.0), (11.0, 57.5),
        (12.0, 53.0), (13.0, 48.0), (14.0, 43.5), (15.0, 40.0),
    ];
    let fl = focal_mm_ff.clamp(8.0, 15.0);
    for i in 0..TABLE.len() - 1 {
        let (f0, a0) = TABLE[i];
        let (f1, a1) = TABLE[i + 1];
        if fl >= f0 && fl <= f1 { return a0 + (fl - f0) / (f1 - f0) * (a1 - a0); }
    }
    TABLE.last().unwrap().1
}

/// Forward model: predicted image-circle radius (px) for a physical focal
/// length, given the body. Equidistant fisheye: r = (f_mm/pitch) · θ_max(f).
pub fn model_circle_radius_px(focal_mm: f64, pitch_mm: f64, crop_factor: f64, calib_scale: f64) -> f64 {
    let theta = laowa_half_aov_deg((focal_mm * crop_factor).clamp(8.0, 15.0)).to_radians();
    calib_scale * (focal_mm / pitch_mm) * theta
}

/// Invert the forward model: which 8–15mm focal length(s) produce the measured
/// image-circle radius? Returns every root (the r(f) curve is not guaranteed
/// monotonic on large sensors), refined by linear interpolation. Empty → the
/// single closest focal is returned by the caller.
fn focal_candidates_from_circle(
    r_meas_px: f64, pitch_mm: f64, crop_factor: f64, calib_scale: f64,
) -> Vec<f64> {
    let r = |f: f64| model_circle_radius_px(f, pitch_mm, crop_factor, calib_scale) - r_meas_px;
    const STEPS: usize = 700;
    let f_at = |i: usize| 8.0 + (15.0 - 8.0) * i as f64 / STEPS as f64;
    let mut roots = Vec::new();
    let mut prev = r(f_at(0));
    for i in 1..=STEPS {
        let (f0, f1) = (f_at(i - 1), f_at(i));
        let cur = r(f1);
        if prev == 0.0 || (prev < 0.0) != (cur < 0.0) {
            // linear interpolation of the crossing
            let t = if cur != prev { prev / (prev - cur) } else { 0.0 };
            roots.push(f0 + t * (f1 - f0));
        }
        prev = cur;
    }
    roots
}

/// Resolve a focal length when neither the CLI nor EXIF provides one, by
/// inverting the image-circle radius. Prints what it decided (and any
/// ambiguity), and falls back to 15mm if there is no usable circle.
pub fn auto_focal_length(
    r_circle: f64, is_circular: bool, pitch_mm: f64, crop_factor: f64, calib_scale: f64,
) -> f64 {
    if !is_circular {
        eprintln!("No EXIF focal length and no image circle (full-frame fisheye); \
                   assuming 15mm. Override with -f.");
        return 15.0;
    }
    let cands = focal_candidates_from_circle(r_circle, pitch_mm, crop_factor, calib_scale);
    match cands.as_slice() {
        [] => {
            eprintln!("No EXIF focal length; image circle ({:.0}px) outside the 8–15mm \
                       model range; assuming 15mm. Override with -f.", r_circle);
            15.0
        }
        [f] => {
            eprintln!("Auto-detected focal length {:.1}mm from {:.0}px image circle \
                       (pitch {:.2}µm).", f, r_circle, pitch_mm * 1000.0);
            *f
        }
        many => {
            let chosen = *many.last().unwrap();
            let list = many.iter().map(|f| format!("{:.1}", f))
                .collect::<Vec<_>>().join(" or ");
            eprintln!("Ambiguous focal length: {:.0}px circle matches {}mm; using \
                       {:.1}mm. Override with -f for certainty.", r_circle, list, chosen);
            chosen
        }
    }
}

// ---------------------------------------------------------------------------
// Image-circle detection
// ---------------------------------------------------------------------------

pub fn detect_image_circle(img: &DynamicImage) -> (f64, bool) {
    let (w, h) = img.dimensions();
    let cx = w as f64 / 2.0; let cy = h as f64 / 2.0;
    let half_diag = (cx*cx + cy*cy).sqrt();
    const BLACK: u8 = 20; const N: usize = 72;
    let mut radii = Vec::with_capacity(N);
    for i in 0..N {
        let a = std::f64::consts::TAU * i as f64 / N as f64;
        let (ca, sa) = (a.cos(), a.sin());
        for r in (1..=(half_diag as u32)).rev() {
            let px = (cx + r as f64 * ca).round() as i64;
            let py = (cy + r as f64 * sa).round() as i64;
            if px < 0 || py < 0 || px >= w as i64 || py >= h as i64 { continue; }
            let p = img.get_pixel(px as u32, py as u32).0;
            if p[0].max(p[1]).max(p[2]) > BLACK { radii.push(r as f64); break; }
        }
    }
    if !radii.is_empty() {
        radii.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let med = radii[radii.len() / 2];
        let spread = radii.last().unwrap() - radii.first().unwrap();
        if spread < 0.15 * med { return (med, true); }
    }
    // No clean all-around circle. Common case: the image circle is taller than
    // the sensor (top & bottom cropped) but narrower (left/right fall off to
    // black). The center row still spans the full diameter, so measure that.
    if let Some(r_h) = detect_circle_horizontal(img) {
        return (r_h, true);
    }
    (half_diag, false)
}

/// Measure the image-circle radius from the horizontal extent across the
/// vertical center, for frames where the top and bottom are cropped but the
/// left and right edges fall off to black. Scans a thin band of rows around the
/// center, finds the black→image boundary inward from each frame edge, and
/// returns the radius only if BOTH sides fall off to black inside the frame and
/// the two half-widths agree (i.e. a centered circle, not a full-frame image).
fn detect_circle_horizontal(img: &DynamicImage) -> Option<f64> {
    let (w, h) = img.dimensions();
    let cx = w as f64 / 2.0;
    let cy = h as i64 / 2;
    const BLACK: u8 = 20;
    let band = (h / 200).max(1) as i64; // ~±0.5% of height around center
    let lum = |x: u32, y: u32| { let p = img.get_pixel(x, y).0; p[0].max(p[1]).max(p[2]) };
    let (mut rl, mut rr) = (Vec::new(), Vec::new());
    for dy in -band..=band {
        let y = cy + dy;
        if y < 0 || y >= h as i64 { continue; }
        let y = y as u32;
        if lum(cx as u32, y) <= BLACK { continue; } // center must be image content
        // First non-black scanning inward from each frame edge.
        let left  = (0..cx as u32).find(|&x| lum(x, y) > BLACK);
        let right = (cx as u32 + 1..w).rev().find(|&x| lum(x, y) > BLACK);
        let (Some(l), Some(r)) = (left, right) else { continue };
        // Require a real black margin on both sides (edge not at the frame border).
        if l <= 1 || r >= w - 2 { continue; }
        rl.push(cx - l as f64);
        rr.push(r as f64 - cx);
    }
    if rl.len() < 3 { return None; }
    let median = |mut v: Vec<f64>| { v.sort_by(|a, b| a.partial_cmp(b).unwrap()); v[v.len() / 2] };
    let (ml, mr) = (median(rl), median(rr));
    if (ml - mr).abs() > 0.10 * ml.max(mr) { return None; } // must be centered
    Some((ml + mr) / 2.0)
}

// ---------------------------------------------------------------------------
// Sampling, rendering, output
// ---------------------------------------------------------------------------

/// Bilinearly sample `img` at fractional (x, y). Returns `None` outside bounds
/// so callers can paint out-of-frame pixels black.
pub fn bilinear(img: &DynamicImage, x: f64, y: f64) -> Option<[u8; 3]> {
    let (w, h) = img.dimensions();
    if x < 0.0 || y < 0.0 || x >= w as f64 || y >= h as f64 { return None; }
    let x0 = x.floor() as i64; let y0 = y.floor() as i64;
    let clx = |v: i64| v.clamp(0, w as i64 - 1) as u32;
    let cly = |v: i64| v.clamp(0, h as i64 - 1) as u32;
    let p00 = img.get_pixel(clx(x0),     cly(y0)).0;
    let p10 = img.get_pixel(clx(x0 + 1), cly(y0)).0;
    let p01 = img.get_pixel(clx(x0),     cly(y0 + 1)).0;
    let p11 = img.get_pixel(clx(x0 + 1), cly(y0 + 1)).0;
    let tx = x - x0 as f64; let ty = y - y0 as f64;
    let l = |a: u8, b: u8, t: f64| -> u8 {
        (a as f64 + (b as f64 - a as f64) * t).round() as u8
    };
    Some([l(l(p00[0],p10[0],tx),l(p01[0],p11[0],tx),ty),
          l(l(p00[1],p10[1],tx),l(p01[1],p11[1],tx),ty),
          l(l(p00[2],p10[2],tx),l(p01[2],p11[2],tx),ty)])
}

/// The shared backward-remap engine. Builds an `out_w × out_h` image by calling
/// `sample(ox, oy)` for every output pixel, in parallel across rows. Every
/// subcommand (flatten/refish/tunnel) is just a different `sample` closure.
pub fn render<F>(out_w: u32, out_h: u32, sample: F) -> RgbImage
where
    F: Fn(u32, u32) -> [u8; 3] + Sync,
{
    let pixels: Vec<(u32, u32, [u8; 3])> = (0..out_h)
        .into_par_iter()
        .flat_map(|oy| {
            let mut row = Vec::with_capacity(out_w as usize);
            for ox in 0..out_w { row.push((ox, oy, sample(ox, oy))); }
            row
        })
        .collect();
    let mut out: RgbImage = ImageBuffer::new(out_w, out_h);
    for (ox, oy, rgb) in pixels { out.put_pixel(ox, oy, Rgb(rgb)); }
    out
}

pub fn is_jpeg(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).map(|e| e.to_lowercase()).as_deref(),
        Some("jpg") | Some("jpeg") | Some("jpe") | Some("jfif")
    )
}

/// Save an image, encoding JPEG at the requested quality (the image crate's
/// `save()` defaults to 75, well below camera output). Non-JPEG formats use the
/// default encoder for their extension.
pub fn save_image(img: &RgbImage, path: &Path, quality: u8) -> Result<(), image::ImageError> {
    if is_jpeg(path) {
        let file = File::create(path)?;
        let mut enc = JpegEncoder::new_with_quality(BufWriter::new(file), quality.clamp(1, 100));
        enc.encode_image(img)
    } else {
        img.save(path)
    }
}

/// Embed the effective CLI/processing settings into the output JPEG's EXIF.
///
/// The settings summary goes into `ImageDescription` (EXIF 0x010E) — the field
/// Google Photos surfaces as the photo's description in its info panel — and a
/// short tool tag goes into `Software` (0x0131), the standard "processed by"
/// field. Both are common, human-visible fields, not obscure maker notes.
pub fn write_settings_exif(path: &Path, description: &str, software: &str) -> Result<(), String> {
    let mut meta = Metadata::new();
    meta.set_tag(ExifTag::ImageDescription(description.to_string()));
    meta.set_tag(ExifTag::Software(software.to_string()));
    meta.write_to_file(path).map_err(|e| e.to_string())
}
