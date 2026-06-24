# fisheye-flatten

A Rust CLI tool that crops the vertical-center strip of a fisheye photo and applies a **graduated, center-preserving remap** to correct horizontal barrel distortion.

Optimised for the **Laowa 8-15mm f/2.8 fisheye** on any sensor — including the Fuji GFX 100S II (100MP medium format) and Nikon Z8. Focal length and camera body are auto-detected from EXIF.

## The idea

Fisheye lenses compress the center and stretch the edges. A naive full rectilinear remap fixes the edges but squishes the center. Instead, this tool blends:

- **Center columns**: identity (pixel taken straight from same position in source)
- **Edge columns**: full equidistant→rectilinear correction
- **In between**: smooth power-curve blend controlled by `--blend-power`

```
blend weight = edge_strength × |x_norm|^blend_power

x_norm = -1 (left edge)    weight = edge_strength  → full correction
x_norm =  0 (center)       weight = 0              → identity, no change
x_norm = +1 (right edge)   weight = edge_strength  → full correction
```

Vertical rows within the strip are passed through unchanged (no vertical warp).

## Usage

```bash
# Auto-detect focal length + camera from EXIF:
./defish photo.jpg

# Explicit 30% strip, verbose:
./defish photo.jpg -p 0.30 -v

# GFX 100S II, 15mm:
./defish DSCF1234.JPG -o output.jpg -p 0.30 -f 15 -c 0.79 -v

# Tune the blend:
./defish photo.jpg -p 0.30 -b 2.5 -e 0.9   # very center-preserving
./defish photo.jpg -p 0.30 -b 1.5 -e 1.0   # more aggressive correction
```

## Options

| Flag | Default | Description |
|------|---------|-------------|
| `-p` / `--strip-percent` | `0.30` | Fraction of frame height to keep (0..1) |
| `-b` / `--blend-power` | `2.0` | How fast edge correction ramps in. Higher = more center-preserving |
| `-e` / `--edge-strength` | `1.0` | Max correction at far edge (0=none, 1=full rectilinear) |
| `-f` / `--focal-length` | EXIF | Focal length in mm |
| `-c` / `--crop-factor` | EXIF model | Sensor crop factor vs full-frame |
| `-s` / `--scale` | `1.0` | Output scale multiplier |
| `-i` / `--interp` | `bilinear` | Interpolation: `nearest` or `bilinear` |
| `-o` / `--output` | auto | Output path (default: `{input}_defish.jpg`) |
| `-v` / `--verbose` | off | Print geometry details |

## Blend power guide

| `--blend-power` | Character |
|----------------|-----------|
| `1.5` | Linear-ish, noticeable correction even near center |
| `2.0` | Smooth quadratic — good default |
| `2.5` | Correction stays subtle until ~60% from center |
| `3.0` | Very flat center, strong snap at outer 30% |

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

| Focal length | Half diagonal AoV (FF) |
|-------------|----------------------|
| 8mm | 90° (180° total) |
| 10mm | 65° |
| 12mm | 53° |
| 15mm | 40° (80° total) |

GFX 100S II (crop 0.79): 15mm on GFX = 11.85mm FF-equivalent → wider AoV lookup.

## Build

```bash
git clone https://github.com/asah/fisheye-flatten.git
cd fisheye-flatten
cargo build --release
# Binary: target/release/defish
```

Requires Rust ≥ 1.75 stable. Uses Rayon for parallel row processing.
