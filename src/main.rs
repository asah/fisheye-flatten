/// defish — Laowa 8-15mm Fisheye → Rectilinear strip remapper
///
/// Model: equidistant fisheye  r = f * θ  (Laowa 8-15 is very close to equidistant)
///
/// Strategy:
///   1. Crop the vertical-center strip (default 30% of frame height).
///   2. For every destination pixel in the output, compute the angle it would
///      subtend in a rectilinear projection, then map it back through the
///      fisheye equation to the source pixel.  This "unwarps" the horizontal
///      barrel distortion so straight horizontal lines become straight.
///   3. Vertical direction uses the same fisheye-to-angle inverse and then
///      re-projects to rectilinear, giving full consistency.
///
/// Lens profiles built in (Laowa 8-15mm f/2.8):
///   The lens is ~equidistant. The only parameter that changes with focal
///   length is the full-frame diagonal AoV, which we derive from focal length.
///   EXIF focal length is read automatically; override with --focal-length.
///
/// Output is the same pixel width as the crop, same height as the strip.
/// Use --scale to upscale/downscale.

use clap::Parser;
use image::{DynamicImage, GenericImageView, ImageBuffer, Rgb};
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::fs::File;
use std::io::BufReader;

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Parser, Debug)]
#[command(
    name = "defish",
    about = "Crop & defish Laowa 8-15mm fisheye images to a flat strip",
    long_about = "Crops the vertical-center strip of a fisheye photo and \
                  remaps it to rectilinear projection. Optimised for the \
                  Laowa 8-15mm f/2.8 fisheye on any sensor (Fuji GFX 100S II, \
                  Nikon Z8, etc.). Reads focal length from EXIF automatically."
)]
struct Args {
    /// Input image (JPEG/PNG/TIFF)
    input: PathBuf,

    /// Output image path (extension determines format)
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Height of center strip to keep, as fraction of frame height (0..1)
    #[arg(short = 'p', long, default_value_t = 0.30)]
    strip_percent: f64,

    /// Override focal length in mm (auto-read from EXIF if omitted)
    #[arg(short = 'f', long)]
    focal_length: Option<f64>,

    /// Sensor crop factor relative to full-frame (1.0=FF, 0.79=GFX 100S II, etc.)
    /// GFX 100S II medium format: 43.8x32.9mm diagonal ~55mm, crop factor ~0.79
    /// Nikon Z8 full frame: crop factor 1.0
    #[arg(short = 'c', long)]
    crop_factor: Option<f64>,

    /// Output scale multiplier (1.0 = same pixel dimensions as input strip)
    #[arg(short = 's', long, default_value_t = 1.0)]
    scale: f64,

    /// Interpolation: nearest | bilinear  (default: bilinear)
    #[arg(short = 'i', long, default_value = "bilinear")]
    interp: String,

    /// Verbose logging
    #[arg(short = 'v', long)]
    verbose: bool,
}

// ---------------------------------------------------------------------------
// EXIF helpers
// ---------------------------------------------------------------------------

fn read_exif_focal_length(path: &Path) -> Option<f64> {
    let file = File::open(path).ok()?;
    let mut bufreader = BufReader::new(file);
    let exifreader = exif::Reader::new();
    let exif = exifreader.read_from_container(&mut bufreader).ok()?;

    let field = exif.get_field(exif::Tag::FocalLength, exif::In::PRIMARY)?;
    match field.value {
        exif::Value::Rational(ref v) if !v.is_empty() => {
            Some(v[0].num as f64 / v[0].denom as f64)
        }
        _ => None,
    }
}

fn read_exif_make_model(path: &Path) -> Option<(String, String)> {
    let file = File::open(path).ok()?;
    let mut bufreader = BufReader::new(file);
    let exifreader = exif::Reader::new();
    let exif = exifreader.read_from_container(&mut bufreader).ok()?;

    let make = exif
        .get_field(exif::Tag::Make, exif::In::PRIMARY)
        .map(|f| f.display_value().to_string())
        .unwrap_or_default();
    let model = exif
        .get_field(exif::Tag::Model, exif::In::PRIMARY)
        .map(|f| f.display_value().to_string())
        .unwrap_or_default();
    Some((make, model))
}

// ---------------------------------------------------------------------------
// Sensor / lens database
// ---------------------------------------------------------------------------

/// GFX 100S II sensor: 43.8 x 32.9 mm, diagonal = 54.75 mm
/// Full-frame: 36 x 24 mm, diagonal = 43.27 mm
/// Crop factor = 43.27 / 54.75 = 0.790
fn detect_crop_factor(model: &str) -> f64 {
    let m = model.to_lowercase();
    if m.contains("gfx") {
        0.790
    } else if m.contains("z 8") || m.contains("z8") || m.contains("z 9") || m.contains("z9")
        || m.contains("z6") || m.contains("z7") || m.contains("d850") || m.contains("d800")
        || m.contains("d810") || m.contains("d780")
    {
        1.0
    } else if m.contains("a7") || m.contains("a1") || m.contains("a9") {
        1.0
    } else if m.contains("r5") || m.contains("r3") || m.contains("r6") {
        1.0
    } else {
        eprintln!("Warning: unknown camera model '{}', assuming full-frame (crop=1.0). \
                   Use --crop-factor to override.", model);
        1.0
    }
}

// ---------------------------------------------------------------------------
// Fisheye geometry
// ---------------------------------------------------------------------------

/// Laowa 8-15mm half diagonal AoV at a given focal length (full-frame equivalent).
/// These are derived from Laowa published specs + community measurements.
fn laowa_8_15_half_aov_deg(focal_mm: f64) -> f64 {
    let table: &[(f64, f64)] = &[
        (8.0,  90.0),
        (9.0,  77.0),
        (10.0, 65.0),
        (11.0, 57.5),
        (12.0, 53.0),
        (13.0, 48.0),
        (14.0, 43.5),
        (15.0, 40.0),
    ];

    let fl = focal_mm.clamp(8.0, 15.0);

    for i in 0..table.len() - 1 {
        let (f0, a0) = table[i];
        let (f1, a1) = table[i + 1];
        if fl >= f0 && fl <= f1 {
            let t = (fl - f0) / (f1 - f0);
            return a0 + t * (a1 - a0);
        }
    }
    table.last().unwrap().1
}

/// Inverse remap: given normalized rectilinear dest coords, find fisheye src coords.
/// rect_x, rect_y: in rectilinear space, normalized so max_rect_{x,y} = ±1 at edges.
/// half_aov_rad: half diagonal AoV of the lens.
/// Returns fisheye source coords normalized to image-circle radius = 1.
fn rectilinear_to_fisheye(
    rect_x: f64,
    rect_y: f64,
    half_aov_rad: f64,
) -> Option<(f64, f64)> {
    let r_rect = (rect_x * rect_x + rect_y * rect_y).sqrt();
    let theta = r_rect.atan(); // rectilinear tangent angle → actual angle

    if theta >= half_aov_rad {
        return None;
    }

    let phi = rect_y.atan2(rect_x);

    // equidistant: r_fish (norm) = theta / half_aov_rad
    let r_fish = theta / half_aov_rad;

    Some((r_fish * phi.cos(), r_fish * phi.sin()))
}

// ---------------------------------------------------------------------------
// Interpolation
// ---------------------------------------------------------------------------

fn bilinear(img: &DynamicImage, x: f64, y: f64) -> [u8; 3] {
    let (w, h) = img.dimensions();
    let x0 = x.floor() as i64;
    let y0 = y.floor() as i64;

    let clamp_x = |v: i64| v.clamp(0, w as i64 - 1) as u32;
    let clamp_y = |v: i64| v.clamp(0, h as i64 - 1) as u32;

    let p00 = img.get_pixel(clamp_x(x0),   clamp_y(y0)).0;
    let p10 = img.get_pixel(clamp_x(x0+1), clamp_y(y0)).0;
    let p01 = img.get_pixel(clamp_x(x0),   clamp_y(y0+1)).0;
    let p11 = img.get_pixel(clamp_x(x0+1), clamp_y(y0+1)).0;

    let tx = x - x0 as f64;
    let ty = y - y0 as f64;

    let lerp = |a: u8, b: u8, t: f64| -> u8 {
        (a as f64 + (b as f64 - a as f64) * t).round() as u8
    };

    [
        lerp(lerp(p00[0], p10[0], tx), lerp(p01[0], p11[0], tx), ty),
        lerp(lerp(p00[1], p10[1], tx), lerp(p01[1], p11[1], tx), ty),
        lerp(lerp(p00[2], p10[2], tx), lerp(p01[2], p11[2], tx), ty),
    ]
}

fn nearest(img: &DynamicImage, x: f64, y: f64) -> [u8; 3] {
    let (w, h) = img.dimensions();
    let xi = (x.round() as i64).clamp(0, w as i64 - 1) as u32;
    let yi = (y.round() as i64).clamp(0, h as i64 - 1) as u32;
    let p = img.get_pixel(xi, yi).0;
    [p[0], p[1], p[2]]
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    let args = Args::parse();

    let img = image::open(&args.input).unwrap_or_else(|e| {
        eprintln!("Cannot open input image: {e}");
        std::process::exit(1);
    });
    let (src_w, src_h) = img.dimensions();

    if args.verbose {
        eprintln!("Input: {}x{} px", src_w, src_h);
    }

    let exif_fl = read_exif_focal_length(&args.input);
    let exif_cam = read_exif_make_model(&args.input);

    let focal_mm = args.focal_length.or(exif_fl).unwrap_or_else(|| {
        eprintln!("Warning: no EXIF focal length found and --focal-length not set; assuming 15mm.");
        15.0
    });

    let crop_factor = args.crop_factor.unwrap_or_else(|| {
        if let Some((_, model)) = &exif_cam {
            detect_crop_factor(model)
        } else {
            eprintln!("Warning: no EXIF camera model found; assuming full-frame (crop=1.0).");
            1.0
        }
    });

    if args.verbose {
        if let Some((make, model)) = &exif_cam {
            eprintln!("Camera: {} {}", make, model);
        }
        eprintln!("Focal length: {:.1}mm  |  Crop factor: {:.3}", focal_mm, crop_factor);
    }

    // Convert to FF-equivalent focal length for AoV lookup
    // GFX (crop 0.79) at 15mm sees same angle as FF at 15*0.79=11.85mm
    let fl_ff_equiv = focal_mm * crop_factor;
    let half_aov_deg = laowa_8_15_half_aov_deg(fl_ff_equiv.clamp(8.0, 15.0));
    let half_aov_rad = half_aov_deg.to_radians();

    if args.verbose {
        eprintln!(
            "FF-equiv focal: {:.1}mm  |  Lens half-AoV: {:.1}°  ({:.1}° total diagonal)",
            fl_ff_equiv, half_aov_deg, half_aov_deg * 2.0
        );
    }

    // Strip geometry
    let strip_frac = args.strip_percent.clamp(0.01, 1.0);
    let strip_h = ((src_h as f64 * strip_frac).round() as u32).max(1);
    let strip_y0 = (src_h - strip_h) / 2;

    if args.verbose {
        eprintln!(
            "Strip: y=[{}, {}], height={} px ({:.1}% of frame)",
            strip_y0, strip_y0 + strip_h, strip_h, strip_frac * 100.0
        );
    }

    let out_w = ((src_w as f64 * args.scale).round() as u32).max(1);
    let out_h = ((strip_h as f64 * args.scale).round() as u32).max(1);

    if args.verbose {
        eprintln!("Output: {}x{} px", out_w, out_h);
    }

    // Normalisation: we normalize pixel coords so the image-circle fills from
    // -1 to +1 in the shorter dimension. For a full-frame-filling fisheye at 15mm
    // the circle just covers the corners, so norm_r = half-diagonal of sensor.
    let cx = src_w as f64 / 2.0;
    let cy = src_h as f64 / 2.0;
    let norm_r = (cx * cx + cy * cy).sqrt(); // half-diagonal in pixels

    // Angular extent covered by the strip half-height and half-width
    let strip_half_h_norm = (strip_h as f64 / 2.0) / norm_r;
    let max_theta_y = (strip_half_h_norm * half_aov_rad).min(half_aov_rad * 0.995);
    let max_rect_y = max_theta_y.tan();

    let half_w_norm = cx / norm_r;
    let max_theta_x = (half_w_norm * half_aov_rad).min(half_aov_rad * 0.995);
    let max_rect_x = max_theta_x.tan();

    if args.verbose {
        eprintln!(
            "Angular coverage after remap: H={:.1}°  V={:.1}°",
            max_theta_x.to_degrees() * 2.0,
            max_theta_y.to_degrees() * 2.0
        );
    }

    let interp_mode = args.interp.to_lowercase();
    let img_ref = &img;

    // Backward remap (parallel over rows)
    let pixels: Vec<(u32, u32, [u8; 3])> = (0..out_h)
        .into_par_iter()
        .flat_map(|oy| {
            let mut row = Vec::with_capacity(out_w as usize);

            for ox in 0..out_w {
                // Normalize destination coords to [-1, 1]
                let dx = (ox as f64 + 0.5) / out_w  as f64 * 2.0 - 1.0;
                let dy = (oy as f64 + 0.5) / out_h as f64 * 2.0 - 1.0;

                // Scale to rectilinear angular space
                let rect_x = dx * max_rect_x;
                let rect_y = dy * max_rect_y;

                let color = if let Some((fish_x, fish_y)) =
                    rectilinear_to_fisheye(rect_x, rect_y, half_aov_rad)
                {
                    // fish_x, fish_y are normalized to image-circle (r=1)
                    // Map back to source pixel coords (full frame, not just strip)
                    let src_x = cx + fish_x * norm_r;
                    let src_y = cy + fish_y * norm_r;

                    if interp_mode == "nearest" {
                        nearest(img_ref, src_x, src_y)
                    } else {
                        bilinear(img_ref, src_x, src_y)
                    }
                } else {
                    [0u8; 3]
                };

                row.push((ox, oy, color));
            }
            row
        })
        .collect();

    let mut out_img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::new(out_w, out_h);
    for (ox, oy, rgb) in pixels {
        out_img.put_pixel(ox, oy, Rgb(rgb));
    }

    let out_path = args.output.unwrap_or_else(|| {
        let stem = args.input.file_stem().unwrap_or_default().to_string_lossy();
        let ext = args.input.extension().unwrap_or_default().to_string_lossy();
        PathBuf::from(format!("{}_defish.{}", stem, ext))
    });

    out_img.save(&out_path).unwrap_or_else(|e| {
        eprintln!("Cannot save output: {e}");
        std::process::exit(1);
    });

    println!("Saved: {}", out_path.display());
    println!("Output size: {}x{} px", out_w, out_h);
}
