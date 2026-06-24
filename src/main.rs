/// defish — Laowa 8-15mm: center-preserving horizontal defish
///
/// Key insight: the fisheye distortion we want to correct is HORIZONTAL barrel
/// distortion. Vertically within the strip, content is already approximately
/// correct (the strip is narrow enough that vertical fisheye error is small).
///
/// Remap strategy:
///   For each output pixel at (ox, oy):
///   1. Map oy → same row in source (no vertical warp within strip)
///   2. For ox: blend between identity and fisheye correction based on |ox_norm|
///      - weight = edge_strength * |ox_norm|^blend_power
///      - identity: src_x = ox (same horizontal position, normalized)
///      - corrected: src_x from full equidistant→rectilinear inverse
///      - final: lerp(identity, corrected, weight)
///
/// This means the center column is untouched pixel-for-pixel, and correction
/// ramps in only as you move toward the left/right edges.

use clap::Parser;
use image::{DynamicImage, GenericImageView, ImageBuffer, Rgb};
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::fs::File;
use std::io::BufReader;

#[derive(Parser, Debug)]
#[command(
    name = "defish",
    about = "Crop & defish Laowa 8-15mm fisheye — center-preserving graduated horizontal remap"
)]
struct Args {
    input: PathBuf,
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Fraction of frame height to crop as center strip (0..1)
    #[arg(short = 'p', long, default_value_t = 0.30)]
    strip_percent: f64,
    /// Focal length in mm (auto from EXIF)
    #[arg(short = 'f', long)]
    focal_length: Option<f64>,
    /// Sensor crop factor vs full-frame (auto from EXIF model)
    #[arg(short = 'c', long)]
    crop_factor: Option<f64>,
    /// Blend power: how fast edge correction ramps in. 2.0=smooth, 3.0=very center-preserving
    #[arg(short = 'b', long, default_value_t = 2.0)]
    blend_power: f64,
    /// Max correction strength at the far edge (0=none, 1=full rectilinear)
    #[arg(short = 'e', long, default_value_t = 1.0)]
    edge_strength: f64,
    #[arg(short = 's', long, default_value_t = 1.0)]
    scale: f64,
    #[arg(short = 'i', long, default_value = "bilinear")]
    interp: String,
    #[arg(short = 'v', long)]
    verbose: bool,
}

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

/// Equidistant fisheye backward map (horizontal only):
/// Given desired rectilinear horizontal angle (tan_theta_x),
/// with the row fixed at a known vertical angle (tan_theta_y),
/// find the source fisheye x-coordinate normalized to image half-width.
///
/// We work in 3D ray space:
///   rectilinear ray direction: (tan_x, tan_y, 1) normalized
///   θ = angle from optical axis = atan(sqrt(tan_x²+tan_y²))
///   φ = azimuth = atan2(tan_y, tan_x)
///   equidistant r = θ / half_aov  (normalized to 0..1 at max angle)
///   source coords: (r·cos(φ), r·sin(φ))
fn fisheye_src_x(
    tan_x: f64,   // desired rectilinear horizontal (what we want to appear here)
    tan_y: f64,   // vertical angle of this row (fixed, from strip position)
    half_aov: f64,
) -> Option<f64> {
    let r_rect = (tan_x * tan_x + tan_y * tan_y).sqrt();
    let theta = r_rect.atan();
    if theta >= half_aov * 0.998 { return None; }
    let phi = tan_y.atan2(tan_x);
    let r_fish = theta / half_aov; // normalized 0..1
    // x component of fisheye source in normalized coords
    Some(r_fish * phi.cos())
}

fn bilinear(img: &DynamicImage, x: f64, y: f64) -> [u8; 3] {
    let (w, h) = img.dimensions();
    let x0 = x.floor() as i64;
    let y0 = y.floor() as i64;
    let cx = |v: i64| v.clamp(0, w as i64 - 1) as u32;
    let cy = |v: i64| v.clamp(0, h as i64 - 1) as u32;
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
    let p = img.get_pixel(xi, yi).0;
    [p[0],p[1],p[2]]
}

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
    let half_aov = laowa_8_15_half_aov_deg(fl_ff.clamp(8.0, 15.0)).to_radians();

    // Strip
    let strip_frac = args.strip_percent.clamp(0.01, 1.0);
    let strip_h = ((src_h as f64 * strip_frac).round() as u32).max(1);
    let strip_y0 = (src_h - strip_h) / 2;

    let out_w = ((src_w as f64 * args.scale).round() as u32).max(1);
    let out_h = ((strip_h as f64 * args.scale).round() as u32).max(1);

    // Normalisation: we work in units of "half the image diagonal" so
    // the image circle edge = 1.0. All source lookups are in full-image pixel coords.
    let src_cx = src_w as f64 / 2.0;  // optical center x
    let src_cy = src_h as f64 / 2.0;  // optical center y
    // norm_r: the angular scale. We want half_aov to correspond to the half-diagonal.
    // So 1 unit of normalized coord = half_aov radians.
    let norm_r = (src_cx * src_cx + src_cy * src_cy).sqrt();

    // Max horizontal angle covered by the frame half-width
    let max_tan_x = (src_cx / norm_r) * half_aov.tan();

    if args.verbose {
        eprintln!("Input: {}x{}  focal: {:.1}mm  crop: {:.3}  half-AoV: {:.1}°",
            src_w, src_h, focal_mm, crop_factor, half_aov.to_degrees());
        eprintln!("Strip: y=[{}, {}] h={}px  output: {}x{}",
            strip_y0, strip_y0+strip_h, strip_h, out_w, out_h);
        eprintln!("blend_power: {}  edge_strength: {}", args.blend_power, args.edge_strength);
    }

    let interp_mode = args.interp.to_lowercase();
    let img_ref = &img;

    let pixels: Vec<(u32, u32, [u8; 3])> = (0..out_h)
        .into_par_iter()
        .flat_map(|oy| {
            let mut row = Vec::with_capacity(out_w as usize);

            // This output row maps to a source row in the strip
            // oy=0 → strip_y0,  oy=out_h-1 → strip_y0+strip_h-1
            let src_y_frac = (oy as f64 + 0.5) / out_h as f64; // 0..1 within strip
            let src_y_px = strip_y0 as f64 + src_y_frac * strip_h as f64;

            // Vertical angle of this row from optical axis
            let dy_norm = (src_y_px - src_cy) / norm_r; // normalized, signed
            let tan_y = dy_norm * half_aov.tan(); // approximate vertical angle

            for ox in 0..out_w {
                // Normalized horizontal output position: -1 at left, +1 at right
                let ox_norm = (ox as f64 + 0.5) / out_w as f64 * 2.0 - 1.0;

                // Identity source x (no correction): same relative position in source
                let id_src_x = src_cx + ox_norm * src_cx; // maps [-1,1] → [0, src_w]

                // Blend weight: 0 at center, edge_strength at edge
                let weight = args.edge_strength * ox_norm.abs().powf(args.blend_power);

                // Full rectilinear correction:
                // What horizontal angle does this output pixel represent in rectilinear?
                let tan_x = ox_norm * max_tan_x;

                let corrected_src_x = fisheye_src_x(tan_x, tan_y, half_aov)
                    .map(|nx| {
                        // nx is in normalized fisheye coords [-1..1] (image circle)
                        // convert to pixel: center + nx * norm_r (full diagonal scale)
                        src_cx + nx * norm_r
                    })
                    .unwrap_or(id_src_x);

                // Graduated blend
                let final_src_x = id_src_x + weight * (corrected_src_x - id_src_x);

                let color = if interp_mode == "nearest" {
                    nearest(img_ref, final_src_x, src_y_px)
                } else {
                    bilinear(img_ref, final_src_x, src_y_px)
                };

                row.push((ox, oy, color));
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
