/// defish — Laowa 8-15mm fisheye: full 2D remap to cylindrical or rectilinear
///
/// This is a FULL 2D remap, not a horizontal-only crop+rescale.
///
/// For each output pixel at (azimuth az, elevation el):
///   - Source fisheye pixel is found by computing the 3D ray direction
///   - The ray lands at a point in the fisheye that may be OUTSIDE the
///     naive center-strip crop, pulling in content from above/below
///
/// This is the key: a tree at 60° azimuth and 0° elevation in the output
/// maps to a fisheye source pixel that is at radius r=60°/90°*R from center,
/// along the horizontal. But the SAME tree's branches at 60° az, 15° el
/// pull from r=62°/90°*R along a direction angled up-right — which is OUTSIDE
/// a naive horizontal crop. The cylindrical remap shows the correct tree.
///
/// Projections:
///   Cylindrical:  x=f*az,   y=f*tan(el)   — uniform angular spacing, no edge stretch
///   Rectilinear:  x=f*tan(az), y=f*tan(el) — straight lines, edge stretch near 90°
///
/// Auto-selected: cylindrical for circular fisheye (8mm), rectilinear for 10-15mm.

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
#[command(name = "defish",
    about = "Full 2D defish of Laowa 8-15mm fisheye to cylindrical or rectilinear strip",
    long_about = "\
Full 2D fisheye → cylindrical/rectilinear remap. Unlike a simple crop, edge \
columns pull content from outside the naive horizontal strip — e.g. a tree at \
60° to the left shows branches that were above/below the crop line in the \
fisheye, remapped to their correct position in the output.\n\n\
Auto-selects projection:\n  \
  circular fisheye (8mm, black border) → cylindrical (no edge stretch)\n  \
  full-frame fisheye (10-15mm)         → rectilinear (straight lines)\n\n\
Examples:\n  \
  defish photo.jpg                   # full auto\n  \
  defish photo.jpg -p 0.5           # 50% vertical coverage of image circle\n  \
  defish photo.jpg --projection cyl # force cylindrical\n  \
  defish photo.jpg -f 8 -c 0.79    # GFX 100S II at 8mm"
)]
struct Args {
    input: PathBuf,
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Vertical coverage: fraction of image circle diameter (for circular)
    /// or frame height (for full-frame). Controls how tall the output is.
    #[arg(short = 'p', long, default_value_t = 0.50)]
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
    /// Output projection: rect or cyl. Auto-selected from focal length.
    #[arg(long)]
    projection: Option<Projection>,
    /// Output scale multiplier
    #[arg(short = 's', long, default_value_t = 1.0)]
    scale: f64,
    #[arg(short = 'v', long)]
    verbose: bool,
}

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

fn laowa_half_aov_deg(focal_mm_ff: f64) -> f64 {
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

fn detect_image_circle(img: &DynamicImage) -> (f64, bool) {
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
    if radii.is_empty() { return (cx.min(cy), false); }
    radii.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let med = radii[radii.len() / 2];
    let spread = radii.last().unwrap() - radii.first().unwrap();
    if spread < 0.15 * med { (med, true) } else { (half_diag, false) }
}

fn bilinear(img: &DynamicImage, x: f64, y: f64) -> Option<[u8; 3]> {
    let (w, h) = img.dimensions();
    // Return None if outside image bounds (will show as black)
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

fn main() {
    let args = Args::parse();

    let img = image::open(&args.input).unwrap_or_else(|e| {
        eprintln!("Cannot open '{}': {}", args.input.display(), e); std::process::exit(1);
    });
    let (src_w, src_h) = img.dimensions();
    let src_cx = src_w as f64 / 2.0;
    let src_cy = src_h as f64 / 2.0;

    let focal_mm = args.focal_length
        .or_else(|| read_exif_focal_length(&args.input))
        .unwrap_or_else(|| { eprintln!("No EXIF focal length; assuming 15mm."); 15.0 });

    let crop_factor = args.crop_factor.unwrap_or_else(|| {
        read_exif_model(&args.input).map(|m| detect_crop_factor(&m))
            .unwrap_or_else(|| { eprintln!("No EXIF camera model; assuming full-frame."); 1.0 })
    });

    let fl_ff     = focal_mm * crop_factor;
    let theta_max = laowa_half_aov_deg(fl_ff.clamp(8.0, 15.0)).to_radians();

    let (r_circle, is_circular) = if let Some(r) = args.circle_radius {
        let hd = (src_cx*src_cx + src_cy*src_cy).sqrt();
        (r, r < 0.9 * hd)
    } else {
        detect_image_circle(&img)
    };

    let proj = args.projection.clone().unwrap_or(
        if is_circular { Projection::Cylindrical } else { Projection::Rectilinear }
    );

    // f_pix: pixels per radian at center (same for source and output at center)
    // equidistant: r = theta/theta_max * R  → dr/dtheta = R/theta_max
    let f_pix = r_circle / theta_max;

    // Output angular coverage:
    //   Horizontal: az_max = theta_h (the horizontal half-AoV of the lens,
    //               limited by image circle or strip choice)
    //   Vertical:   el_max chosen by strip_percent
    //
    // For circular: full circle covers ±90° in all directions.
    //   strip_percent controls el_max: 0.5 → el_max=arctan(0.5*R / f_pix)
    // For full-frame: strip_percent controls fraction of frame height.

    // el_max: maximum elevation angle in output
    // We choose el_max such that the output height covers strip_percent of
    // the source image circle diameter (for circular) or frame height (full-frame)
    let half_h_src = if is_circular { r_circle } else { src_cy };
    // strip_percent * half_h_src gives half-height in source pixels at az=0
    // At az=0: y_src = f_pix * sin(el) ≈ f_pix * el for small el,
    // but exactly: for equidistant, y_src = (el / theta_max) * R = f_pix * el
    let el_max_src_px = args.strip_percent.clamp(0.01, 0.99) * half_h_src;
    // el_max = el_max_src_px / f_pix  (equidistant: y = f_pix * el)
    let el_max = (el_max_src_px / f_pix).min(theta_max * 0.95);

    // az_max: maximum azimuth. For circular: limited so the 3D ray stays
    // within the image circle (theta < theta_max).
    // At el=el_max, az_max satisfies: acos(cos(az)*cos(el_max)) < theta_max
    // → cos(az) > cos(theta_max)/cos(el_max)
    // → az < acos(cos(theta_max)/cos(el_max))
    // Cap slightly below to avoid sampling outside the circle.
    let az_max = if is_circular {
        let cos_az_max = (theta_max * 0.98).cos() / el_max.cos();
        if cos_az_max <= -1.0 { theta_max * 0.98 }
        else if cos_az_max >= 1.0 { 0.0 }
        else { cos_az_max.acos() }
    } else {
        // Full-frame: az_max from equidistant at horizontal frame edge
        (src_cx / r_circle) * theta_max
    };

    // Output dimensions
    // Cylindrical:  x = f_pix * az  → out_w = 2 * f_pix * az_max
    // Rectilinear:  x = f_pix * tan(az) → out_w = 2 * f_pix * tan(az_max)
    let cx_out = match proj {
        Projection::Cylindrical => f_pix * az_max,
        Projection::Rectilinear => f_pix * az_max.tan(),
    };
    // Vertical: y = f_pix * tan(el)  (both projections use perspective vertical)
    // out_h = 2 * f_pix * tan(el_max)
    let cy_out = f_pix * el_max.tan();

    let out_w = ((cx_out * 2.0 * args.scale).round() as u32).max(1);
    let out_h = ((cy_out * 2.0 * args.scale).round() as u32).max(1);

    if args.verbose {
        eprintln!("Camera:     focal {:.1}mm  crop {:.3}×  FF-equiv {:.1}mm",
            focal_mm, crop_factor, fl_ff);
        eprintln!("Lens:       theta_max={:.1}°  f_pix={:.1}px/rad  R={:.0}px",
            theta_max.to_degrees(), f_pix, r_circle);
        eprintln!("Circle:     R={:.0}px  {}",
            r_circle, if is_circular { "circular (black border detected)" } else { "full-frame" });
        eprintln!("Coverage:   az±{:.1}°  el±{:.1}°",
            az_max.to_degrees(), el_max.to_degrees());
        eprintln!("Projection: {:?}", proj);
        eprintln!("Output:     {}×{}px", out_w, out_h);
    }

    // -------------------------------------------------------------------------
    // Full 2D backward remap
    //
    // For each output pixel (ox, oy):
    //   1. Convert to normalized output coords (ox_norm, oy_norm) in [-1,+1]
    //   2. Compute the 3D ray direction (azimuth, elevation):
    //        Cylindrical:  az = ox_norm * az_max
    //                      el = atan(oy_norm * tan(el_max))
    //        Rectilinear:  az = atan(ox_norm * tan(az_max))
    //                      el = atan(oy_norm * tan(el_max))
    //   3. Compute off-axis angle: theta = acos(cos(az)*cos(el))
    //   4. Equidistant backward: r = theta/theta_max * R_circle
    //   5. Project onto sensor:
    //        x_src = cx + r * sin(az)/sin(theta)
    //        y_src = cy + r * sin(el)/sin(theta)
    //      (for theta≈0: x_src = cx + f_pix*az, y_src = cy + f_pix*el)
    // -------------------------------------------------------------------------

    let img_ref   = &img;
    let tan_el_max = el_max.tan();
    let tan_az_max = az_max.tan();

    let pixels: Vec<(u32, u32, [u8; 3])> = (0..out_h)
        .into_par_iter()
        .flat_map(|oy| {
            let mut row = Vec::with_capacity(out_w as usize);
            // oy_norm: -1=top, 0=center, +1=bottom
            let oy_norm = (oy as f64 + 0.5 - cy_out) / cy_out;
            // Elevation angle (both projections use perspective vertical)
            let el = (oy_norm * tan_el_max).atan();

            for ox in 0..out_w {
                let ox_norm = (ox as f64 + 0.5 - cx_out) / cx_out;

                // Azimuth angle
                let az = match proj {
                    Projection::Cylindrical => ox_norm * az_max,
                    Projection::Rectilinear => (ox_norm * tan_az_max).atan(),
                };

                // Off-axis angle from optical axis
                let cos_theta = az.cos() * el.cos();
                let theta = cos_theta.acos();

                // Source pixel via equidistant backward map
                let (src_x, src_y) = if theta < 1e-9 {
                    (src_cx, src_cy)
                } else {
                    let r = theta / theta_max * r_circle;
                    let sin_th = theta.sin();
                    (src_cx + r * az.sin() * el.cos() / sin_th,
                     src_cy + r * el.sin() / sin_th)
                };

                let rgb = bilinear(img_ref, src_x, src_y).unwrap_or([0, 0, 0]);
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
