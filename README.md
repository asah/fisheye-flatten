# fisheye-flatten

A Rust CLI tool that crops the vertical-center strip of a fisheye photo and remaps it to rectilinear projection.

Optimised for the **Laowa 8-15mm f/2.8 fisheye** on any sensor — including the Fuji GFX 100S II (100MP medium format) and Nikon Z8. Focal length and camera body are auto-detected from EXIF.

## The idea

Fisheye lenses distort heavily toward the edges. If you only care about a horizontal panoramic strip through the center of the frame, you can:

1. **Crop** the vertical center (e.g. 30% of frame height)
2. **Remap** it from equidistant fisheye projection → rectilinear (perspective) projection

This trades center resolution for edge consistency — straight horizontal lines become straight, and spatial density is uniform across the frame.

```
Input (fisheye):          Output (rectilinear strip):
╔══════════════╗          ┌──────────────────────────┐
║   ~distorted ║          │  straight, uniform 30%   │
║  ┌──────────┐║   →      │  of original frame       │
║  │  center  │║          └──────────────────────────┘
║  └──────────┘║
║              ║
╚══════════════╝
```

## Usage

```bash
# Auto-detect focal length + camera from EXIF:
./defish photo.jpg

# Explicit 30% strip, verbose:
./defish photo.jpg -p 0.30 -v

# GFX 100S II, 15mm, output to specific file:
./defish DSCF1234.JPG -o output.jpg -p 0.30 -v

# Scale output up (e.g. for 100MP source):
./defish photo.jpg -p 0.30 -s 1.5

# Override focal length and crop factor manually:
./defish photo.jpg -f 12 -c 0.79
```

## Options

| Flag | Default | Description |
|------|---------|-------------|
| `-p` / `--strip-percent` | `0.30` | Fraction of frame height to keep (0..1) |
| `-f` / `--focal-length` | EXIF | Focal length in mm |
| `-c` / `--crop-factor` | EXIF model | Sensor crop factor vs full-frame |
| `-s` / `--scale` | `1.0` | Output scale multiplier |
| `-i` / `--interp` | `bilinear` | Interpolation: `nearest` or `bilinear` |
| `-o` / `--output` | auto | Output path (default: `{input}_defish.jpg`) |
| `-v` / `--verbose` | off | Print geometry details |

## Supported cameras (auto crop-factor detection)

| Camera | Crop factor |
|--------|-------------|
| Fuji GFX (all models) | 0.790 |
| Nikon Z8, Z9, Z6, Z7 | 1.000 |
| Sony A7/A1/A9 series | 1.000 |
| Canon R5/R3/R6 | 1.000 |
| Unknown | 1.000 (warns) |

## Lens model

The Laowa 8-15mm f/2.8 is modelled as an **equidistant fisheye**: `r = f · θ`

Half diagonal AoV table (full-frame equivalent):

| Focal length | Half AoV |
|-------------|----------|
| 8mm | 90° (180° total) |
| 10mm | 65° |
| 12mm | 53° |
| 15mm | 40° (80° total) |

The GFX 100S II sensor (43.8 × 32.9 mm) has a crop factor of ~0.79 relative to full-frame, meaning 15mm on GFX sees the same angle as ~11.85mm on full-frame — the tool accounts for this automatically.

## Build

```bash
git clone https://github.com/asah/fisheye-flatten.git
cd fisheye-flatten
cargo build --release
# Binary: target/release/defish
```

Requires Rust ≥ 1.75 (stable). Uses `rayon` for parallel pixel processing.

## Performance

On a modern multi-core machine, a 100MP GFX image (~11200 × 8400 px) processes in a few seconds thanks to parallel row processing via Rayon.

## Future work

- Lanczos interpolation for highest quality on large files
- 16-bit TIFF pipeline (deps already support it)
- Per-focal-length calibration from shot grids
- Slight equisolid correction at wide end (8-10mm)
