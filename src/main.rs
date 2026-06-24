/// defish — Laowa 8-15mm fisheye to rectilinear strip
///
/// Takes a fisheye photo, crops the vertical center strip, and remaps it
/// from equidistant fisheye projection to rectilinear (perspective) projection.
///
/// The output is wider than the input because rectilinear projection
/// allocates more pixels to the center than the edges — so to keep the
/// center at the same scale as the source, the edges must expand outward.
/// Height scales proportionally so pixels remain square.
///
/// Lens model: equidistant fisheye  r = (θ / θ_max) · R_circle
/// Focal length and camera crop factor are read from EXIF automatically.

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
automatically. Output is wider than input to preserve center scale.\n\n\
Examples:\n  \
  defish photo.jpg                          # full auto, 30% strip\n  \
  defish photo.jpg -p 0.5                   # 50% strip\n  \
  defish photo.jpg -p 0.5 -s 0.5           # 50% strip, half resolution output\n  \
  defish photo.jpg -f 8 -c 0.79            # manual overrides (GFX 100S II at 8mm)"
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

    /// Override sensor crop factor vs full-frame (normally detected from EXIF camera model).
    /// Fuji GFX = 0.79, Nikon Z8 / full-frame = 1.0
    #[arg(short = 'c', long)]
    crop_factor: Option<f64>,

    /// Output scale multiplier, applied after the automatic width expansion.
    /// Use 0.5 to halve resolution, 2.0 to double, etc.
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
    if m.contains("gfx") {
        // Fuji GFX medium format: 43.8×32.9mm sensor, diagonal 54.8mm
        // vs full-frame diagonal 43.3mm → crop factor 0.790
        0.790
    } else if ["z 8","z8","z 9","z9","z6","z7","d850","d800","d810",
                "a7","a1","a9","r5","r3","r6"]
               .iter().any(|s| m.contains(s)) {
        1.0
    } else {
        eprintln!("Note: unrecognised camera model '{}'; assuming full-frame. \
                   Use -c to override.", model);
        1.0
    }
}

// ---------------------------------------------------------------------------
// Lens: Laowa 8-15mm f/2.8 equidistant fisheye
// Half diagonal AoV table (full-frame equivalent focal lengths)
// ---------------------------------------------------------------------------

fn laowa_half_aov_deg(focal_mm_ff_equiv: f64) -> f64 {
    // Source: Laowa published specs + community measurements
    const TABLE: &[(f64, f64)] = &[
        (8.0,  90.0),
        (9.0,  77.0),
        (10.0, 65.0),
        (11.0, 57.5),
        (12.0, 53.0),
        (13.0, 48.0),
        (14.0, 43.5),
        (15.0, 40.0),
    ];
    let fl = focal_mm_ff_equiv.clamp(8.0, 15.0);
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
// Pixel interpolation
// ---------------------------------------------------------------------------

fn bilinear(img: &DynamicImage, x: f64, y: f64) -> [u8; 3] {
    let (w, h) = img.dimensions();
    let x0 = x.floor() as i64;
    let y0 = y.floor() as i64;
    let cx = |v: i64| v.clamp(0, w as i64 - 1) as u32;
    let cy = |v: i64| v.clamp(0, h as i64 - 1) as u32;
    let p00 = img.get_pixel(cx(x0),     cy(y0)).0;
    let p10 = img.get_pixel(cx(x0 + 1), cy(y0)).0;
    let p01 = img.get_pixel(cx(x0),     cy(y0 + 1)).0;
    let p11 = img.get_pixel(cx(x0 + 1), cy(y0 + 1)).0;
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
            eprintln!("No EXIF focal length found; assuming 15mm. Use -f to override.");
            15.0
        });

    let crop_factor = args.crop_factor.unwrap_or_else(|| {
        read_exif_model(&args.input)
            .map(|m| detect_crop_factor(&m))
            .unwrap_or_else(|| {
                eprintln!("No EXIF camera model found; assuming full-frame. Use -c to override.");
                1.0
            })
    });

    // Full-frame-equivalent focal length for AoV lookup
    let fl_ff     = focal_mm * crop_factor;
    let theta_max = laowa_half_aov_deg(fl_ff.clamp(8.0, 15.0)).to_radians();

    // --- Sensor geometry ---
    let src_cx   = src_w as f64 / 2.0;
    let src_cy   = src_h as f64 / 2.0;
    let r_circle = (src_cx * src_cx + src_cy * src_cy).sqrt(); // half-diagonal

    // Horizontal half-angle covered by the frame (equidistant: r = θ/θ_max · R)
    let theta_h = (src_cx / r_circle) * theta_max;

    // Center-preserving output half-width:
    //   In rectilinear, dx/dθ at center = f_rect.
    //   In fisheye,     dx/dθ at center = R / θ_max.
    //   For equal pixel density at center: f_rect = R / θ_max.
    //   Output x at angle θ: x_out = (R / θ_max) · tan(θ)
    //   At frame edge (θ = θ_h): cx_out = R / θ_max · tan(θ_h)
    let cx_out   = r_circle / theta_max * theta_h.tan();
    let out_w    = ((cx_out * 2.0 * args.scale).round() as u32).max(1);

    // --- Strip geometry ---
    let strip_frac = args.strip_percent.clamp(0.01, 1.0);
    let strip_h_px = ((src_h as f64 * strip_frac).round() as u32).max(1);
    let strip_y0   = (src_h - strip_h_px) / 2;

    // Output height: scale strip height by same ratio as width expansion,
    // keeping pixels square (isotropic sampling density)
    let width_ratio = cx_out / src_cx;
    let out_h = ((strip_h_px as f64 * width_ratio * args.scale).round() as u32).max(1);

    if args.verbose {
        eprintln!("Camera:      focal {:.1}mm  crop {:.3}×  → FF-equiv {:.1}mm",
            focal_mm, crop_factor, fl_ff);
        eprintln!("Lens:        half-AoV {:.1}°  (θ_h horiz {:.1}°)",
            theta_max.to_degrees(), theta_h.to_degrees());
        eprintln!("Geometry:    R_circle {:.0}px  cx_out {:.0}px",
            r_circle, cx_out);
        eprintln!("Width:       {} → {}px  ({:+.1}%)",
            src_w, out_w, (width_ratio - 1.0) * 100.0);
        eprintln!("Strip:       {:.0}% of frame  y=[{}, {}]  h={}px",
            strip_frac * 100.0, strip_y0, strip_y0 + strip_h_px, strip_h_px);
        eprintln!("Output:      {}×{}px", out_w, out_h);
    }

    // --- Backward remap ---
    //
    // For each output pixel at (ox, oy):
    //   ox_norm = (ox - cx_out) / cx_out  ∈ [-1, +1]
    //
    //   Rectilinear angle: θ_rect = atan(ox_norm · tan(θ_h))
    //   Fisheye source x:  r = θ_rect / θ_max · R_circle
    //                       src_x = cx + r          [r is signed]
    //
    //   Vertical: direct linear mapping within the strip (no vertical warp)

    let img_ref = &img;

    let pixels: Vec<(u32, u32, [u8; 3])> = (0..out_h)
        .into_par_iter()
        .flat_map(|oy| {
            let mut row = Vec::with_capacity(out_w as usize);

            let src_y_frac = (oy as f64 + 0.5) / out_h as f64;
            let src_y_px   = strip_y0 as f64 + src_y_frac * strip_h_px as f64;

            let tan_theta_h = theta_h.tan();

            for ox in 0..out_w {
                let ox_norm    = (ox as f64 + 0.5 - cx_out) / cx_out;
                let theta_rect = (ox_norm * tan_theta_h).atan();
                let r_fish     = theta_rect / theta_max * r_circle;
                let src_x      = src_cx + r_fish;

                let rgb = bilinear(img_ref, src_x, src_y_px);
                row.push((ox, oy, rgb));
            }
            row
        })
        .collect();

    // --- Assemble output ---
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
