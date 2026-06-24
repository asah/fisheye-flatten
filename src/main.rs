/// defish — Laowa 8-15mm: center-preserving graduated horizontal defish
///
/// Projection model:
///   Equidistant fisheye: r = (θ/θ_max) · R_circle  [uniform angular sampling]
///   Rectilinear output:  x = f · tan(θ)             [tan stretches edges]
///
///   Center-preserving output width:
///     cx_out_full = tan(θ_h) / θ_max · R_circle
///   This expands the output to keep center pixel density equal to source.
///
///   At wide focal lengths (8mm = 180° diagonal) tan(θ_h) → ∞, so we cap:
///     --max-width-ratio (default 1.5) limits expansion to 1.5× source width.
///   At 15mm: +13% (cap never triggers). At 8mm: capped at 1.5× (vs 2.83× uncapped).
///
///   Graduated blend: weight = edge_strength · |x_norm|^blend_power
///     weight=0 at center → identity (no change)
///     weight=1 at edge   → full rectilinear correction

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
    /// Sensor crop factor vs full-frame. GFX 100S II=0.79, Nikon Z8=1.0 (auto from EXIF)
    #[arg(short = 'c', long)]
    crop_factor: Option<f64>,
    /// How fast edge correction ramps in from center.
    /// 2.0=smooth quadratic  3.0=very center-preserving  1.0=linear
    #[arg(short = 'b', long, default_value_t = 2.0)]
    blend_power: f64,
    /// Max correction strength at far edge (0.0=none/pure crop, 1.0=full rectilinear)
    #[arg(short = 'e', long, default_value_t = 1.0)]
    edge_strength: f64,
    /// Max output width as a ratio of input width. Prevents extreme expansion at
    /// wide focal lengths (8mm = 180° would otherwise expand 183%). Default 1.5.
    #[arg(short = 'w', long, default_value_t = 1.5)]
    max_width_ratio: f64,
    /// Additional output scale multiplier
    #[arg(short = 's', long, default_value_t = 1.0)]
    scale: f64,
    /// Interpolation: nearest | bilinear
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

fn bilinear(img: &DynamicImage, x: f64, y: f64) -> [u8; 3] {
    let (w, h) = img.dimensions();
    let x0 = x.floor() as i64; let y0 = y.floor() as i64;
    let clx = |v: i64| v.clamp(0, w as i64-1) as u32;
    let cly = |v: i64| v.clamp(0, h as i64-1) as u32;
    let p00 = img.get_pixel(clx(x0),   cly(y0)).0;
    let p10 = img.get_pixel(clx(x0+1), cly(y0)).0;
    let p01 = img.get_pixel(clx(x0),   cly(y0+1)).0;
    let p11 = img.get_pixel(clx(x0+1), cly(y0+1)).0;
    let tx = x-x0 as f64; let ty = y-y0 as f64;
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

fn main() {
    let args = Args::parse();

    let img = image::open(&args.input).unwrap_or_else(|e| {
        eprintln!("Cannot open: {e}"); std::process::exit(1);
    });
    let (src_w, src_h) = img.dimensions();

    let focal_mm = args.focal_length
        .or_else(|| read_exif_focal_length(&args.input))
        .unwrap_or_else(|| { eprintln!("No EXIF focal length, assuming 15mm."); 15.0 });

    let crop_factor = args.crop_factor.unwrap_or_else(|| {
        read_exif_model(&args.input).map(|m| detect_crop_factor(&m)).unwrap_or(1.0)
    });

    let fl_ff     = focal_mm * crop_factor;
    let theta_max = laowa_8_15_half_aov_deg(fl_ff.clamp(8.0, 15.0)).to_radians();

    // Sensor geometry
    let src_cx   = src_w as f64 / 2.0;
    let src_cy   = src_h as f64 / 2.0;
    let r_circle = (src_cx*src_cx + src_cy*src_cy).sqrt();

    // Horizontal half-angle at frame edge (equidistant)
    let theta_h = (src_cx / r_circle) * theta_max;

    // Center-preserving full output half-width
    // tan(theta_h) can be large at wide angles — cap it
    let tan_theta_h = theta_h.tan().min(args.max_width_ratio * src_cx / r_circle * theta_max / 1.0);
    // Recompute tan_theta_h properly with cap on output width ratio:
    // cx_out_full = tan(theta_h)/theta_max * R  -- cap so cx_out_full <= max_ratio * src_cx
    let cx_out_full_uncapped = theta_h.tan() / theta_max * r_circle;
    let cx_out_full = cx_out_full_uncapped.min(args.max_width_ratio * src_cx);
    let was_capped  = cx_out_full_uncapped > cx_out_full;

    // Effective tan_theta_h after cap (used for backward map)
    // cx_out_full = tan_eff / theta_max * R  =>  tan_eff = cx_out_full * theta_max / R
    let tan_theta_h_eff = cx_out_full * theta_max / r_circle;

    // Blend output half-width between identity (src_cx) and full
    let cx_out = src_cx + args.edge_strength * (cx_out_full - src_cx);

    // Strip
    let strip_frac  = args.strip_percent.clamp(0.01, 1.0);
    let strip_h_px  = ((src_h as f64 * strip_frac).round() as u32).max(1);
    let strip_y0    = (src_h - strip_h_px) / 2;

    let out_w = ((cx_out * 2.0 * args.scale).round() as u32).max(1);
    // Height: scale with width ratio to preserve pixel density
    // (so aspect ratio within strip remains sensible)
    let width_ratio = cx_out / src_cx;
    let out_h = ((strip_h_px as f64 * width_ratio * args.scale).round() as u32).max(1);

    if args.verbose {
        eprintln!("Input: {}×{}  focal: {:.1}mm  crop: {:.3}  FF-equiv: {:.1}mm",
            src_w, src_h, focal_mm, crop_factor, fl_ff);
        eprintln!("θ_max: {:.2}°  θ_h: {:.2}°  R_circle: {:.0}px",
            theta_max.to_degrees(), theta_h.to_degrees(), r_circle);
        eprintln!("cx_out_full (e=1, uncapped): {:.0}px  ({:+.1}% wider than src){}",
            cx_out_full_uncapped, (cx_out_full_uncapped/src_cx - 1.0)*100.0,
            if was_capped { format!("  → CAPPED at {:.1}×", args.max_width_ratio) } else { String::new() });
        eprintln!("cx_out (e={:.2}): {:.0}px → out: {}×{}  ({:+.1}% wider)",
            args.edge_strength, cx_out, out_w, out_h, (cx_out/src_cx - 1.0)*100.0);
        eprintln!("blend_power: {}  edge_strength: {}  max_width_ratio: {}",
            args.blend_power, args.edge_strength, args.max_width_ratio);
    }

    let interp_mode = args.interp.to_lowercase();
    let img_ref     = &img;

    let pixels: Vec<(u32, u32, [u8; 3])> = (0..out_h)
        .into_par_iter()
        .flat_map(|oy| {
            let mut row = Vec::with_capacity(out_w as usize);

            // Map output row → source row within strip
            // The strip occupies strip_h_px rows in the source.
            // The output has out_h rows = strip_h_px * width_ratio.
            // We need to sample the strip at the right vertical position.
            let src_y_frac = (oy as f64 + 0.5) / out_h as f64;
            let src_y_px   = strip_y0 as f64 + src_y_frac * strip_h_px as f64;

            for ox_px in 0..out_w {
                // Normalize: -1=left edge, 0=center, +1=right edge
                let ox_norm = (ox_px as f64 + 0.5 - cx_out) / cx_out;

                // Identity source x
                let id_src_x = src_cx + ox_norm * src_cx;

                // Full correction: atan maps linear tan-space back to angle,
                // then equidistant maps angle back to source pixel
                let theta_rect = (ox_norm * tan_theta_h_eff).atan();
                let r_fish     = (theta_rect / theta_max) * r_circle;
                let corr_src_x = src_cx + r_fish;

                // Graduated blend
                let weight      = args.edge_strength * ox_norm.abs().powf(args.blend_power);
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
    println!("Saved: {}  ({}×{})", out_path.display(), out_w, out_h);
}
