# fisheye-flatten

Rust CLI tool that crops the vertical center strip of a Laowa 8-15mm fisheye photo and remaps it to rectilinear projection — producing a natural-looking wide panoramic strip.

Designed for the **Laowa 8-15mm f/2.8 fisheye** on any sensor. Optimised for the **Fuji GFX 100S II** (100MP medium format) and **Nikon Z8**. Focal length and camera body are read from EXIF automatically.

## What it does

The fisheye packs edge content too close to the center (barrel distortion). The tool remaps each output pixel through the inverse equidistant→rectilinear transform, so edges stretch outward to their correct proportions.

The output is **wider than the input** — this is correct, not a bug. Rectilinear projection allocates more pixels per degree at the center than at the edges, so preserving center scale requires expanding the output width. Height scales proportionally so pixels remain square.

| Focal length | Output width expansion |
|---|---|
| 15mm | +13% |
| 12mm | +26% |
| 10mm | +46% |
| 8mm  | +183% |

## Usage

```bash
defish photo.jpg                     # auto everything, 30% strip
defish photo.jpg -p 0.5              # 50% strip height
defish photo.jpg -p 0.5 -s 0.5      # 50% strip, half-resolution output
defish photo.jpg -f 8 -c 0.79       # GFX 100S II at 8mm (override EXIF)
defish photo.jpg -v                  # print geometry details
```

## Options

| Flag | Default | Description |
|---|---|---|
| `-p` | `0.30` | Strip height as fraction of frame (0–1) |
| `-f` | EXIF | Focal length in mm |
| `-c` | EXIF model | Crop factor vs full-frame (GFX=0.79, Z8=1.0) |
| `-s` | `1.0` | Output scale multiplier |
| `-o` | auto | Output path (default: `{stem}_defish.{ext}`) |
| `-v` | off | Verbose geometry output |

## Build

```bash
git clone https://github.com/asah/fisheye-flatten.git
cd fisheye-flatten
cargo build --release
# binary: target/release/defish
```

Requires Rust ≥ 1.75 stable. Uses Rayon for parallel processing.
