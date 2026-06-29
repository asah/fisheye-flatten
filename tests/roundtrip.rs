//! Round-trip test: `refish` (rectilinear → fisheye) then `flatten` (fisheye →
//! rectilinear) should reproduce the input checkerboard's central region — the
//! parts that survive the strip crop and the field-of-view limit.
//!
//! It can't be a pixel-exact comparison: `flatten` re-renders at a different
//! pixel scale, so the squares don't line up 1:1. Instead we assert the result
//! is the *same checkerboard in character and orientation*:
//!   - statistics (brightness + contrast) of the central region match the source
//!   - the red-vertical / green-horizontal center cross is preserved on-axis
//!
//! `refish --fov 130` (±65°) is paired with `flatten -f 10 -c 1` so both use the
//! same 65° half-angle equidistant model — i.e. they're true inverses.

mod common;
use common::*;

#[test]
fn refish_then_defish_recovers_similar_image() {
    let src_path = fixture("testdata/checkerboard.jpg");
    assert!(src_path.exists(), "missing fixture {}", src_path.display());
    let fish = tmp("rt_fish.jpg");
    let rec = tmp("rt_rec.jpg");

    // rectilinear → fisheye
    run(&[
        "refish", path(&src_path),
        "--source-fov", "130", "--fov", "130", "--size", "1024",
        "-o", path(&fish),
    ]);
    // fisheye → rectilinear (same 65° model: -f 10 ↔ --fov 130)
    run(&[
        "flatten", path(&fish),
        "--projection", "rect", "-f", "10", "-c", "1", "-p", "60",
        "-o", path(&rec),
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
