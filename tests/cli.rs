//! Broad coverage of the CLI: every subcommand and the key options, exercised
//! against the committed fixtures. These run the real binary (no ffmpeg needed —
//! `animate` is tested in `--frames` mode).

mod common;
use common::*;

const CHECKER: &str = "testdata/checkerboard.jpg"; // rectilinear test pattern
const FISH: &str = "tests/test_fisheye.jpg"; // a fisheye photo

// ----------------------------------------------------------------- flatten ---

#[test]
fn flatten_both_projections_produce_wide_output() {
    let fish = fixture(FISH);
    let cyl = tmp("cli_cyl.jpg");
    let rect = tmp("cli_rect.jpg");
    run(&["flatten", path(&fish), "-f", "15", "-c", "1", "--projection", "cyl", "-o", path(&cyl)]);
    run(&["flatten", path(&fish), "-f", "15", "-c", "1", "--projection", "rect", "-o", path(&rect)]);
    let (cw, ch) = dims(&cyl);
    let (rw, rh) = dims(&rect);
    assert!(cw > ch, "cylindrical should be wide: {cw}x{ch}");
    assert!(rw > rh, "rectilinear should be wide: {rw}x{rh}");
    assert_ne!(cw, rw, "the two projections should differ in width");
}

#[test]
fn flatten_strip_accepts_fraction_or_percent() {
    let fish = fixture(FISH);
    let frac = tmp("cli_p_frac.jpg");
    let pct = tmp("cli_p_pct.jpg");
    run(&["flatten", path(&fish), "-f", "15", "-c", "1", "-p", "0.3", "-o", path(&frac)]);
    run(&["flatten", path(&fish), "-f", "15", "-c", "1", "-p", "30", "-o", path(&pct)]);
    assert_eq!(dims(&frac), dims(&pct), "`-p 0.3` and `-p 30` must mean the same thing");
}

#[test]
fn flatten_scale_halves_dimensions() {
    let fish = fixture(FISH);
    let full = tmp("cli_s1.jpg");
    let half = tmp("cli_s05.jpg");
    run(&["flatten", path(&fish), "-f", "15", "-c", "1", "-s", "1.0", "-o", path(&full)]);
    run(&["flatten", path(&fish), "-f", "15", "-c", "1", "-s", "0.5", "-o", path(&half)]);
    let (fw, fh) = dims(&full);
    let (hw, hh) = dims(&half);
    assert!((hw as i64 - (fw / 2) as i64).abs() <= 2, "width: {hw} vs {}/2", fw);
    assert!((hh as i64 - (fh / 2) as i64).abs() <= 2, "height: {hh} vs {}/2", fh);
}

#[test]
fn flatten_quality_affects_file_size() {
    let fish = fixture(FISH);
    let hi = tmp("cli_q95.jpg");
    let lo = tmp("cli_q20.jpg");
    run(&["flatten", path(&fish), "-f", "15", "-c", "1", "-q", "95", "-o", path(&hi)]);
    run(&["flatten", path(&fish), "-f", "15", "-c", "1", "-q", "20", "-o", path(&lo)]);
    assert!(file_len(&lo) < file_len(&hi), "lower quality should be smaller: {} vs {}", file_len(&lo), file_len(&hi));
}

#[test]
fn flatten_circle_radius_override_is_accepted() {
    let fish = fixture(FISH);
    let out = tmp("cli_r.jpg");
    run(&["flatten", path(&fish), "-f", "8", "-c", "1", "-r", "300", "-o", path(&out)]);
    let (w, h) = dims(&out);
    assert!(w > 0 && h > 0);
}

#[test]
fn flatten_default_output_is_written_next_to_input() {
    // Regression: `defish dir/photo.jpg` must write dir/photo_defish.jpg, not
    // photo_defish.jpg in the current directory.
    let dir = tmp("cli_subdir");
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("photo.jpg");
    std::fs::copy(fixture(FISH), &input).unwrap();
    let expected = dir.join("photo_defish.jpg");
    let _ = std::fs::remove_file(&expected);

    run(&["flatten", path(&input), "-f", "15", "-c", "1"]);

    assert!(expected.exists(), "default output should be next to input: {}", expected.display());
}

#[test]
fn calibrate_reports_scale_without_writing_an_image() {
    // Make a circular fisheye, then calibrate against it.
    let checker = fixture(CHECKER);
    let circ = tmp("cli_circ.jpg");
    run(&["refish", path(&checker), "--source-fov", "100", "--fov", "100", "--size", "512", "-o", path(&circ)]);

    let would_write = tmp("cli_circ_defish.jpg");
    let _ = std::fs::remove_file(&would_write);

    let out = run(&["flatten", path(&circ), "--calibrate", "10", "-c", "1"]);
    assert!(out.contains("calib-scale"), "calibrate should print a recommended --calib-scale:\n{out}");
    assert!(!would_write.exists(), "calibrate must not write an output image");
}

// ------------------------------------------------------------------ refish ---

#[test]
fn refish_produces_a_circular_image() {
    let checker = fixture(CHECKER);
    let circ = tmp("cli_refish.jpg");
    run(&["refish", path(&checker), "--source-fov", "100", "--fov", "100", "--size", "400", "-o", path(&circ)]);
    assert_eq!(dims(&circ), (400, 400), "output should be a square of the requested size");
    let img = load(&circ);
    assert!(black_fraction(&img) > 0.1, "a circular fisheye has black corners");
    assert!(luma(img.get_pixel(200, 200)) > 15.0, "the center should contain image content");
}

#[test]
fn refish_wider_fov_packs_into_a_smaller_disc() {
    let checker = fixture(CHECKER);
    let narrow = tmp("cli_fov90.jpg");
    let wide = tmp("cli_fov180.jpg");
    run(&["refish", path(&checker), "--source-fov", "100", "--fov", "90", "--size", "400", "-o", path(&narrow)]);
    run(&["refish", path(&checker), "--source-fov", "100", "--fov", "180", "--size", "400", "-o", path(&wide)]);
    assert!(
        black_fraction(&load(&wide)) > black_fraction(&load(&narrow)),
        "a wider output FoV squeezes the source into a smaller disc → more black"
    );
}

// ------------------------------------------------------------------ tunnel ---

#[test]
fn tunnel_fills_the_frame() {
    let checker = fixture(CHECKER);
    let out = tmp("cli_tunnel.jpg");
    run(&["tunnel", path(&checker), "-k", "1.5", "-o", path(&out)]);
    assert_eq!(dims(&out), dims(&checker), "tunnel keeps the source dimensions");
    assert!(black_fraction(&load(&out)) < 0.02, "tunnel fills the frame — no black borders");
}

#[test]
fn tunnel_accepts_negative_strength() {
    // Also exercises clap's negative-number handling (`-k -0.5`).
    let checker = fixture(CHECKER);
    let out = tmp("cli_bulge.jpg");
    run(&["tunnel", path(&checker), "-k", "-0.5", "-o", path(&out)]);
    assert_eq!(dims(&out), dims(&checker));
}

// ----------------------------------------------------------------- animate ---

#[test]
fn animate_frames_writes_a_progressing_sequence() {
    let fish = fixture(FISH);
    let base = tmp("cli_anim.png");
    run(&["animate", path(&fish), "-f", "15", "-c", "1", "--frames", "--steps", "5", "--max-width", "300", "-o", path(&base)]);
    for i in 0..5 {
        assert!(tmp(&format!("cli_anim_{i:04}.png")).exists(), "missing frame {i}");
    }
    let first = load(&tmp("cli_anim_0000.png"));
    let last = load(&tmp("cli_anim_0004.png"));
    assert!(mean_abs_diff(&first, &last) > 2.0, "frames should change over the animation");
}

#[test]
fn animate_show_crop_starts_on_the_full_frame() {
    let fish = fixture(FISH);
    let base = tmp("cli_sc.png");
    run(&["animate", path(&fish), "-f", "15", "-c", "1", "--frames", "--steps", "4", "--show-crop", "--max-width", "300", "-o", path(&base)]);
    let first = load(&tmp("cli_sc_0000.png"));
    let last = load(&tmp("cli_sc_0003.png"));
    // The full frame letterboxed into the wide canvas → noticeable black margins
    // that shrink as it crops to the strip.
    assert!(black_fraction(&first) > 0.1, "show-crop should start letterboxed");
    assert!(black_fraction(&first) > black_fraction(&last), "letterbox should shrink toward the strip");
}

// --------------------------------------------------------------- CLI shape ---

#[test]
fn help_lists_all_subcommands() {
    let out = run(&["--help"]);
    for sub in ["flatten", "refish", "tunnel", "animate"] {
        assert!(out.contains(sub), "--help should list `{sub}`:\n{out}");
    }
}

#[test]
fn bare_image_arg_defaults_to_flatten() {
    let fish = fixture(FISH);
    let out = tmp("cli_default.jpg");
    // No subcommand → the default-subcommand shim runs `flatten`.
    run(&[path(&fish), "-f", "15", "-c", "1", "-o", path(&out)]);
    let (w, h) = dims(&out);
    assert!(w > h, "default flatten should produce a wide strip");
}

#[test]
fn errors_on_missing_input_and_bad_projection() {
    run_fail(&["flatten", "/no/such/file.jpg", "-f", "15"]);
    let fish = fixture(FISH);
    run_fail(&["flatten", path(&fish), "--projection", "banana", "-f", "15"]);
}
