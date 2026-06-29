//! Shared helpers for the integration tests (run the real CLI, inspect outputs).
#![allow(dead_code)]

use image::{Rgb, RgbImage};
use std::path::{Path, PathBuf};
use std::process::Command;

/// A path under the crate root (e.g. "testdata/checkerboard.jpg").
pub fn fixture(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// A scratch path unique to the test binary run.
pub fn tmp(name: &str) -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join(name)
}

pub fn path(p: &Path) -> &str {
    p.to_str().expect("utf-8 path")
}

fn invoke(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_defish"))
        .args(args)
        .output()
        .expect("failed to spawn defish")
}

/// Run defish, require success, return combined stdout+stderr.
pub fn run(args: &[&str]) -> String {
    let out = invoke(args);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "defish {args:?} failed ({}):\n{combined}", out.status);
    combined
}

/// Run defish, require a non-zero exit, return stderr.
pub fn run_fail(args: &[&str]) -> String {
    let out = invoke(args);
    assert!(
        !out.status.success(),
        "defish {args:?} unexpectedly succeeded:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

pub fn load(p: &Path) -> RgbImage {
    image::open(p)
        .unwrap_or_else(|e| panic!("open {}: {e}", p.display()))
        .to_rgb8()
}

pub fn dims(p: &Path) -> (u32, u32) {
    load(p).dimensions()
}

pub fn file_len(p: &Path) -> u64 {
    std::fs::metadata(p).unwrap_or_else(|e| panic!("stat {}: {e}", p.display())).len()
}

pub fn luma(p: &Rgb<u8>) -> f64 {
    let [r, g, b] = p.0;
    0.299 * r as f64 + 0.587 * g as f64 + 0.114 * b as f64
}

/// Mean and std-dev of luminance over the central `frac` of the image.
pub fn central_stats(img: &RgbImage, frac: f64) -> (f64, f64) {
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

/// Fraction of (near-)black pixels — useful for detecting circle borders / letterboxing.
pub fn black_fraction(img: &RgbImage) -> f64 {
    let mut c = 0u64;
    for p in img.pixels() {
        let [r, g, b] = p.0;
        if r < 12 && g < 12 && b < 12 {
            c += 1;
        }
    }
    c as f64 / (img.width() as f64 * img.height() as f64)
}

/// Mean absolute per-channel difference between two equally-sized images.
pub fn mean_abs_diff(a: &RgbImage, b: &RgbImage) -> f64 {
    assert_eq!(a.dimensions(), b.dimensions(), "size mismatch in mean_abs_diff");
    let mut sum = 0.0;
    for (pa, pb) in a.pixels().zip(b.pixels()) {
        for k in 0..3 {
            sum += (pa.0[k] as f64 - pb.0[k] as f64).abs();
        }
    }
    sum / (a.width() as f64 * a.height() as f64 * 3.0)
}

/// Strongly-red fraction in a thin vertical band centered on column `xc`.
pub fn red_band_fraction(img: &RgbImage, xc: u32) -> f64 {
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

/// Strongly-green fraction in a thin horizontal band centered on row `yc`.
pub fn green_band_fraction(img: &RgbImage, yc: u32) -> f64 {
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
