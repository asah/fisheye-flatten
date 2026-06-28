// defish — fisheye toolkit CLI.
//
// Subcommands (all share the remap engine and lens model in lib.rs):
//   flatten  fisheye → cylindrical/rectilinear strip (the default; see below)
//   refish   (planned) rectilinear → fisheye
//   tunnel   (planned) radial "center further away" warp
//
// Back-compat: `defish photo.jpg ...` with no subcommand runs `flatten`, so
// existing invocations keep working.

use clap::{Args, Parser, Subcommand};
use image::{DynamicImage, GenericImageView};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command as Proc, Stdio};

use fisheye_defish::*;

#[derive(Parser, Debug)]
#[command(name = "defish", version,
    about = "Fisheye toolkit: flatten (and, soon, refish / tunnel) for the Laowa 8-15mm",
    long_about = "\
A small fisheye toolkit. The default `flatten` operation does a full 2D \
fisheye → cylindrical/rectilinear remap: unlike a simple crop, edge columns \
pull content from outside the naive horizontal strip.\n\n\
Running `defish photo.jpg` (no subcommand) is shorthand for `defish flatten \
photo.jpg`, so existing commands keep working."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Flatten a fisheye photo to a cylindrical/rectilinear strip
    Flatten(FlattenArgs),
    /// Project a rectilinear photo into a circular fisheye
    Refish(RefishArgs),
    /// Radial warp: push the center back ("tunnel") or bulge it forward
    Tunnel(TunnelArgs),
    /// Animate the flatten: N frames from fisheye → flat, to video or frames
    Animate(AnimateArgs),
}

#[derive(Args, Debug)]
#[command(long_about = "\
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
struct FlattenArgs {
    input: PathBuf,
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Vertical coverage of the image circle (circular) or frame (full-frame),
    /// as a fraction (0.30) or percentage (30) — both mean 30%. Taller = bigger.
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
    /// JPEG output quality, 1–100 (ignored for non-JPEG output)
    #[arg(short = 'q', long, default_value_t = 95)]
    quality: u8,
    /// Calibrate: given the TRUE focal length (mm) of this shot, print the
    /// recommended --calib-scale and exit without writing an image.
    #[arg(long, value_name = "FOCAL_MM")]
    calibrate: Option<f64>,
    /// Correction factor for the image-circle→focal-length model (from
    /// --calibrate). Default 1.0 uses the uncalibrated equidistant model.
    #[arg(long, default_value_t = 1.0)]
    calib_scale: f64,
    #[arg(short = 'v', long)]
    verbose: bool,
}

#[derive(Args, Debug)]
#[command(long_about = "\
Project a rectilinear (normal) photo into a circular fisheye — the inverse of \
flatten. Straight lines bow outward and the frame is wrapped into a disc.\n\n\
You can only render the field of view the source actually contains: a 70° photo \
fills the circle out to 70°, with black beyond. The source FoV is taken from \
EXIF focal length + body, or set it directly with --source-fov.\n\n\
Examples:\n  \
  defish refish photo.jpg                  # fill the circle with the source FoV\n  \
  defish refish photo.jpg --fov 180        # exaggerate to a 180° fisheye look\n  \
  defish refish photo.jpg --source-fov 75  # source has no usable EXIF"
)]
struct RefishArgs {
    input: PathBuf,
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Output fisheye diameter in pixels (default: shorter source side)
    #[arg(long)]
    size: Option<u32>,
    /// Angular coverage of the OUTPUT fisheye, full angle in degrees
    /// (default: the source's own horizontal field of view)
    #[arg(long)]
    fov: Option<f64>,
    /// Source horizontal field of view in degrees (overrides EXIF-derived)
    #[arg(long)]
    source_fov: Option<f64>,
    /// Source focal length in mm (normally read from EXIF)
    #[arg(short = 'f', long)]
    focal_length: Option<f64>,
    /// Source sensor crop factor (auto from EXIF model)
    #[arg(short = 'c', long)]
    crop_factor: Option<f64>,
    /// JPEG output quality, 1–100 (ignored for non-JPEG output)
    #[arg(short = 'q', long, default_value_t = 95)]
    quality: u8,
    #[arg(short = 'v', long)]
    verbose: bool,
}

#[derive(Args, Debug)]
#[command(allow_negative_numbers = true, long_about = "\
Radial warp about the image center, applied to any image (fisheye or not). \
Positive strength compresses the center and magnifies the edges, making the \
center look further away (a \"tunnel\"); negative strength does the opposite \
(a bulge). The frame is filled edge-to-edge — no black borders.\n\n\
Examples:\n  \
  defish tunnel photo.jpg              # default tunnel (strength 1.0)\n  \
  defish tunnel photo.jpg -k 2.5       # stronger tunnel\n  \
  defish tunnel photo.jpg -k -0.5      # bulge the center forward"
)]
struct TunnelArgs {
    input: PathBuf,
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Effect strength: >0 pushes the center back (tunnel), <0 bulges it forward.
    /// 0 is a no-op. Clamped above -0.95.
    #[arg(short = 'k', long, default_value_t = 1.0)]
    strength: f64,
    /// JPEG output quality, 1–100 (ignored for non-JPEG output)
    #[arg(short = 'q', long, default_value_t = 95)]
    quality: u8,
    #[arg(short = 'v', long)]
    verbose: bool,
}

#[derive(Args, Debug)]
#[command(long_about = "\
Animate the flattening: render N frames that morph from the original fisheye \
(frame 0) to the fully-flattened panorama (last frame), anchored at the center \
so the edges \"unroll\". Takes every `flatten` option (-f, -p, --projection, …).\n\n\
By default it streams frames straight into ffmpeg (full color, small file). If \
ffmpeg isn't installed it falls back to writing a numbered image sequence and \
prints the ffmpeg command to assemble it. Codec is chosen by the -o extension: \
.mp4/.mkv→H.264, .mov→ProRes, .webm→VP9.\n\n\
Examples:\n  \
  defish animate photo.jpg --steps 60                 # → photo_anim.mp4 (H.264)\n  \
  defish animate photo.jpg --steps 60 -o unroll.mov   # ProRes\n  \
  defish animate photo.jpg --steps 60 --pix-fmt yuv444p   # no chroma subsampling\n  \
  defish animate photo.jpg --steps 60 --frames        # numbered PNG sequence"
)]
struct AnimateArgs {
    #[command(flatten)]
    flatten: FlattenArgs,
    /// Number of frames: frame 0 = fisheye, last = fully flat.
    #[arg(long, default_value_t = 30)]
    steps: u32,
    /// Animation speed: morph frames shown per second. The video is always
    /// encoded at a compatible 30fps (frames duplicated to fit).
    #[arg(long, default_value_t = 30.0)]
    fps: f64,
    /// Quality for H.264/VP9 (CRF): lower = better/larger. Typical 14–24.
    #[arg(long, default_value_t = 16)]
    crf: u32,
    /// ffmpeg pixel format, e.g. yuv420p (default) or yuv444p (no subsampling).
    #[arg(long, default_value = "yuv420p")]
    pix_fmt: String,
    /// Cap frame width in pixels (keeps videos sane); 0 disables.
    #[arg(long, default_value_t = 1920)]
    max_width: u32,
    /// Write a numbered image sequence instead of a video (no ffmpeg needed).
    #[arg(long)]
    frames: bool,
    /// Start on the whole input frame and animate the top/bottom crop first,
    /// then the unroll (instead of starting on the cropped strip).
    #[arg(long)]
    show_crop: bool,
    /// Fraction of the timeline spent on the crop phase when --show-crop is set.
    #[arg(long, default_value_t = 0.4)]
    crop_frac: f64,
}

fn main() {
    // Default-subcommand shim: if the first argument isn't a known subcommand or
    // a global flag, insert `flatten` so `defish photo.jpg ...` keeps working.
    let mut argv: Vec<String> = std::env::args().collect();
    let first_is_sub = matches!(
        argv.get(1).map(|s| s.as_str()),
        Some("flatten") | Some("refish") | Some("tunnel") | Some("animate") | Some("help")
            | Some("-h") | Some("--help") | Some("-V") | Some("--version")
    );
    if argv.len() > 1 && !first_is_sub {
        argv.insert(1, "flatten".to_string());
    }

    match Cli::parse_from(argv).command {
        Command::Flatten(a) => run_flatten(&a),
        Command::Refish(a) => run_refish(&a),
        Command::Tunnel(a) => run_tunnel(&a),
        Command::Animate(a) => run_animate(&a),
    }
}

/// Resolved flatten geometry — everything the per-pixel sampler needs. Shared
/// by `flatten` (renders the final image) and `animate` (renders frames at
/// intermediate correction strengths).
struct Geom {
    src_cx: f64, src_cy: f64,
    cx_out: f64, cy_out: f64,     // output center = scaled half-extent
    az_max: f64, el_max: f64,     // angular coverage (radians)
    tan_az_max: f64, tan_el_max: f64,
    theta_max: f64, r_circle: f64,
    f_pix: f64,
    out_w: u32, out_h: u32,
    scale: f64,
    proj: Projection,
}

#[allow(clippy::too_many_arguments)]
fn build_geom(
    src_cx: f64, src_cy: f64, r_circle: f64, is_circular: bool,
    theta_max: f64, strip_frac: f64, proj: Projection, scale: f64,
) -> Geom {
    let f_pix = r_circle / theta_max;
    // Vertical coverage from the strip fraction (equidistant: y_src = f_pix·el).
    let half_h_src = if is_circular { r_circle } else { src_cy };
    let el_max = ((strip_frac * half_h_src) / f_pix).min(theta_max * 0.95);
    // Horizontal coverage = full circle half-FoV (decoupled from el_max).
    let az_max = if is_circular { theta_max * 0.98 } else { (src_cx / r_circle) * theta_max };
    // Output center/extent, scaled. `scale` folds into the center so the output
    // stays symmetric at any scale (at scale=1 this equals the unscaled extent).
    let cx_out = (match proj {
        Projection::Cylindrical => f_pix * az_max,
        Projection::Rectilinear => f_pix * az_max.tan(),
    }) * scale;
    let cy_out = f_pix * el_max.tan() * scale;
    let out_w = ((cx_out * 2.0).round() as u32).max(1);
    let out_h = ((cy_out * 2.0).round() as u32).max(1);
    Geom {
        src_cx, src_cy, cx_out, cy_out, az_max, el_max,
        tan_az_max: az_max.tan(), tan_el_max: el_max.tan(),
        theta_max, r_circle, f_pix, out_w, out_h, scale, proj,
    }
}

/// Full-flatten source coordinate for output pixel (ox, oy) — the t=1 mapping.
fn defish_source_coord(g: &Geom, ox: u32, oy: u32) -> (f64, f64) {
    let oy_norm = (oy as f64 + 0.5 - g.cy_out) / g.cy_out;
    let el = (oy_norm * g.tan_el_max).atan();
    let ox_norm = (ox as f64 + 0.5 - g.cx_out) / g.cx_out;
    let az = match g.proj {
        Projection::Cylindrical => ox_norm * g.az_max,
        Projection::Rectilinear => (ox_norm * g.tan_az_max).atan(),
    };
    let cos_theta = az.cos() * el.cos();
    let theta = cos_theta.acos();
    if theta < 1e-9 {
        (g.src_cx, g.src_cy)
    } else {
        let r = theta / g.theta_max * g.r_circle;
        let sin_th = theta.sin();
        (g.src_cx + r * az.sin() * el.cos() / sin_th,
         g.src_cy + r * el.sin() / sin_th)
    }
}

/// Fisheye passthrough source coord: a 1:1, center-anchored crop of the source
/// (the strip region at the flatten's center scale). This is the t=0 state of
/// the plain unroll — it coincides with the full flatten at the center.
fn strip_passthrough_coord(g: &Geom, ox: u32, oy: u32) -> (f64, f64) {
    (g.src_cx + (ox as f64 + 0.5 - g.cx_out) / g.scale,
     g.src_cy + (oy as f64 + 0.5 - g.cy_out) / g.scale)
}

/// Whole-input source coord: the entire source frame fit into the output canvas
/// (letterboxed), so the animation can start on the actual input photo before
/// the crop. Pixels outside the fitted image sample out of bounds → black.
fn full_fit_coord(g: &Geom, ox: u32, oy: u32) -> (f64, f64) {
    let (src_w, src_h) = (2.0 * g.src_cx, 2.0 * g.src_cy);
    let fit = (g.cx_out * 2.0 / src_w).min(g.cy_out * 2.0 / src_h);
    (g.src_cx + (ox as f64 + 0.5 - g.cx_out) / fit,
     g.src_cy + (oy as f64 + 0.5 - g.cy_out) / fit)
}

#[inline]
fn lerp2(a: (f64, f64), b: (f64, f64), u: f64) -> (f64, f64) {
    (a.0 + u * (b.0 - a.0), a.1 + u * (b.1 - a.1))
}

/// Sample for a partial flatten at strength `t`:
///   t = 1 → full flatten (exactly `defish_source_coord`)
///   t = 0 → fisheye passthrough (or the full input frame, if `show_crop`)
/// Plain mode morphs passthrough → flatten (center-anchored "unroll"). With
/// `show_crop`, the first `crop_frac` of the timeline morphs the full input
/// frame → the cropped strip (animating the top/bottom crop), then the rest
/// unrolls. The last frame is always the full flatten.
fn defish_sample(g: &Geom, img: &DynamicImage, ox: u32, oy: u32, t: f64,
                 show_crop: bool, crop_frac: f64) -> [u8; 3] {
    let s1 = defish_source_coord(g, ox, oy);
    let (sx, sy) = if !show_crop {
        if t >= 1.0 { s1 } else { lerp2(strip_passthrough_coord(g, ox, oy), s1, t) }
    } else {
        let c = crop_frac.clamp(0.01, 0.99);
        let s0 = strip_passthrough_coord(g, ox, oy);
        if t <= c {
            lerp2(full_fit_coord(g, ox, oy), s0, t / c) // phase 1: crop
        } else {
            lerp2(s0, s1, (t - c) / (1.0 - c))          // phase 2: unroll
        }
    };
    bilinear(img, sx, sy).unwrap_or([0, 0, 0])
}

/// Image + detected body/circle params shared by flatten, calibrate, animate.
struct Detected {
    img: DynamicImage,
    crop_factor: f64,
    r_circle: f64,
    is_circular: bool,
    pitch_mm: f64,
}

fn open_and_detect(args: &FlattenArgs) -> Detected {
    let img = image::open(&args.input).unwrap_or_else(|e| {
        eprintln!("Cannot open '{}': {}", args.input.display(), e); std::process::exit(1);
    });
    let (src_w, src_h) = img.dimensions();
    let (src_cx, src_cy) = (src_w as f64 / 2.0, src_h as f64 / 2.0);
    // The camera model gives crop factor and pixel pitch.
    let model = read_exif_model(&args.input);
    let crop_factor = args.crop_factor.unwrap_or_else(|| {
        model.as_deref().map(detect_crop_factor)
            .unwrap_or_else(|| { eprintln!("No EXIF camera model; assuming full-frame."); 1.0 })
    });
    // Image circle (independent of focal length); its radius encodes the focal.
    let (r_circle, is_circular) = if let Some(r) = args.circle_radius {
        let hd = (src_cx*src_cx + src_cy*src_cy).sqrt();
        (r, r < 0.9 * hd)
    } else {
        detect_image_circle(&img)
    };
    let pitch_mm = sensor_pixel_pitch_mm(&args.input, model.as_deref(), crop_factor, src_w);
    Detected { img, crop_factor, r_circle, is_circular, pitch_mm }
}

/// A fully-resolved flatten: source image, geometry, and reporting parameters.
/// `scale` lets callers (animate) override `-s` without mutating args.
struct Resolved {
    img: DynamicImage,
    g: Geom,
    focal_mm: f64,
    crop_factor: f64,
    fl_ff: f64,
    pitch_mm: f64,
    r_circle: f64,
    is_circular: bool,
    strip_frac: f64,
    proj: Projection,
}

fn resolve(args: &FlattenArgs, scale: f64) -> Resolved {
    let d = open_and_detect(args);
    let (src_w, src_h) = d.img.dimensions();
    let (src_cx, src_cy) = (src_w as f64 / 2.0, src_h as f64 / 2.0);

    // Focal length: explicit flag, then EXIF (non-positive = absent, as manual
    // lenses report 0), then recovered from the image circle.
    let focal_mm = args.focal_length
        .or_else(|| read_exif_focal_length(&args.input).filter(|f| *f > 0.0))
        .unwrap_or_else(|| auto_focal_length(d.r_circle, d.is_circular, d.pitch_mm, d.crop_factor, args.calib_scale));
    let fl_ff = focal_mm * d.crop_factor;
    let theta_max = laowa_half_aov_deg(fl_ff.clamp(8.0, 15.0)).to_radians();
    let proj = args.projection.clone().unwrap_or(
        if d.is_circular { Projection::Cylindrical } else { Projection::Rectilinear });
    // -p accepts a fraction (0.30) or a percentage (30): values >1 → percent.
    let strip_frac = (if args.strip_percent > 1.0 {
        args.strip_percent / 100.0
    } else {
        args.strip_percent
    }).clamp(0.01, 0.99);
    let g = build_geom(src_cx, src_cy, d.r_circle, d.is_circular, theta_max, strip_frac,
                       proj.clone(), scale);
    Resolved {
        img: d.img, g, focal_mm, crop_factor: d.crop_factor, fl_ff, pitch_mm: d.pitch_mm,
        r_circle: d.r_circle, is_circular: d.is_circular, strip_frac, proj,
    }
}

fn verbose_geom(r: &Resolved, quality: u8) {
    eprintln!("Camera:     focal {:.1}mm  crop {:.3}×  FF-equiv {:.1}mm  pitch {:.2}µm",
        r.focal_mm, r.crop_factor, r.fl_ff, r.pitch_mm * 1000.0);
    eprintln!("Lens:       theta_max={:.1}°  f_pix={:.1}px/rad  R={:.0}px",
        r.g.theta_max.to_degrees(), r.g.f_pix, r.r_circle);
    eprintln!("Circle:     R={:.0}px  {}",
        r.r_circle, if r.is_circular { "circular (image circle detected)" } else { "full-frame" });
    eprintln!("Coverage:   az±{:.1}°  el±{:.1}°", r.g.az_max.to_degrees(), r.g.el_max.to_degrees());
    eprintln!("Projection: {:?}", r.proj);
    eprintln!("Output:     {}×{}px  (JPEG quality {})", r.g.out_w, r.g.out_h, quality.clamp(1, 100));
}

fn flatten_description(r: &Resolved, args: &FlattenArgs) -> String {
    let proj_name = match r.proj {
        Projection::Cylindrical => "cylindrical",
        Projection::Rectilinear => "rectilinear",
    };
    let circle = if r.is_circular { "circular" } else { "full-frame" };
    let src_name = args.input.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    // ASCII only: EXIF ImageDescription is an ASCII field.
    format!(
        "defish (fisheye-flatten v{ver}): projection={proj_name}, focal={focal:.1}mm, \
         crop={crop:.3}x, FF-equiv={ffeq:.1}mm, lens={circle} R={radius:.0}px, \
         strip={strip:.0}%, scale={scale}, quality={quality}, \
         coverage=az+/-{az:.1}deg/el+/-{el:.1}deg, output={ow}x{oh}px, source={src_name}",
        ver = env!("CARGO_PKG_VERSION"),
        focal = r.focal_mm, crop = r.crop_factor, ffeq = r.fl_ff,
        radius = r.r_circle, strip = r.strip_frac * 100.0, scale = args.scale,
        quality = args.quality.clamp(1, 100),
        az = r.g.az_max.to_degrees(), el = r.g.el_max.to_degrees(), ow = r.g.out_w, oh = r.g.out_h,
    )
}

fn run_calibrate(args: &FlattenArgs, known_f: f64) {
    let d = open_and_detect(args);
    if !d.is_circular {
        eprintln!("Cannot calibrate: no image circle detected (need a circular \
                   fisheye shot with a visible black border).");
        std::process::exit(1);
    }
    let model_r = model_circle_radius_px(known_f, d.pitch_mm, d.crop_factor, 1.0);
    let k = d.r_circle / model_r;
    println!("Calibration @ {:.1}mm:", known_f);
    println!("  measured circle r = {:.0}px", d.r_circle);
    println!("  model    circle r = {:.0}px  (pitch {:.2}µm, crop {:.3}×)",
        model_r, d.pitch_mm * 1000.0, d.crop_factor);
    println!("  → re-run with  --calib-scale {:.4}", k);
}

fn run_flatten(args: &FlattenArgs) {
    // --calibrate measures the circle and reports a scale, then stops.
    if let Some(known_f) = args.calibrate { run_calibrate(args, known_f); return; }

    let r = resolve(args, args.scale);
    if args.verbose { verbose_geom(&r, args.quality); }

    let out_img = render(r.g.out_w, r.g.out_h, |ox, oy| defish_sample(&r.g, &r.img, ox, oy, 1.0, false, 0.5));

    let out_path = args.output.clone().unwrap_or_else(|| default_output(&args.input, "defish"));
    save_image(&out_img, &out_path, args.quality).unwrap_or_else(|e| {
        eprintln!("Cannot save: {e}"); std::process::exit(1);
    });
    // Record settings in EXIF so they travel with the file (JPEG only).
    embed_exif(&out_path, &flatten_description(&r, args), args.verbose);
    println!("Saved: {}  ({}×{})", out_path.display(), r.g.out_w, r.g.out_h);
}

/// Build the default output path `{stem}_{suffix}.{ext}` next to the input.
fn default_output(input: &Path, suffix: &str) -> PathBuf {
    let stem = input.file_stem().unwrap_or_default().to_string_lossy();
    let ext  = input.extension().unwrap_or_default().to_string_lossy();
    PathBuf::from(format!("{}_{}.{}", stem, suffix, ext))
}

/// Embed a settings summary + tool tag in the output JPEG (no-op for non-JPEG).
fn embed_exif(path: &Path, description: &str, verbose: bool) {
    if !is_jpeg(path) {
        if verbose { eprintln!("EXIF:       skipped (output is not a JPEG)"); }
        return;
    }
    let software = format!("defish (fisheye-flatten) v{}", env!("CARGO_PKG_VERSION"));
    match write_settings_exif(path, description, &software) {
        Ok(()) => if verbose { eprintln!("EXIF:       {description}"); },
        Err(e) => eprintln!("Warning: could not embed settings in EXIF: {e}"),
    }
}

// ===========================================================================
// refish — rectilinear → circular fisheye (the inverse of flatten)
// ===========================================================================
fn run_refish(args: &RefishArgs) {
    let img = image::open(&args.input).unwrap_or_else(|e| {
        eprintln!("Cannot open '{}': {}", args.input.display(), e); std::process::exit(1);
    });
    let (src_w, src_h) = img.dimensions();
    let src_cx = src_w as f64 / 2.0;
    let src_cy = src_h as f64 / 2.0;

    // Source focal length in pixels (f_src): tells us the source's field of view.
    //   --source-fov wins; else EXIF/flag focal + body pixel pitch; else assume 90°.
    let f_src_pix = if let Some(sfov) = args.source_fov {
        src_cx / (sfov.to_radians() / 2.0).tan()
    } else if let Some(focal) = args.focal_length
        .or_else(|| read_exif_focal_length(&args.input).filter(|f| *f > 0.0))
    {
        let model = read_exif_model(&args.input);
        let crop = args.crop_factor.unwrap_or_else(||
            model.as_deref().map(detect_crop_factor).unwrap_or(1.0));
        let pitch = sensor_pixel_pitch_mm(&args.input, model.as_deref(), crop, src_w);
        focal / pitch // f in pixels = focal_mm / pitch_mm
    } else {
        eprintln!("No source FoV or EXIF focal length; assuming 90° horizontal FoV. \
                   Set --source-fov.");
        src_cx / (45.0_f64).to_radians().tan()
    };

    // Output fisheye angular radius. Default: fill the circle with the source's
    // own horizontal half-FoV (adds barrel distortion without inventing FoV).
    let src_hfov = (src_cx / f_src_pix).atan();
    let theta_max = match args.fov {
        Some(deg) => (deg.to_radians() / 2.0).min(89.9_f64.to_radians()),
        None => src_hfov,
    };

    let size = args.size.unwrap_or_else(|| src_w.min(src_h)).max(1);
    let r_out = size as f64 / 2.0;
    let c_out = r_out; // center of the square output
    const THETA_CAP: f64 = 1.569; // ~89.9° — beyond this tan() explodes / no rectilinear source

    if args.verbose {
        eprintln!("Source:     {}×{}px  f_src={:.0}px/rad  hFoV={:.1}°",
            src_w, src_h, f_src_pix, src_hfov.to_degrees() * 2.0);
        eprintln!("Output:     {0}×{0}px circle  coverage={1:.1}° (±{2:.1}°)  q{3}",
            size, theta_max.to_degrees() * 2.0, theta_max.to_degrees(), args.quality.clamp(1, 100));
    }

    let img_ref = &img;
    let out_img = render(size, size, |ox, oy| {
        let dx = ox as f64 + 0.5 - c_out;
        let dy = oy as f64 + 0.5 - c_out;
        let r = (dx * dx + dy * dy).sqrt();
        if r > r_out { return [0, 0, 0]; }           // outside the image circle
        let theta = r / r_out * theta_max;           // equidistant: r ∝ θ
        if theta >= THETA_CAP { return [0, 0, 0]; }
        if r < 1e-9 {
            return bilinear(img_ref, src_cx, src_cy).unwrap_or([0, 0, 0]);
        }
        // Gnomonic projection of the ray (θ, φ) onto the source plane.
        let rr = f_src_pix * theta.tan();
        let x_src = src_cx + rr * dx / r;
        let y_src = src_cy + rr * dy / r;
        bilinear(img_ref, x_src, y_src).unwrap_or([0, 0, 0])
    });

    let out_path = args.output.clone().unwrap_or_else(|| default_output(&args.input, "refish"));
    save_image(&out_img, &out_path, args.quality).unwrap_or_else(|e| {
        eprintln!("Cannot save: {e}"); std::process::exit(1);
    });

    let src_name = args.input.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let description = format!(
        "refish (fisheye-flatten v{ver}): rectilinear->fisheye, source_hFoV={shf:.1}deg, \
         output_coverage={cov:.1}deg, size={sz}px, quality={q}, source={src}",
        ver = env!("CARGO_PKG_VERSION"), shf = src_hfov.to_degrees() * 2.0,
        cov = theta_max.to_degrees() * 2.0, sz = size, q = args.quality.clamp(1, 100), src = src_name,
    );
    embed_exif(&out_path, &description, args.verbose);
    println!("Saved: {}  ({sz}×{sz})", out_path.display(), sz = size);
}

// ===========================================================================
// tunnel — radial warp about the center (lens-agnostic)
// ===========================================================================
fn run_tunnel(args: &TunnelArgs) {
    let img = image::open(&args.input).unwrap_or_else(|e| {
        eprintln!("Cannot open '{}': {}", args.input.display(), e); std::process::exit(1);
    });
    let (w, h) = img.dimensions();
    let cx = w as f64 / 2.0;
    let cy = h as f64 / 2.0;

    // Backward radial map on a per-direction normalized radius ρ = max(|dx|/cx,
    // |dy|/cy) ∈ [0,1], so ρ=1 lands exactly on the frame edge in every
    // direction → the output fills the frame with no black borders.
    // r_src/r_out = ρ^(p-1), with p = 1/(1+strength):
    //   strength>0 → p<1 → center compressed, edges magnified (tunnel)
    //   strength<0 → p>1 → center magnified (bulge)
    let p = 1.0 / (1.0 + args.strength.max(-0.95));

    if args.verbose {
        eprintln!("Tunnel:     {}×{}px  strength={:.3}  (exponent p={:.3})  q{}",
            w, h, args.strength, p, args.quality.clamp(1, 100));
    }

    let img_ref = &img;
    let out_img = render(w, h, |ox, oy| {
        let dx = ox as f64 + 0.5 - cx;
        let dy = oy as f64 + 0.5 - cy;
        let rho = (dx.abs() / cx).max(dy.abs() / cy);
        if rho < 1e-6 {
            return bilinear(img_ref, cx, cy).unwrap_or([0, 0, 0]);
        }
        let scale = rho.powf(p - 1.0);
        let x_src = cx + dx * scale;
        let y_src = cy + dy * scale;
        bilinear(img_ref, x_src, y_src).unwrap_or([0, 0, 0])
    });

    let out_path = args.output.clone().unwrap_or_else(|| default_output(&args.input, "tunnel"));
    save_image(&out_img, &out_path, args.quality).unwrap_or_else(|e| {
        eprintln!("Cannot save: {e}"); std::process::exit(1);
    });

    let src_name = args.input.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let description = format!(
        "tunnel (fisheye-flatten v{ver}): radial warp, strength={s}, exponent={p:.3}, \
         quality={q}, source={src}",
        ver = env!("CARGO_PKG_VERSION"), s = args.strength, p = p,
        q = args.quality.clamp(1, 100), src = src_name,
    );
    embed_exif(&out_path, &description, args.verbose);
    println!("Saved: {}  ({}×{})", out_path.display(), w, h);
}

// ===========================================================================
// animate — N frames morphing fisheye → flat, to video (ffmpeg) or a sequence
// ===========================================================================
fn ffmpeg_available() -> bool {
    Proc::new("ffmpeg").arg("-version")
        .stdout(Stdio::null()).stderr(Stdio::null())
        .status().map(|s| s.success()).unwrap_or(false)
}

fn is_video_ext(e: &str) -> bool {
    matches!(e.to_lowercase().as_str(), "mp4" | "mov" | "mkv" | "webm" | "avi" | "m4v")
}

/// ffmpeg output args (codec + filters) chosen by container extension. The crop
/// filter forces even dimensions, which yuv420p / ProRes require.
fn video_codec_args(ext: &str, crf: u32, pix_fmt: &str) -> Vec<String> {
    let even = ["-vf".to_string(), "crop=trunc(iw/2)*2:trunc(ih/2)*2".to_string()];
    let codec: Vec<String> = match ext {
        "mov" => vec!["-c:v", "prores_ks", "-profile:v", "3", "-pix_fmt", "yuv422p10le"]
            .into_iter().map(String::from).collect(),
        "webm" => vec!["-c:v".into(), "libvpx-vp9".into(), "-crf".into(), crf.to_string(),
                       "-b:v".into(), "0".into(), "-pix_fmt".into(), pix_fmt.into()],
        _ => vec!["-c:v".into(), "libx264".into(), "-crf".into(), crf.to_string(),
                  "-pix_fmt".into(), pix_fmt.into()],
    };
    even.into_iter().chain(codec).collect()
}

/// Base path (dir + stem, no extension) for a numbered frame sequence.
fn sequence_stem(input: &Path, output: Option<&PathBuf>) -> String {
    match output {
        Some(o) => {
            let stem = o.file_stem().unwrap_or_default().to_string_lossy().into_owned();
            match o.parent().filter(|p| !p.as_os_str().is_empty()) {
                Some(p) => p.join(stem).to_string_lossy().into_owned(),
                None => stem,
            }
        }
        None => format!("{}_anim", input.file_stem().unwrap_or_default().to_string_lossy()),
    }
}

fn run_animate(args: &AnimateArgs) {
    let fa = &args.flatten;

    // Resolve geometry, then cap the frame width so videos stay a sane size.
    let mut r = resolve(fa, fa.scale);
    if args.max_width > 0 && r.g.out_w > args.max_width {
        let capped = fa.scale * (args.max_width as f64 / r.g.out_w as f64);
        r = resolve(fa, capped);
        eprintln!("Note: capping frame width to {}px (scale {:.3}); use --max-width 0 to disable.",
            args.max_width, capped);
    }
    let (w, h) = (r.g.out_w, r.g.out_h);
    let steps = args.steps.max(1);
    if fa.verbose {
        verbose_geom(&r, fa.quality);
        eprintln!("Animate:    {} frames  fps {}  (frame 0 = fisheye, last = flat)", steps, args.fps);
    }
    // t: 0 → fisheye, 1 → fully flat.
    let t_of = |i: u32| if steps <= 1 { 1.0 } else { i as f64 / (steps - 1) as f64 };

    // Numbered image sequence: explicit --frames, or fallback when ffmpeg absent.
    if args.frames || !ffmpeg_available() {
        if !args.frames {
            eprintln!("ffmpeg not found on PATH — writing a numbered image sequence instead.");
        }
        let ext = fa.output.as_ref()
            .and_then(|p| p.extension()).and_then(|e| e.to_str())
            .filter(|e| !is_video_ext(e))
            .unwrap_or("png").to_string();
        let stem = sequence_stem(&fa.input, fa.output.as_ref());
        for i in 0..steps {
            let t = t_of(i);
            let frame = render(w, h, |ox, oy| defish_sample(&r.g, &r.img, ox, oy, t, args.show_crop, args.crop_frac));
            let path = PathBuf::from(format!("{}_{:04}.{}", stem, i, ext));
            save_image(&frame, &path, fa.quality).unwrap_or_else(|e| {
                eprintln!("Cannot save frame {i}: {e}"); std::process::exit(1);
            });
            eprint!("\rframe {}/{}", i + 1, steps);
        }
        eprintln!();
        println!("Saved {} frames: {}_%04d.{}  ({}×{})", steps, stem, ext, w, h);
        println!("Assemble:  ffmpeg -framerate {} -i {}_%04d.{} -c:v libx264 -crf {} -pix_fmt {} out.mp4",
            args.fps, stem, ext, args.crf, args.pix_fmt);
        return;
    }

    // Video: stream raw RGB frames straight into ffmpeg (no temp files).
    let out_path = fa.output.clone()
        .unwrap_or_else(|| default_output(&fa.input, "anim").with_extension("mp4"));
    let ext = out_path.extension().and_then(|e| e.to_str()).unwrap_or("mp4").to_lowercase();

    let mut cmd = Proc::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error", "-y",
              "-f", "rawvideo", "-pix_fmt", "rgb24",
              "-s", &format!("{w}x{h}"), "-r", &format!("{}", args.fps), "-i", "-"]);
    cmd.args(video_codec_args(&ext, args.crf, &args.pix_fmt));
    // Compatibility: emit a constant 30fps stream (each animation frame, fed at
    // the input rate `--fps`, is duplicated to fill 30fps) so finicky players
    // (QuickTime/Preview) animate it; +faststart moves the moov atom to the
    // front for mp4/mov. Perceived speed and duration are unchanged.
    cmd.args(["-r", "30"]);
    if matches!(ext.as_str(), "mp4" | "mov" | "m4v") {
        cmd.args(["-movflags", "+faststart"]);
    }
    cmd.arg(&out_path);
    cmd.stdin(Stdio::piped());
    let mut child = cmd.spawn().unwrap_or_else(|e| {
        eprintln!("Failed to launch ffmpeg: {e}"); std::process::exit(1);
    });
    let mut stdin = child.stdin.take().expect("ffmpeg stdin");
    for i in 0..steps {
        let t = t_of(i);
        let frame = render(w, h, |ox, oy| defish_sample(&r.g, &r.img, ox, oy, t, args.show_crop, args.crop_frac));
        if let Err(e) = stdin.write_all(frame.as_raw()) {
            eprintln!("\nffmpeg pipe closed early: {e}"); break;
        }
        eprint!("\rframe {}/{}", i + 1, steps);
    }
    eprintln!();
    drop(stdin); // EOF so ffmpeg finalizes
    let status = child.wait().unwrap_or_else(|e| {
        eprintln!("ffmpeg wait failed: {e}"); std::process::exit(1);
    });
    if !status.success() {
        eprintln!("ffmpeg exited with {status}"); std::process::exit(1);
    }
    println!("Saved: {}  ({}×{}, {} frames @ {}fps)", out_path.display(), w, h, steps, args.fps);
}
