/// defish — Laowa 8-15mm fisheye to rectilinear strip
///
/// Handles two distinct modes automatically:
///
///  CIRCULAR fisheye (8mm): image circle doesn't fill the sensor.
///   The image circle is detected from the black border.
///   R_circle = detected image circle radius (NOT sensor half-diagonal).
///   At 8mm the circle edge = 90° half-AoV.
///
///  FULL-FRAME fisheye (10-15mm): image circle fills/exceeds the sensor.
///   R_circle = sensor half-diagonal (corners map to max angle).
///
/// In both cases: equidistant fisheye  r = (θ / θ_max) · R_circle
/// Output is wider than input to preserve center pixel density.

use clap::Parser;
use image::{DynamicImage, GenericImageView, ImageBuffer, Rgb};
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::fs::File;
use std::io::BufReader;

#[derive(Parser, Debug)]
#[command(
    name = "defish",
    about = "Defish Laowa 8-15mm fisheye photos into rectilinear panoramic strips",
    long_about = "\
Crops the vertical center strip of a fisheye photo and remaps it to \
rectilinear projection. Reads focal length and camera model from EXIF \
automatically. For circular fisheye images (8mm), auto-detects the image \
circle radius from the black border.\n\n\
Examples:\n  \
  defish photo.jpg                    # full auto, 30% strip\n  \
  defish photo.jpg -p 0.5             # 50% strip\n  \
  defish photo.jpg -p 0.5 -s 0.5     # 50% strip, half-res output\n  \
  defish photo.jpg -f 8 -c 0.79      # GFX 100S II at 8mm (manual override)"
)]
struct Args {
    /// Input image (JPEG, PNG, or TIFF)
    input: PathBuf,

    /// Output path (default: <input>_defish.<ext>)
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Fraction of frame height to use as center strip, e.g. 0.3 = middle 30%
    #[arg(short = 'p', long, default_value_t = 0.30)]
    strip_percent: f64,

    /// Override focal length in mm (normally read from EXIF)
    #[arg(short = 'f', long)]
    focal_length: Option<f64>,

    /// Override sensor crop factor vs full-frame (normally detected from EXIF).
    /// Fuji GFX = 0.79, Nikon Z8 / full-frame = 1.0
    #[arg(short = 'c', long)]
    crop_factor: Option<f64>,

    /// Override image circle radius in pixels (normally auto-detected).
    /// Only needed if auto-detection fails on unusual images.
    #[arg(short = 'r', long)]
    circle_radius: Option<f64>,

    /// Output scale multiplier (applied after auto width expansion).
    #[arg(short = 's', long, default_value_t = 1.0)]
    scale: f64,

    /// Print geometry details
    #[arg(short = 'v', long)]
    verbose: bool,
}

// ---------------------------------------------------------------------------
// EXIF
// ---------------------------------------------------------------------------

fn read_exif_focal_length(path: &Path) -> Option<f64> {
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

fn read_exif_model(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    let mut buf = BufReader::new(file);
    let exif = exif::Reader::new().read_from_container(&mut buf).ok()?;
    Some(exif.get_field(exif::Tag::Model, exif::In::PRIMARY)?
        .display_value().to_string())
}

fn detect_crop_factor(model: &str) -> f64 {
    let m = model.to_lowercase();
    if m.contains("gfx") { 0.790 }
    else if ["z 8","z8","z 9","z9","z6","z7","d850","d800","d810",
              "a7","a1","a9","r5","r3","r6"].iter().any(|s| m.contains(s)) { 1.0 }
    else {
        eprintln!("Note: unrecognised camera '{}'; assuming full-frame. Use -c to override.", model);
        1.0
    }
}

// ---------------------------------------------------------------------------
// Lens AoV table: Laowa 8-15mm f/2.8 (half diagonal AoV, FF-equivalent)
// ---------------------------------------------------------------------------

fn laowa_half_aov_deg(focal_mm_ff: f64) -> f64 {
    const TABLE: &[(f64, f64)] = &[
        (8.0, 90.0), (9.0, 77.0), (10.0, 65.0), (11.0, 57.5),
        (12.0, 53.0), (13.0, 48.0), (14.0, 43.5), (15.0, 40.0),
    ];
    let fl = focal_mm_ff.clamp(8.0, 15.0);
    for i in 0..TABLE.len() - 1 {
        let (f0, a0) = TABLE[i];
        let (f1, a1) = TABLE[i + 1];
        if fl >= f0 && fl <= f1 {
            return a0 + (fl - f0) / (f1 - f0) * (a1 - a0);
        }
    }
    TABLE.last().unwrap().1
}

// ---------------------------------------------------------------------------
// Image circle detection
//
// For circular fisheye (8mm), the image circle doesn't fill the sensor.
// We detect its radius by scanning outward from center along many radii
// and finding where pixel brightness drops into the black border.
//
// For full-frame fisheye (10-15mm), the image circle fills the sensor and
// this detection returns ~ the half-diagonal (correct behaviour).
// ---------------------------------------------------------------------------

fn detect_image_circle(img: &DynamicImage) -> f64 {
    let (w, h) = img.dimensions();
    let cx = w as f64 / 2.0;
    let cy = h as f64 / 2.0;
    let max_r = cx.min(cy) as u32;

    // Black threshold: pixel brightness below this = outside image circle
    const BLACK_THRESH: u8 = 20;
    // Number of radii to sample
    const N_ANGLES: usize = 72; // every 5 degrees

    let mut radii = Vec::with_capacity(N_ANGLES);

    for i in 0..N_ANGLES {
        let angle = std::f64::consts::TAU * i as f64 / N_ANGLES as f64;
        let cos_a = angle.cos();
        let sin_a = angle.sin();

        // Scan inward from max_r until we find a non-black pixel
        let mut found = false;
        for r in (1..=max_r).rev() {
            let px = (cx + r as f64 * cos_a).round() as i64;
            let py = (cy + r as f64 * sin_a).round() as i64;
            if px < 0 || py < 0 || px >= w as i64 || py >= h as i64 {
                continue;
            }
            let pixel = img.get_pixel(px as u32, py as u32).0;
            let brightness = pixel[0].max(pixel[1]).max(pixel[2]);
            if brightness > BLACK_THRESH {
                radii.push(r as f64);
                found = true;
                break;
            }
        }
        if !found {
            // This angle is all black — skip it (might happen at extreme diagonals)
        }
    }

    if radii.is_empty() {
        // Fallback: use half the shorter dimension
        return cx.min(cy);
    }

    // Use the median to be robust against glare, vignetting, or lens flare
    // at the circle edge
    radii.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = radii[radii.len() / 2];

    // Sanity check: if most radii agree with the median (circular image),
    // use the median. If they spread widely (full-frame, no circle), use
    // the half-diagonal.
    let half_diag = (cx * cx + cy * cy).sqrt();
    let spread = radii.last().unwrap() - radii.first().unwrap();

    if spread < 0.15 * median {
        // Tight cluster → circular image circle detected
        median
    } else {
        // Wide spread → full-frame fisheye, circle fills sensor
        half_diag
    }
}

// ---------------------------------------------------------------------------
// Pixel interpolation
// ---------------------------------------------------------------------------

fn bilinear(img: &DynamicImage, x: f64, y: f64) -> [u8; 3] {
    let (w, h) = img.dimensions();
    let x0 = x.floor() as i64;
    let y0 = y.floor() as i64;
    let clx = |v: i64| v.clamp(0, w as i64 - 1) as u32;
    let cly = |v: i64| v.clamp(0, h as i64 - 1) as u32;
    let p00 = img.get_pixel(clx(x0),     cly(y0)).0;
    let p10 = img.get_pixel(clx(x0 + 1), cly(y0)).0;
    let p01 = img.get_pixel(clx(x0),     cly(y0 + 1)).0;
    let p11 = img.get_pixel(clx(x0 + 1), cly(y0 + 1)).0;
    let tx = x - x0 as f64;
    let ty = y - y0 as f64;
    let l = |a: u8, b: u8, t: f64| -> u8 {
        (a as f64 + (b as f64 - a as f64) * t).round() as u8
    };
    [
        l(l(p00[0], p10[0], tx), l(p01[0], p11[0], tx), ty),
        l(l(p00[1], p10[1], tx), l(p01[1], p11[1], tx), ty),
        l(l(p00[2], p10[2], tx), l(p01[2], p11[2], tx), ty),
    ]
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    let args = Args::parse();

    let img = image::open(&args.input).unwrap_or_else(|e| {
        eprintln!("Cannot open '{}': {}", args.input.display(), e);
        std::process::exit(1);
    });
    let (src_w, src_h) = img.dimensions();

    // --- Lens / sensor parameters ---
    let focal_mm = args.focal_length
        .or_else(|| read_exif_focal_length(&args.input))
        .unwrap_or_else(|| {
            eprintln!("No EXIF focal length; assuming 15mm. Use -f to override.");
            15.0
        });

    let crop_factor = args.crop_factor.unwrap_or_else(|| {
        read_exif_model(&args.input)
            .map(|m| detect_crop_factor(&m))
            .unwrap_or_else(|| {
                eprintln!("No EXIF camera model; assuming full-frame. Use -c to override.");
                1.0
            })
    });

    let fl_ff     = focal_mm * crop_factor;
    let theta_max = laowa_half_aov_deg(fl_ff.clamp(8.0, 15.0)).to_radians();

    // --- Image circle radius ---
    // For circular fisheye (8mm): detected from black border.
    //   R_circle maps to theta_max at its edge.
    // For full-frame fisheye (12-15mm): R_circle = half-diagonal.
    let r_circle = args.circle_radius.unwrap_or_else(|| detect_image_circle(&img));

    let src_cx = src_w as f64 / 2.0;
    let src_cy = src_h as f64 / 2.0;
    let half_diag = (src_cx * src_cx + src_cy * src_cy).sqrt();
    let is_circular = r_circle < 0.9 * half_diag;

    // --- Strip geometry (computed early, needed for theta_h) ---
    let strip_frac = args.strip_percent.clamp(0.01, 1.0);
    let max_strip_h = if is_circular { (r_circle * 2.0) as u32 } else { src_h };
    let strip_h_px  = (((max_strip_h as f64) * strip_frac).round() as u32).max(1);
    let strip_y0    = (src_h - strip_h_px) / 2;
    let strip_half_h = strip_h_px as f64 / 2.0;

    // Horizontal half-width of the strip at the strip's vertical extent.
    // For circular: the strip is a horizontal band cutting through the circle.
    //   At vertical offset ±strip_half_h, the circle edge is at x = sqrt(R²-y²).
    //   Use the center row (y=0) width but cap so theta_h stays below ~75°
    //   to avoid tan() explosion near 90°.
    // For full-frame: strip spans the full sensor width.
    let strip_half_w = if is_circular {
        // Chord half-width at the strip's half-height
        let chord_hw = (r_circle * r_circle - strip_half_h * strip_half_h)
            .max(0.0).sqrt();
        // Also cap to keep theta_h ≤ 75° (tan(75°)=3.73, gives ~3x expansion max)
        let max_hw_for_75deg = r_circle * (75.0f64.to_radians() / theta_max).sin();
        chord_hw.min(max_hw_for_75deg)
    } else {
        src_cx
    };

    let theta_h = (strip_half_w / r_circle) * theta_max;  // equidistant: r = θ/θ_max * R

    // Center-preserving output half-width: cx_out = R/θ_max * tan(θ_h)
    let cx_out = r_circle / theta_max * theta_h.tan();
    let width_ratio = cx_out / strip_half_w;



    let out_w = ((cx_out * 2.0 * args.scale).round() as u32).max(1);
    let out_h = ((strip_h_px as f64 * width_ratio * args.scale).round() as u32).max(1);

    if args.verbose {
        eprintln!("Camera:      focal {:.1}mm  crop {:.3}×  → FF-equiv {:.1}mm",
            focal_mm, crop_factor, fl_ff);
        eprintln!("Lens:        half-AoV {:.1}°  θ_h {:.1}°",
            theta_max.to_degrees(), theta_h.to_degrees());
        eprintln!("Circle:      R={:.0}px  half-diag={:.0}px  {}",
            r_circle, half_diag,
            if is_circular { "→ CIRCULAR fisheye (black border detected)" }
            else { "→ full-frame fisheye (fills sensor)" });
        eprintln!("Width:       strip_half_w={:.0}px → cx_out={:.0}px  ({:+.1}%)",
            strip_half_w, cx_out, (width_ratio - 1.0) * 100.0);
        eprintln!("Strip:       {:.0}% of circle  y=[{}, {}]  h={}px",
            strip_frac * 100.0, strip_y0, strip_y0 + strip_h_px, strip_h_px);
        eprintln!("Output:      {}×{}px", out_w, out_h);
    }

    // --- Backward remap ---
    let img_ref = &img;
    let tan_theta_h = theta_h.tan();

    let pixels: Vec<(u32, u32, [u8; 3])> = (0..out_h)
        .into_par_iter()
        .flat_map(|oy| {
            let mut row = Vec::with_capacity(out_w as usize);

            let src_y_frac = (oy as f64 + 0.5) / out_h as f64;
            let src_y_px   = strip_y0 as f64 + src_y_frac * strip_h_px as f64;

            for ox in 0..out_w {
                // ox_norm: -1=left edge of strip, 0=center, +1=right edge
                let ox_norm    = (ox as f64 + 0.5 - cx_out) / cx_out;
                let theta_rect = (ox_norm * tan_theta_h).atan();
                // Equidistant backward: source x offset from image center
                let r_fish     = theta_rect / theta_max * r_circle;
                let src_x      = src_cx + r_fish;

                let rgb = bilinear(img_ref, src_x, src_y_px);
                row.push((ox, oy, rgb));
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
        let ext  = args.input.extension().unwrap_or_default().to_string_lossy();
        PathBuf::from(format!("{}_defish.{}", stem, ext))
    });

    out_img.save(&out_path).unwrap_or_else(|e| {
        eprintln!("Cannot save '{}': {}", out_path.display(), e);
        std::process::exit(1);
    });

    println!("Saved: {}  ({}×{})", out_path.display(), out_w, out_h);
}
// placeholder
