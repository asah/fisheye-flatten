/// defish — Laowa 8-15mm: center-preserving graduated horizontal defish
///
/// Correct fisheye barrel distortion model:
///
///   The fisheye maps scene angle θ → sensor radius r = (θ/θ_max) · R_circle
///   A rectilinear lens maps θ → r = f · tan(θ)  (tan stretches edges more)
///
///   Barrel distortion means the fisheye packs edge content too CLOSE to the
///   center (linear in θ, not tan). To correct, for each output pixel at
///   position x_out, we find the angle it represents in rectilinear space,
///   then look up where that angle falls in the fisheye source.
///
///   Output covers the SAME horizontal FoV as the source (no content added,
///   no extrapolation outside the frame). The correction pulls edge source
///   pixels INWARD (closer to center in source) and places them at the output
///   edge position — effectively stretching the edges, which is the inverse
///   of barrel compression.
///
/// Graduated blend:
///   At center (x=0):   weight=0 → pure identity (no change)
///   At edge  (x=±1):   weight=edge_strength → full fisheye correction
///   Between:           weight = edge_strength · |x_norm|^blend_power

use clap::Parser;
use image::{DynamicImage, GenericImageView, ImageBuffer, Rgb};
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::fs::File;
use std::io::BufReader;

#[derive(Parser, Debug)]
#[command(
    name = "defish",
    about = "Crop & defish Laowa 8-15mm fisheye — center-preserving graduated remap"
)]
struct Args {
    input: PathBuf,
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Fraction of frame height to keep as center strip (0..1)
    #[arg(short = 'p', long, default_value_t = 0.30)]
    strip_percent: f64,
    /// Focal length in mm (auto from EXIF)
    #[arg(short = 'f', long)]
    focal_length: Option<f64>,
    /// Sensor crop factor vs full-frame (auto from EXIF model). GFX=0.79, Z8=1.0
    #[arg(short = 'c', long)]
    crop_factor: Option<f64>,
    /// How fast edge correction ramps in. 2.0=smooth, 3.0=very center-preserving, 1.0=linear
    #[arg(short = 'b', long, default_value_t = 2.0)]
    blend_power: f64,
    /// Max correction strength at far edge (0=none, 1=full rectilinear)
    #[arg(short = 'e', long, default_value_t = 1.0)]
    edge_strength: f64,
    #[arg(short = 's', long, default_value_t = 1.0)]
    scale: f64,
    #[arg(short = 'i', long, default_value = "bilinear")]
    interp: String,
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
        exif::Value::Rational(v) if !v.is_empty() => Some(v[0].num as f64 / v[0].denom as f64),
        _ => None,
    }
}

fn read_exif_model(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    let mut buf = BufReader::new(file);
    let exif = exif::Reader::new().read_from_container(&mut buf).ok()?;
    Some(exif.get_field(exif::Tag::Model, exif::In::PRIMARY)?.display_value().to_string())
}

fn detect_crop_factor(model: &str) -> f64 {
    let m = model.to_lowercase();
    if m.contains("gfx") { 0.790 }
    else if ["z 8","z8","z 9","z9","z6","z7","d850","d800","d810",
             "a7","a1","a9","r5","r3","r6"].iter().any(|s| m.contains(s)) { 1.0 }
    else { eprintln!("Unknown model '{}', assuming FF crop=1.0", model); 1.0 }
}

// ---------------------------------------------------------------------------
// Lens: Laowa 8-15mm equidistant half-diagonal AoV table
// ---------------------------------------------------------------------------

fn laowa_8_15_half_aov_deg(focal_mm: f64) -> f64 {
    let table: &[(f64, f64)] = &[
        (8.0,90.0),(9.0,77.0),(10.0,65.0),(11.0,57.5),
        (12.0,53.0),(13.0,48.0),(14.0,43.5),(15.0,40.0),
    ];
    let fl = focal_mm.clamp(8.0, 15.0);
    for i in 0..table.len()-1 {
        let (f0,a0) = table[i]; let (f1,a1) = table[i+1];
        if fl >= f0 && fl <= f1 { return a0 + (fl-f0)/(f1-f0)*(a1-a0); }
    }
    table.last().unwrap().1
}

// ---------------------------------------------------------------------------
// Core remap
//
// Geometry (all working on the horizontal axis; rows passed through directly):
//
//   R_circle = half-diagonal of sensor in pixels (image circle radius)
//   theta_max = half diagonal AoV of the lens (radians)
//
//   For a pixel at x_out (pixels from left), normalized ox = (x_out - cx) / cx ∈ [-1,1]:
//
//   Horizontal angle at left/right frame edge in the SOURCE:
//     theta_h = (cx / R_circle) * theta_max   [equidistant: r = theta/theta_max * R_circle]
//
//   For IDENTITY (no correction): src_x = x_out  (identity)
//
//   For FULL CORRECTION (rectilinear):
//     The output pixel at ox represents rectilinear angle:
//       theta_rect = atan(ox * tan(theta_h))
//     Map back through equidistant to find source:
//       r_fish = (theta_rect / theta_max) * R_circle
//       src_x = cx + sign(ox) * r_fish
//
//   GRADUATED BLEND:
//     weight = edge_strength * |ox|^blend_power
//     final_src_x = lerp(identity_src_x, corrected_src_x, weight)
//                 = x_out + weight * (corrected_src_x - x_out)
// ---------------------------------------------------------------------------

fn bilinear(img: &DynamicImage, x: f64, y: f64) -> [u8; 3] {
    let (w, h) = img.dimensions();
    let x0 = x.floor() as i64; let y0 = y.floor() as i64;
    let cx = |v: i64| v.clamp(0, w as i64-1) as u32;
    let cy = |v: i64| v.clamp(0, h as i64-1) as u32;
    let p00 = img.get_pixel(cx(x0),   cy(y0)).0;
    let p10 = img.get_pixel(cx(x0+1), cy(y0)).0;
    let p01 = img.get_pixel(cx(x0),   cy(y0+1)).0;
    let p11 = img.get_pixel(cx(x0+1), cy(y0+1)).0;
    let tx = x - x0 as f64; let ty = y - y0 as f64;
    let l = |a:u8,b:u8,t:f64|->u8{(a as f64+(b as f64-a as f64)*t).round() as u8};
    [l(l(p00[0],p10[0],tx),l(p01[0],p11[0],tx),ty),
     l(l(p00[1],p10[1],tx),l(p01[1],p11[1],tx),ty),
     l(l(p00[2],p10[2],tx),l(p01[2],p11[2],tx),ty)]
}

fn nearest(img: &DynamicImage, x: f64, y: f64) -> [u8; 3] {
    let (w, h) = img.dimensions();
    let xi = (x.round() as i64).clamp(0, w as i64-1) as u32;
    let yi = (y.round() as i64).clamp(0, h as i64-1) as u32;
    let p = img.get_pixel(xi, yi).0; [p[0],p[1],p[2]]
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    let args = Args::parse();

    let img = image::open(&args.input).unwrap_or_else(|e| {
        eprintln!("Cannot open: {e}"); std::process::exit(1);
    });
    let (src_w, src_h) = img.dimensions();

    let focal_mm = args.focal_length
        .or_else(|| read_exif_focal_length(&args.input))
        .unwrap_or_else(|| { eprintln!("No EXIF focal length; assuming 15mm."); 15.0 });

    let crop_factor = args.crop_factor.unwrap_or_else(|| {
        read_exif_model(&args.input).map(|m| detect_crop_factor(&m)).unwrap_or(1.0)
    });

    let fl_ff = focal_mm * crop_factor;
    let theta_max = laowa_8_15_half_aov_deg(fl_ff.clamp(8.0, 15.0)).to_radians();

    // Geometry constants
    let src_cx = src_w as f64 / 2.0;
    let src_cy = src_h as f64 / 2.0;
    let r_circle = (src_cx * src_cx + src_cy * src_cy).sqrt(); // half-diagonal

    // Horizontal angle at the left/right frame edge (equidistant: r = theta/theta_max * R)
    let theta_h = (src_cx / r_circle) * theta_max;
    let tan_theta_h = theta_h.tan();

    // Strip
    let strip_frac = args.strip_percent.clamp(0.01, 1.0);
    let strip_h_px = ((src_h as f64 * strip_frac).round() as u32).max(1);
    let strip_y0 = (src_h - strip_h_px) / 2;

    let out_w = ((src_w as f64 * args.scale).round() as u32).max(1);
    let out_h = ((strip_h_px as f64 * args.scale).round() as u32).max(1);

    if args.verbose {
        eprintln!("Input: {}x{}  focal: {:.1}mm  crop: {:.3}  FF-equiv: {:.1}mm",
            src_w, src_h, focal_mm, crop_factor, fl_ff);
        eprintln!("theta_max (diag): {:.2}°  theta_h (horiz edge): {:.2}°",
            theta_max.to_degrees(), theta_h.to_degrees());
        eprintln!("R_circle: {:.0}px  tan_theta_h: {:.4}", r_circle, tan_theta_h);
        eprintln!("Strip: y=[{}, {}] h={}px  output: {}x{}",
            strip_y0, strip_y0+strip_h_px, strip_h_px, out_w, out_h);
        eprintln!("blend_power: {}  edge_strength: {}", args.blend_power, args.edge_strength);

        // Show sample displacements
        eprintln!("Sample displacements (corrected - identity):");
        for frac in [0.25f64, 0.5, 0.75, 1.0] {
            let ox = frac;  // normalized, right half
            let theta_rect = (ox * tan_theta_h).atan();
            let r_fish = (theta_rect / theta_max) * r_circle;
            let corr_x = src_cx + r_fish;
            let id_x   = src_cx + ox * src_cx;
            eprintln!("  x={:.0}%: id_src={:.0}px  corr_src={:.0}px  delta={:+.0}px",
                frac*100.0, id_x, corr_x, corr_x - id_x);
        }
    }

    let interp_mode = args.interp.to_lowercase();
    let img_ref = &img;

    let pixels: Vec<(u32, u32, [u8; 3])> = (0..out_h)
        .into_par_iter()
        .flat_map(|oy| {
            let mut row = Vec::with_capacity(out_w as usize);

            // Map output row → source row (simple scale within strip, no vertical warp)
            let src_y_frac = (oy as f64 + 0.5) / out_h as f64;
            let src_y_px   = strip_y0 as f64 + src_y_frac * strip_h_px as f64;

            for ox_px in 0..out_w {
                // Normalized horizontal position: -1=left edge, 0=center, +1=right edge
                let ox_norm = (ox_px as f64 + 0.5) / out_w as f64 * 2.0 - 1.0;

                // Identity source x: same relative position in source
                let id_src_x = src_cx + ox_norm * src_cx;

                // Full rectilinear correction:
                //   This output pixel represents rectilinear angle atan(ox_norm * tan_theta_h)
                //   Map back through equidistant: r_fish = theta_rect/theta_max * R_circle
                let theta_rect = (ox_norm * tan_theta_h).atan();
                let r_fish     = (theta_rect / theta_max) * r_circle;
                let corr_src_x = src_cx + r_fish; // r_fish is already signed via theta_rect

                // Graduated blend: 0 at center → edge_strength at edges
                let weight     = args.edge_strength * ox_norm.abs().powf(args.blend_power);
                let final_src_x = id_src_x + weight * (corr_src_x - id_src_x);

                let color = if interp_mode == "nearest" {
                    nearest(img_ref, final_src_x, src_y_px)
                } else {
                    bilinear(img_ref, final_src_x, src_y_px)
                };
                row.push((ox_px, oy, color));
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
    println!("Saved: {}  ({}x{})", out_path.display(), out_w, out_h);
}
