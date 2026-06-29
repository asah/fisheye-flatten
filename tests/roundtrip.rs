//! Round-trip test: `refish` (rectilinear → fisheye) then `flatten` (fisheye →
//! rectilinear) should reproduce the input checkerboard's central region — the
//! parts that survive the strip crop and the field-of-view limit.
//!
//! It can't be a pixel-exact comparison: `flatten` re-renders at a different
//! pixel scale, so the squares don't line up 1:1. Instead we assert the result
//! is the *same checkerboard in character and orientation*:
//!   - statistics (brightness + contrast) of the central region match the source
//!   - the red-vertical / green-horizontal center cross is preserved on-axis
//! Together these catch black/garbage output, lost contrast, flips, and
//! mis-centering, while tolerating the expected resampling/crop differences.
//!
//! `refish --fov 130` (±65°) is paired with `flatten -f 10 -c 1` so both use the
//! same 65° half-angle equidistant model — i.e. they're true inverses.

use image::{Rgb, RgbImage};
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/checkerboard.jpg")
}

fn tmp(name: &str) -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join(name)
}

fn run(args: &[&str]) {
    let status = Command::new(env!("CARGO_BIN_EXE_defish"))
        .args(args)
        .status()
        .expect("failed to spawn defish");
    assert!(status.success(), "defish {args:?} exited with {status}");
}

fn load(p: &Path) -> RgbImage {
    image::open(p)
        .unwrap_or_else(|e| panic!("open {}: {e}", p.display()))
        .to_rgb8()
}

fn luma(p: &Rgb<u8>) -> f64 {
    let [r, g, b] = p.0;
    0.299 * r as f64 + 0.587 * g as f64 + 0.114 * b as f64
}

/// Mean and standard deviation of luminance over the central `frac` of the image.
fn central_stats(img: &RgbImage, frac: f64) -> (f64, f64) {
    let (w, h) = img.dimensions();
    let (cw, ch) = ((w as f64 * frac) as u32, (h as f64 * frac) as u32);
    let (x0, y0) = ((w - cw) / 2, (h - ch) / 2);
    let (mut sum, mut sumsq, mut n) = (0.0, 0.0, 0.0);
    for y in y0..y0 + ch {
        for x in x0..x0 + cw {
            let v = luma(img.get_pixel(x, y));
            sum += v;
            sumsq += v * v;
            n += 1.0;
        }
    }
    let mean = sum / n;
    (mean, (sumsq / n - mean * mean).max(0.0).sqrt())
}

/// Fraction of strongly-red pixels in a thin vertical band centered on column `xc`.
fn red_band_fraction(img: &RgbImage, xc: u32) -> f64 {
    let (w, h) = img.dimensions();
    let band = (w / 200).max(2);
    let (x0, x1) = (xc.saturating_sub(band), (xc + band).min(w - 1));
    let (mut c, mut t) = (0u64, 0u64);
    for x in x0..=x1 {
        for y in 0..h {
            let [r, g, b] = img.get_pixel(x, y).0;
            t += 1;
            if r > 140 && g < 100 && b < 100 {
                c += 1;
            }
        }
    }
    c as f64 / t as f64
}

/// Fraction of strongly-green pixels in a thin horizontal band centered on row `yc`.
fn green_band_fraction(img: &RgbImage, yc: u32) -> f64 {
    let (w, h) = img.dimensions();
    let band = (h / 200).max(2);
    let (y0, y1) = (yc.saturating_sub(band), (yc + band).min(h - 1));
    let (mut c, mut t) = (0u64, 0u64);
    for y in y0..=y1 {
        for x in 0..w {
            let [r, g, b] = img.get_pixel(x, y).0;
            t += 1;
            if g > 130 && r < 110 && b < 110 {
                c += 1;
            }
        }
    }
    c as f64 / t as f64
}

#[test]
fn refish_then_defish_recovers_similar_image() {
    let src_path = fixture();
    assert!(src_path.exists(), "missing fixture {}", src_path.display());
    let fish = tmp("rt_fish.jpg");
    let rec = tmp("rt_rec.jpg");

    // rectilinear → fisheye
    run(&[
        "refish", src_path.to_str().unwrap(),
        "--source-fov", "130", "--fov", "130", "--size", "1024",
        "-o", fish.to_str().unwrap(),
    ]);
    // fisheye → rectilinear (same 65° model: -f 10 ↔ --fov 130)
    run(&[
        "flatten", fish.to_str().unwrap(),
        "--projection", "rect", "-f", "10", "-c", "1", "-p", "60",
        "-o", rec.to_str().unwrap(),
    ]);

    let src = load(&src_path);
    let out = load(&rec);

    // The recovered image is a wide rectilinear strip.
    let (rw, rh) = out.dimensions();
    assert!(rw > rh, "recovered should be wider than tall, got {rw}x{rh}");

    // Same checkerboard in brightness and contrast (not black, washed out, or noise).
    let (sm, ss) = central_stats(&src, 0.4);
    let (rm, rs) = central_stats(&out, 0.4);
    assert!((sm - rm).abs() < 15.0, "central brightness drifted: src {sm:.1} vs rec {rm:.1}");
    assert!((ss - rs).abs() < 20.0, "central contrast drifted: src {ss:.1} vs rec {rs:.1}");
    assert!(rs > 40.0, "recovered lost the checkerboard pattern (contrast {rs:.1})");

    // The center cross is preserved on-axis (proves centering + no flip/color loss).
    let red_center = red_band_fraction(&out, rw / 2);
    let red_quarter = red_band_fraction(&out, rw / 4);
    let green_center = green_band_fraction(&out, rh / 2);
    assert!(red_center > 0.3, "red vertical center line missing ({red_center:.3})");
    assert!(
        red_center > 2.0 * red_quarter,
        "red line not centered (center {red_center:.3} vs quarter {red_quarter:.3})"
    );
    assert!(green_center > 0.3, "green horizontal center line missing ({green_center:.3})");
}
