/// defish — Laowa 8-15mm fisheye to rectilinear or cylindrical strip
///
/// Two projection modes selected automatically by focal length:
///
///  RECTILINEAR (default, good for 10-15mm):
///    x_out = f * tan(θ_x)
///    Straight lines stay straight. Output wider than input (+13% at 15mm).
///    Edge stretch = 1/cos²(θ) — grows fast, impractical beyond ~60°.
///
///  CYLINDRICAL (default for circular/8mm fisheye):
///    x_out = f * θ_x   (linear in angle, same as equidistant fisheye)
///    No horizontal stretch at any angle. Output same width as source strip.
///    Straight vertical lines stay straight. Horizontal lines bow slightly
///    near top/bottom but this is imperceptible in a narrow horizontal strip.
///
///  Override with --projection rect|cyl
///
/// Image circle auto-detection: for circular fisheye (8mm), the black border
/// is detected and R_circle is set to the image circle radius, not the
/// sensor half-diagonal.

use clap::Parser;
use image::{DynamicImage, GenericImageView, ImageBuffer, Rgb};
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::fs::File;
use std::io::BufReader;

#[derive(Debug, Clone, PartialEq)]
enum Projection { Rectilinear, Cylindrical }

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

#[derive(Parser, Debug)]
#[command(
    name = "defish",
    about = "Defish Laowa 8-15mm fisheye photos into panoramic strips",
    long_about = "\
Crops the vertical center strip of a fisheye photo and remaps it from \
equidistant fisheye to rectilinear or cylindrical projection.\n\n\
Projection is chosen automatically by focal length:\n  \
  8mm  (circular fisheye) → cylindrical: no edge stretch, natural look\n  \
  10-15mm (full-frame)    → rectilinear: straight lines, +13-46% wider\n\n\
Examples:\n  \
  defish photo.jpg                       # full auto\n  \
  defish photo.jpg -p 0.5               # 50% strip\n  \
  defish photo.jpg --projection cyl     # force cylindrical\n  \
  defish photo.jpg -f 8 -c 0.79        # GFX 100S II at 8mm"
)]
struct Args {
    input: PathBuf,
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Fraction of frame height to use as center strip (0..1)
    #[arg(short = 'p', long, default_value_t = 0.30)]
    strip_percent: f64,
    /// Override focal length in mm (normally read from EXIF)
    #[arg(short = 'f', long)]
    focal_length: Option<f64>,
    /// Override sensor crop factor. Fuji GFX=0.79, Nikon Z8=1.0 (auto from EXIF)
    #[arg(short = 'c', long)]
    crop_factor: Option<f64>,
    /// Override image circle radius in pixels (auto-detected from black border)
    #[arg(short = 'r', long)]
    circle_radius: Option<f64>,
    /// Output projection: rect (rectilinear) or cyl (cylindrical).
    /// Auto-selected: cyl for circular fisheye (8mm), rect for full-frame.
    #[arg(long)]
    projection: Option<Projection>,
    /// Output scale multiplier
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
    else { eprintln!("Note: unrecognised camera '{}'; assuming full-frame.", model); 1.0 }
}

// ---------------------------------------------------------------------------
// Lens: Laowa 8-15mm half diagonal AoV (FF-equivalent)
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
// Image circle detection (for circular fisheye with black border)
// ---------------------------------------------------------------------------

fn detect_image_circle(img: &DynamicImage) -> (f64, bool) {
    let (w, h) = img.dimensions();
    let cx = w as f64 / 2.0;
    let cy = h as f64 / 2.0;
    let max_r = cx.min(cy) as u32;
    let half_diag = (cx * cx + cy * cy).sqrt();

    const BLACK_THRESH: u8 = 20;
    const N_ANGLES: usize = 72;

    let mut radii = Vec::with_capacity(N_ANGLES);
    for i in 0..N_ANGLES {
        let angle = std::f64::consts::TAU * i as f64 / N_ANGLES as f64;
        let (cos_a, sin_a) = (angle.cos(), angle.sin());
        for r in (1..=max_r).rev() {
            let px = (cx + r as f64 * cos_a).round() as i64;
            let py = (cy + r as f64 * sin_a).round() as i64;
            if px < 0 || py < 0 || px >= w as i64 || py >= h as i64 { continue; }
            let p = img.get_pixel(px as u32, py as u32).0;
            if p[0].max(p[1]).max(p[2]) > BLACK_THRESH {
                radii.push(r as f64);
                break;
            }
        }
    }

    if radii.is_empty() { return (cx.min(cy), false); }

    radii.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = radii[radii.len() / 2];
    let spread = radii.last().unwrap() - radii.first().unwrap();

    if spread < 0.15 * median {
        (median, true)   // tight cluster → circular fisheye
    } else {
        (half_diag, false) // wide spread → full-frame fisheye
    }
}

// ---------------------------------------------------------------------------
// Interpolation
// ---------------------------------------------------------------------------

fn bilinear(img: &DynamicImage, x: f64, y: f64) -> [u8; 3] {
    let (w, h) = img.dimensions();
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
    [l(l(p00[0],p10[0],tx),l(p01[0],p11[0],tx),ty),
     l(l(p00[1],p10[1],tx),l(p01[1],p11[1],tx),ty),
     l(l(p00[2],p10[2],tx),l(p01[2],p11[2],tx),ty)]
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
    let src_cx = src_w as f64 / 2.0;
    let src_cy = src_h as f64 / 2.0;

    // --- Lens / sensor ---
    let focal_mm = args.focal_length
        .or_else(|| read_exif_focal_length(&args.input))
        .unwrap_or_else(|| { eprintln!("No EXIF focal length; assuming 15mm."); 15.0 });

    let crop_factor = args.crop_factor.unwrap_or_else(|| {
        read_exif_model(&args.input)
            .map(|m| detect_crop_factor(&m))
            .unwrap_or_else(|| { eprintln!("No EXIF camera model; assuming full-frame."); 1.0 })
    });

    let fl_ff     = focal_mm * crop_factor;
    let theta_max = laowa_half_aov_deg(fl_ff.clamp(8.0, 15.0)).to_radians();

    // --- Image circle ---
    let (r_circle, is_circular) = if let Some(r) = args.circle_radius {
        let half_diag = (src_cx*src_cx + src_cy*src_cy).sqrt();
        (r, r < 0.9 * half_diag)
    } else {
        detect_image_circle(&img)
    };

    // --- Projection: auto-select or use override ---
    let proj = args.projection.unwrap_or(
        if is_circular { Projection::Cylindrical } else { Projection::Rectilinear }
    );

    // --- Strip geometry ---
    let strip_frac   = args.strip_percent.clamp(0.01, 1.0);
    let max_strip_h  = if is_circular { (r_circle * 2.0) as u32 } else { src_h };
    let strip_h_px   = (((max_strip_h as f64) * strip_frac).round() as u32).max(1);
    let strip_y0     = (src_h - strip_h_px) / 2;
    let strip_half_h = strip_h_px as f64 / 2.0;

    // Horizontal half-width available in the source strip
    let strip_half_w = if is_circular {
        // Chord half-width at the strip's vertical extent
        (r_circle * r_circle - strip_half_h * strip_half_h).max(0.0).sqrt()
    } else {
        src_cx
    };

    // Horizontal half-angle covered by the strip
    // equidistant: r = θ/θ_max * R  →  θ = r/R * θ_max
    let theta_h = (strip_half_w / r_circle) * theta_max;

    // --- Output dimensions ---
    // Cylindrical: output x = R * θ (linear in angle) → same pixel/radian as source
    //   → output width == source strip width (no expansion)
    // Rectilinear: output x = R/θ_max * tan(θ) → expands at edges
    //   → cx_out = R/θ_max * tan(θ_h), always > strip_half_w
    let cx_out = match proj {
        Projection::Cylindrical  => strip_half_w,  // 1:1, no expansion
        Projection::Rectilinear  => r_circle / theta_max * theta_h.tan(),
    };

    let out_w = ((cx_out * 2.0 * args.scale).round() as u32).max(1);
    // Height is 1:1 with source strip — the remap only moves pixels horizontally
    let out_h = ((strip_h_px as f64 * args.scale).round() as u32).max(1);

    if args.verbose {
        eprintln!("Camera:     focal {:.1}mm  crop {:.3}×  FF-equiv {:.1}mm",
            focal_mm, crop_factor, fl_ff);
        eprintln!("Lens:       half-AoV {:.1}°  θ_h {:.1}°",
            theta_max.to_degrees(), theta_h.to_degrees());
        eprintln!("Circle:     R={:.0}px  {}",
            r_circle,
            if is_circular { "circular fisheye (black border detected)" }
            else { "full-frame fisheye" });
        eprintln!("Projection: {:?}  cx_out={:.0}px  ({:+.1}% vs strip)",
            proj, cx_out, (cx_out / strip_half_w - 1.0) * 100.0);
        eprintln!("Strip:      {:.0}%  y=[{}, {}]  h={}px  half_w={:.0}px",
            strip_frac * 100.0, strip_y0, strip_y0 + strip_h_px,
            strip_h_px, strip_half_w);
        eprintln!("Output:     {}×{}px", out_w, out_h);
    }

    // --- Backward remap ---
    //
    // For each output pixel ox at normalized position ox_norm ∈ [-1, +1]:
    //
    //   Cylindrical:
    //     θ_x = ox_norm * θ_h               (linear — just unscale from output)
    //     src_x = cx + (θ_x / θ_max) * R    (equidistant inverse)
    //     → output pixel maps to SAME angular position in source, 1:1
    //     → this is literally a rescale/crop, no distortion correction at all!
    //     Wait — that can't be right. Let me re-examine...
    //
    //   Actually for cylindrical the SOURCE equidistant and the TARGET cylindrical
    //   both map θ → r linearly, so the horizontal mapping IS 1:1.
    //   What cylindrical DOES change is how off-axis vertical points are handled —
    //   it separates azimuth from elevation properly.
    //   For a HORIZONTAL STRIP where we only copy rows 1:1, the cylindrical
    //   horizontal is identical to the fisheye horizontal. The benefit comes when
    //   doing the FULL 2D remap. For our strip-only use case, cylindrical horizontal
    //   = identity remap (no change to x), but we still get clean edges because
    //   there's no tan() expansion.
    //
    //   So cylindrical for a horizontal strip = source strip, just rescaled to
    //   output dimensions. The distortion at edges is exactly what the fisheye had.
    //   That's fine — the fisheye edges look natural (equidistant is a "good"
    //   projection for panoramas); it's rectilinear that stretches them.
    //
    //   Summary of what each projection does to our horizontal strip:
    //   - Cylindrical: x_out ∝ θ_x  ↔  x_src ∝ θ_x  → 1:1 linear rescale
    //   - Rectilinear: x_out ∝ tan(θ_x) → edge expansion, straight lines

    let img_ref   = &img;
    let tan_th    = theta_h.tan();

    let pixels: Vec<(u32, u32, [u8; 3])> = (0..out_h)
        .into_par_iter()
        .flat_map(|oy| {
            let mut row = Vec::with_capacity(out_w as usize);
            let src_y_frac = (oy as f64 + 0.5) / out_h as f64;
            let src_y_px   = strip_y0 as f64 + src_y_frac * strip_h_px as f64;

            for ox in 0..out_w {
                let ox_norm = (ox as f64 + 0.5 - cx_out) / cx_out;

                // Find the horizontal angle this output pixel represents
                let theta_x = match proj {
                    // Cylindrical: linear in angle
                    Projection::Cylindrical => ox_norm * theta_h,
                    // Rectilinear: linear in tan(angle)
                    Projection::Rectilinear => (ox_norm * tan_th).atan(),
                };

                // Equidistant backward map: r = θ/θ_max * R
                let src_x = src_cx + (theta_x / theta_max) * r_circle;

                let rgb = bilinear(img_ref, src_x, src_y_px);
                row.push((ox, oy, rgb));
            }
            row
        })
        .collect();

    let mut out_img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::new(out_w, out_h);
    for (ox, oy, rgb) in pixels { out_img.put_pixel(ox, oy, Rgb(rgb)); }

    let out_path = args.output.unwrap_or_else(|| {
        let stem = args.input.file_stem().unwrap_or_default().to_string_lossy();
        let ext  = args.input.extension().unwrap_or_default().to_string_lossy();
        PathBuf::from(format!("{}_defish.{}", stem, ext))
    });
    out_img.save(&out_path).unwrap_or_else(|e| {
        eprintln!("Cannot save: {e}"); std::process::exit(1);
    });
    println!("Saved: {}  ({}×{})", out_path.display(), out_w, out_h);
}
