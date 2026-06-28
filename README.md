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

## Commands

`defish` is structured as a small toolkit with subcommands:

| Command | Description |
|---|---|
| `flatten` | Fisheye → cylindrical/rectilinear strip (the default) |
| `refish`  | Rectilinear → circular fisheye (the inverse of `flatten`) |
| `tunnel`  | Radial warp: push the center back ("tunnel") or bulge it forward |
| `animate` | Render N frames morphing fisheye → flat, to video or an image sequence |

`flatten` is the **default subcommand**, so `defish photo.jpg ...` is shorthand
for `defish flatten photo.jpg ...` — existing commands keep working unchanged.
All subcommands share one remap engine and lens model (`src/lib.rs`).

### refish — make a photo look like a fisheye

```bash
defish refish photo.jpg                  # fill the circle with the source's own FoV
defish refish photo.jpg --fov 180        # exaggerate to a 180° fisheye look
defish refish photo.jpg --source-fov 75  # source lacks usable EXIF
```

Straight lines bow outward and the frame wraps into a disc (black outside the
circle). You can only render the field of view the source actually contains — a
70° photo fills the circle to 70°, with black beyond. The source FoV comes from
EXIF focal length + body, or `--source-fov`. Output is a square; `--size` sets
the diameter (default: the shorter source side).

### tunnel — center recedes, edges magnify

```bash
defish tunnel photo.jpg          # default tunnel (strength 1.0)
defish tunnel photo.jpg -k 2.5   # stronger
defish tunnel photo.jpg -k -0.5  # bulge the center forward instead
```

A pure radial warp about the center, on any image (fisheye or not). Positive
`-k/--strength` compresses the center and magnifies the edges (the center looks
further away); negative bulges. The frame is filled edge-to-edge — no black
borders.

### animate — watch the flattening happen

```bash
defish animate photo.jpg --steps 60                 # → photo_anim.mp4 (H.264)
defish animate photo.jpg --steps 60 -o unroll.mov   # ProRes
defish animate photo.jpg --steps 60 --pix-fmt yuv444p   # no chroma subsampling
defish animate photo.jpg --steps 60 --frames        # numbered PNG sequence
defish animate photo.jpg --steps 60 --show-crop     # start on the full frame
```

Renders `--steps N` frames that morph from the original fisheye (frame 0) to the
fully-flattened panorama (last frame), anchored at the center so the edges
"unroll". Takes every `flatten` option (`-f`, `-p`, `--projection`, `-s`, …).

- **Output** is chosen by the `-o` extension: `.mp4`/`.mkv`→H.264, `.mov`→ProRes,
  `.webm`→VP9. `--frames` writes a numbered image sequence instead.
- **ffmpeg** is required for video (frames are streamed to it over a pipe — no
  temp files). Install it (`brew install ffmpeg`). If it's missing, `animate`
  falls back to writing a numbered sequence and prints the ffmpeg command to
  assemble it.
- **No GIF** — its 256-color palette wrecks photographic gradients. The codecs
  above give full color (and 10-bit via ProRes/`--pix-fmt`).
- `--steps N`, `--fps` (default 30; the **animation speed** — morph frames shown
  per second), `--crf` (default 16; lower = better/larger), `--max-width`
  (default 1920; caps frame size so videos stay sane; 0 disables).
- The video is always encoded at a **constant 30fps** with `+faststart` for wide
  player compatibility — at low `--fps` each morph frame is simply held for
  several video frames (perceived speed and duration are unchanged). This avoids
  finicky players (QuickTime/Preview) refusing to animate very-low-fps clips.
- `--show-crop` starts on the **whole input frame** so the animation tells the
  full story (full photo → flat). `--crop-style simul` (default) crops/zooms and
  unrolls in one fluid motion; `--crop-style phased` does the crop first, then
  the unroll (two distinct phases, with `--crop-frac` setting the crop phase's
  share, default 0.4). Without `--show-crop`, the animation starts already
  cropped to the strip.

## Usage

```bash
defish photo.jpg                     # auto everything (= defish flatten photo.jpg)
defish flatten photo.jpg             # explicit form, identical result
defish photo.jpg -p 0.5              # 50% strip height
defish photo.jpg -p 0.5 -s 0.5      # 50% strip, half-resolution output
defish photo.jpg -q 100             # max-quality JPEG (larger file)
defish photo.jpg -f 8 -c 0.79       # GFX 100S II at 8mm (override EXIF)
defish photo.jpg -v                  # print geometry details
```

## Options

| Flag | Default | Description |
|---|---|---|
| `-p` | `0.50` | Vertical coverage as a fraction (`0.30`) or percent (`30`) — both mean 30% |
| `-f` | EXIF / auto | Focal length in mm (see auto-detection below) |
| `-c` | EXIF model | Crop factor vs full-frame (GFX=0.79, Z8=1.0) |
| `--calibrate` | — | Print recommended `--calib-scale` for a shot of known focal length, then exit |
| `--calib-scale` | `1.0` | Correction factor for the circle→focal-length model |
| `-s` | `1.0` | Output scale multiplier |
| `-q` | `95` | JPEG output quality, 1–100 (ignored for non-JPEG) |
| `-o` | auto | Output path (default: `{stem}_defish.{ext}`) |
| `-v` | off | Verbose geometry output |

## Focal-length auto-detection

The Laowa 8-15mm is a fully manual lens with no electronic contacts on most
mounts, so it writes **no focal length to EXIF**. When neither `-f` nor EXIF
supplies one, the tool recovers it from the **image circle**:

```
r_circle_px  =  (focal_mm / pixel_pitch_mm) · θ_max(focal)
```

The pixel pitch comes from the body (EXIF `FocalPlaneXResolution`, or a
per-model sensor-width lookup), and the circle radius is measured directly. The
relation is inverted to solve for the focal length. The radius is found from:

1. **Full circle** — black border on all four sides (rays from the center to a
   consistent radius).
2. **Top/bottom cropped, sides fall off to black** — the image circle is taller
   than the sensor but narrower than it. The center row still spans the full
   diameter, so the radius is measured from the **horizontal extent** (the
   black→image boundary on the left and right, required to be symmetric).

If neither holds (a full-frame fisheye whose circle overflows the frame on all
sides), the signal is gone — pass `-f`.

Because a large sensor can make `r_circle(focal)` non-monotonic, the detector
reports when a radius matches more than one focal length and asks you to confirm
with `-f`.

**Aperture is not auto-detected** — a single frame carries no clean, invertible
aperture signal, and aperture has no effect on the defish geometry anyway.

### Calibration (optional, improves accuracy)

The bare equidistant model can be a few percent off. Shoot one frame at a known
focal length and let the tool fit a correction:

```bash
$ defish ref_8mm.JPG --calibrate 8
Calibration @ 8.0mm:
  measured circle r = 3360px
  model    circle r = 3343px  (pitch 3.76µm, crop 0.790×)
  → re-run with  --calib-scale 1.0051

$ defish photo.JPG --calib-scale 1.0051   # now auto-detect is calibrated
```

## Embedded settings (EXIF)

Every JPEG output gets the effective processing settings written into its EXIF so
they travel with the file:

- **ImageDescription** (`0x010E`) — a human-readable summary (projection, focal
  length, crop factor, FF-equiv, lens type, strip %, scale, angular coverage,
  output size, source file). This is the field **Google Photos** shows as the
  photo's description in its info panel.
- **Software** (`0x0131`) — `defish (fisheye-flatten) vX.Y.Z`, the standard
  "processed by" field.

Both are common, human-visible fields (not maker notes), and the text is ASCII so
it renders cleanly everywhere. Non-JPEG outputs (PNG/TIFF) skip this step.

```
$ defish DSCF1857.JPG
$ exiftool -ImageDescription DSCF1857_defish.JPG
Image Description : defish (fisheye-flatten v0.1.0): projection=rectilinear, focal=8.0mm, ...
```

## Build

```bash
git clone https://github.com/asah/fisheye-flatten.git
cd fisheye-flatten
cargo build --release
# binary: target/release/defish
```

Requires Rust ≥ 1.75 stable. Uses Rayon for parallel processing.
