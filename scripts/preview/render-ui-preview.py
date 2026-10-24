#!/usr/bin/env python3
"""Package ui_preview's actual RGB565 frames as PNG/GIF. Requires Pillow; no font download.

cargo run --manifest-path firmware/Cargo.toml --features cardputer-ui --example ui_preview -- build/cardputer-ui
python scripts/preview/render-ui-preview.py
python scripts/preview/render-ui-preview.py --readme
"""
import argparse
import csv
import json
from pathlib import Path
from PIL import Image, ImageDraw, ImageFont

root = Path(__file__).resolve().parents[2]
frames = root / "build/cardputer-ui"
font = ImageFont.load_default(size=16)
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--readme", action="store_true", help="also update docs/assets/cardputer-demo.gif")
args = parser.parse_args()
def contact_sheet(pages, filename):
    sheet = Image.new("RGB", (1536, 100 + 302 * len(pages)), "#090e18")
    draw = ImageDraw.Draw(sheet)
    draw.text((24, 18), "WAYLINE / CARDPUTER ADV  -  EXACT FIRMWARE RENDERER", font=font, fill="#dce8f8")
    for column, (locale, label) in enumerate([
        ("zh-CN", "SIMPLIFIED CHINESE"), ("zh-TW", "TRADITIONAL CHINESE"), ("en", "ENGLISH")
    ]):
        x = 24 + column * 504
        draw.text((x, 57), label, font=font, fill="#8299b8")
        for row, (page, name) in enumerate(pages):
            y = 90 + row * 302
            image = Image.open(frames / f"{locale}-{page}.ppm")
            image.save(frames / f"{locale}-{page}.png")
            sheet.paste(image.resize((480, 270), Image.Resampling.NEAREST), (x, y))
            draw.text((x, y + 275), name, font=font, fill="#8299b8")
    sheet.save(frames / filename)

def orb_animation(pattern, filename):
    sources = []
    for path in sorted(frames.glob(pattern)):
        with Image.open(path) as image:
            sources.append(image.convert("RGB"))
    if len(sources) < 2:
        raise ValueError(f"Missing animation frames for {filename}; run ui_preview first")
    # One palette for every phase avoids shimmering caused by per-frame palettes.
    width, height = sources[0].size
    samples = Image.new("RGB", (width, height * len(sources)))
    for index, source in enumerate(sources):
        if source.size != (width, height):
            raise ValueError(f"Inconsistent dimensions in {filename}")
        samples.paste(source, (0, height * index))
    palette = samples.quantize(colors=256, method=Image.Quantize.MEDIANCUT)
    animation = [source.quantize(palette=palette, dither=Image.Dither.NONE)
                 .resize((720, 405), Image.Resampling.NEAREST) for source in sources]
    destination = frames / filename
    animation[0].save(destination, save_all=True, append_images=animation[1:],
                      duration=50, loop=0, optimize=True, disposal=1)
    with Image.open(destination) as image:
        duration = 0
        for index in range(image.n_frames):
            image.seek(index)
            duration += image.info.get("duration", 0)
        if duration != len(sources) * 50:
            raise ValueError(f"Animation duration differs from the 20 fps capture: {filename}")
    print(destination)

def readme_demo():
    directory = frames / "readme-demo"
    with (directory / "timeline.tsv").open(encoding="utf-8") as stream:
        timeline = list(csv.DictReader(stream, delimiter="\t"))
    if not timeline or [int(row["elapsed_ms"]) for row in timeline] != list(range(0, 53_000, 50)):
        raise ValueError("Incomplete README timeline; rerun ui_preview")
    sources, durations, snapshots = [], [], {}
    previous = None
    for row in timeline:
        filename = row["file"]
        if Path(filename).name != filename:
            raise ValueError("Unexpected demo frame path")
        with Image.open(directory / filename) as frame:
            image = frame.convert("RGB")
        if image.size != (240, 135):
            raise ValueError("README demo must use the native 240x135 renderer")
        content = image.tobytes()
        if content == previous:
            durations[-1] += 50
        else:
            sources.append(image)
            durations.append(50)
            previous = content
        snapshots[row["stage"]] = image
    # A global palette keeps the UI colors steady. Coalescing identical holds
    # preserves 20 fps timing while avoiding hundreds of redundant full frames.
    samples = Image.new("RGB", (240, 135 * len(sources)))
    for index, source in enumerate(sources):
        samples.paste(source, (0, 135 * index))
    palette = samples.quantize(colors=256, method=Image.Quantize.MEDIANCUT)
    encoded = [source.quantize(palette=palette, dither=Image.Dither.NONE)
               .resize((480, 270), Image.Resampling.NEAREST) for source in sources]
    destination = root / "docs/assets/cardputer-demo.gif"
    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary = directory / "cardputer-demo.gif"
    encoded[0].save(temporary, format="GIF", save_all=True, append_images=encoded[1:],
                    duration=durations, loop=0, optimize=True, disposal=1)
    with Image.open(temporary) as demo:
        if demo.size != (480, 270) or demo.n_frames < 30 or demo.info.get("loop") != 0:
            raise ValueError("Invalid README animation")
        encoded_frames = demo.n_frames
        duration = 0
        for index in range(demo.n_frames):
            demo.seek(index)
            duration += demo.info.get("duration", 0)
        if duration != 53_000:
            raise ValueError("README demo duration changed")
    if temporary.stat().st_size > 8 * 1024 * 1024:
        raise ValueError("README animation exceeds the 8 MiB size budget")
    destination.write_bytes(temporary.read_bytes())
    pages = ["Boot logo", "Waiting for PC", "Connected sphere", "Sharing guide", "Preferences",
             "Sounds and volume", "More", "Connection help", "About", "Reset confirmation",
             "Traditional Chinese", "English"]
    transfers = [stage for stage in snapshots if stage not in pages]
    for stages, filename in [(pages, "pages-contact.png"), (transfers, "transfers-contact.png")]:
        sheet = Image.new("RGB", (1488, 306 * ((len(stages) + 2) // 3)), "#090e18")
        draw = ImageDraw.Draw(sheet)
        for index, stage in enumerate(stages):
            x, y = (index % 3) * 496 + 8, (index // 3) * 306 + 8
            sheet.paste(snapshots[stage].resize((480, 270), Image.Resampling.NEAREST), (x, y))
            draw.text((x, y + 276), stage, font=font, fill="#a0aec2")
        sheet.save(directory / filename)
    report = {"path": str(destination), "source_frames": len(timeline),
              "encoded_frames": encoded_frames, "duration_ms": duration,
              "bytes": destination.stat().st_size, "scenes": list(snapshots)}
    (directory / "verification.json").write_text(json.dumps(report, indent=2), encoding="utf-8")
    print(json.dumps(report))

contact_sheet([(0, "STATUS"), (1, "BEFORE SHARING"), (2, "PREFERENCES"), (3, "MORE")], "cardputer-ui-preview.png")
contact_sheet([("welcome", "WELCOME"), ("help", "CONNECTION HELP"), ("about", "ABOUT"), ("reset", "RESET CONFIRMATION")], "cardputer-ui-features.png")
contact_sheet([("transfer-send", "SENDING"), ("transfer-duplex", "SENDING + RECEIVING"), ("transfer-complete", "CONFIRMED COMPLETE"), ("transfer-interrupted", "INTERRUPTED")], "cardputer-ui-transfer.png")
contact_sheet([("sound", "TASK ALERTS / 50%"), ("sound-muted", "ALERTS OFF / VOLUME RETAINED"), ("sound-max", "100% / CLAMPED"), ("sound-zero", "0% / SILENT")], "cardputer-ui-sound.png")
contact_sheet([("style-soft", "SOFT / SINE"), ("style-8bit", "8-BIT / SQUARE ARPEGGIO"), ("style-chime", "CHIME / HARMONICS"), ("style-minimal", "MINIMAL / SHORT TRIANGLE")], "cardputer-ui-styles.png")
contact_sheet([("orb-offline", "OFFLINE / STILL"), ("orb-connected", "CONNECTED / ROTATING"), ("orb-motion-off", "CONNECTED / MOTION OFF")], "cardputer-orb-states.png")
motion = [Image.open(path).resize((720, 405), Image.Resampling.NEAREST)
          for path in sorted(frames.glob("motion-*.ppm"))]
motion[0].save(frames / "cardputer-ui-motion.gif", save_all=True, append_images=motion[1:], duration=50, loop=0)
motion[0].save(frames / "cardputer-welcome.gif", save_all=True, append_images=motion[1:60], duration=50, loop=0)
orb_animation("orb-[0-9][0-9][0-9].ppm", "cardputer-orb.gif")
orb_animation("orb-transition-[0-9][0-9][0-9].ppm", "cardputer-orb-transition.gif")
print(frames / "cardputer-ui-preview.png")
print(frames / "cardputer-ui-motion.gif")
print(frames / "cardputer-orb-states.png")
if args.readme:
    readme_demo()
