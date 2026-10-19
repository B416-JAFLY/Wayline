#!/usr/bin/env node
// Regenerate the desktop icon assets from the approved SVG geometry.
// Requires sharp (resolve normally, via OMDROP_SHARP_MODULE, or from Codex's
// bundled runtime). Run from any directory: node scripts/generate-desktop-icons.cjs
const fs = require('node:fs/promises');
const path = require('node:path');
const os = require('node:os');
const assert = require('node:assert/strict');

function loadSharp() {
  if (process.env.OMDROP_SHARP_MODULE) return require(process.env.OMDROP_SHARP_MODULE);
  try {
    return require('sharp');
  } catch (error) {
    if (error.code !== 'MODULE_NOT_FOUND') throw error;
    return require(path.join(os.homedir(), '.cache', 'codex-runtimes',
      'codex-primary-runtime', 'dependencies', 'node', 'node_modules', 'sharp'));
  }
}

const sharp = loadSharp();
const root = path.resolve(__dirname, '..');
const assets = path.join(root, 'apps', 'desktop', 'assets');
const sourcePath = path.join(root, 'assets', 'branding', 'omdrop-mark.svg');
const previewPath = path.join(root, 'build', 'desktop-icons-preview.png');
const appSizes = [16, 20, 24, 32, 40, 48, 64, 128, 256];
const traySizes = [16, 20, 24, 32, 40, 48, 64];
const offlineColors = { upper: '#B5B5B5', lower: '#7A7A7A' };

async function raster(svg, size) {
  // Supersample from vector geometry at every target size; never scale an ICO
  // frame from a smaller raster. The transparent square has 44 px side padding.
  const rgba = await sharp(Buffer.from(svg), { density: 72 * size * 4 / 512 })
    .resize(size, size, { kernel: sharp.kernel.lanczos3 })
    .ensureAlpha().raw().toBuffer();
  assert.equal(rgba.length, size * size * 4);
  let transparent = 0;
  let solid = 0;
  let partial = 0;
  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      const alpha = rgba[(y * size + x) * 4 + 3];
      if (alpha === 0) transparent++;
      else if (alpha === 255) solid++;
      else partial++;
      if (x === 0 || y === 0 || x === size - 1 || y === size - 1) {
        assert.equal(alpha, 0, `Icon touches the ${size}px canvas boundary`);
      }
    }
  }
  assert(transparent > 0 && solid > 0 && partial > 0, `Invalid alpha coverage at ${size}px`);
  const png = await sharp(rgba, { raw: { width: size, height: size, channels: 4 } })
    .png({ compressionLevel: 9 }).toBuffer();
  return { size, rgba, png, alpha: { transparent, solid, partial } };
}

function dib(frame) {
  const { size, rgba } = frame;
  const xorSize = size * size * 4;
  const maskStride = Math.ceil(size / 32) * 4;
  const andSize = maskStride * size;
  const data = Buffer.alloc(40 + xorSize + andSize);
  data.writeUInt32LE(40, 0); // BITMAPINFOHEADER
  data.writeInt32LE(size, 4);
  data.writeInt32LE(size * 2, 8); // XOR bitmap followed by its AND mask.
  data.writeUInt16LE(1, 12);
  data.writeUInt16LE(32, 14);
  data.writeUInt32LE(xorSize + andSize, 20);
  for (let y = 0; y < size; y++) {
    const bottomUp = size - y - 1;
    for (let x = 0; x < size; x++) {
      const input = (y * size + x) * 4;
      const output = 40 + (bottomUp * size + x) * 4;
      data[output] = rgba[input + 2];
      data[output + 1] = rgba[input + 1];
      data[output + 2] = rgba[input];
      data[output + 3] = rgba[input + 3];
      if (rgba[input + 3] === 0) {
        data[40 + xorSize + bottomUp * maskStride + Math.floor(x / 8)] |= 0x80 >> (x % 8);
      }
    }
  }
  return data;
}

function ico(frames) {
  const header = Buffer.alloc(6 + frames.length * 16);
  header.writeUInt16LE(1, 2);
  header.writeUInt16LE(frames.length, 4);
  let offset = header.length;
  const payloads = frames.map((frame, index) => {
    const payload = frame.size === 256 ? frame.png : dib(frame);
    const entry = 6 + index * 16;
    header[entry] = frame.size === 256 ? 0 : frame.size;
    header[entry + 1] = header[entry];
    header.writeUInt16LE(1, entry + 4);
    header.writeUInt16LE(32, entry + 6);
    header.writeUInt32LE(payload.length, entry + 8);
    header.writeUInt32LE(offset, entry + 12);
    offset += payload.length;
    return payload;
  });
  return Buffer.concat([header, ...payloads]);
}

async function preview(connected, disconnected) {
  const width = 1280;
  const height = 700;
  const displayedSizes = [16, 20, 24, 32];
  const labels = [];
  const layers = [];
  for (const [theme, top, background, foreground] of [
    ['LIGHT TASKBAR', 96, '#FFFFFF', '#242426'],
    ['DARK TASKBAR', 400, '#1C1C1F', '#E6E6EA'],
  ]) {
    labels.push(`<rect x="16" y="${top - 24}" width="1248" height="296" rx="12" fill="${background}"/>`);
    labels.push(`<text x="32" y="${top - 3}" fill="${foreground}" font-size="13">${theme}</text>`);
    for (const [column, size] of displayedSizes.entries()) {
      const left = 24 + column * 308;
      labels.push(`<text x="${left + 14}" y="${top + 22}" fill="${foreground}" font-size="16">${size} px</text>`);
      for (const [state, frames, x] of [
        ['CONNECTED', connected, left + 14],
        ['DISCONNECTED', disconnected, left + 166],
      ]) {
        const frame = frames.get(size);
        layers.push({ input: frame.png, left: x + Math.floor((128 - size) / 2), top: top + 40 });
        const enlarged = await sharp(frame.png).resize(128, 128, { kernel: sharp.kernel.nearest }).png().toBuffer();
        layers.push({ input: enlarged, left: x, top: top + 98 });
        labels.push(`<text x="${x}" y="${top + 249}" fill="${foreground}" font-size="12">${state}</text>`);
      }
    }
  }
  const background = `<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${height}">
    <rect width="${width}" height="${height}" fill="#E9ECF0"/>
    <g font-family="Segoe UI, Arial, sans-serif"><text x="32" y="35" font-size="22" fill="#242426">Omdrop desktop icons</text>
    <text x="32" y="56" font-size="13" fill="#596170">Actual size above; nearest-neighbor enlargement below. Transparent artwork on light and dark backgrounds.</text>
    ${labels.join('\n')}</g></svg>`;
  await sharp(Buffer.from(background)).composite(layers).png().toFile(previewPath);
}

async function main() {
  const source = await fs.readFile(sourcePath, 'utf8');
  assert.equal((source.match(/<path\b/g) || []).length, 1, 'The mark must have one master path');
  assert.equal((source.match(/<use\b/g) || []).length, 2, 'Both pieces must reuse the master path');
  assert(source.includes('transform="rotate(180 544 354)"'));
  assert(source.includes('viewBox="288 176 512 356"'));
  const svg = source.replace('viewBox="288 176 512 356"', 'viewBox="288 98 512 512"')
    .replace('Transparent background.', 'Transparent square application icon with equal side padding.');
  const graySvg = svg.replace('fill="#65C7FF"', `fill="${offlineColors.upper}"`)
    .replace('fill="#0A84FF"', `fill="${offlineColors.lower}"`);
  assert.notEqual(svg, graySvg);
  const connected = new Map();
  const disconnected = new Map();
  for (const size of appSizes) connected.set(size, await raster(svg, size));
  for (const size of traySizes) {
    const gray = await raster(graySvg, size);
    const blue = connected.get(size);
    for (let offset = 3; offset < gray.rgba.length; offset += 4) {
      assert.equal(gray.rgba[offset], blue.rgba[offset], `State geometry differs at ${size}px`);
    }
    disconnected.set(size, gray);
  }
  await fs.mkdir(assets, { recursive: true });
  await fs.mkdir(path.dirname(previewPath), { recursive: true });
  await fs.writeFile(path.join(assets, 'icon.svg'), svg);
  await fs.writeFile(path.join(assets, 'icon.png'), (await raster(svg, 512)).png);
  await fs.writeFile(path.join(assets, 'icon.ico'), ico(appSizes.map(size => connected.get(size))));
  await fs.writeFile(path.join(assets, 'tray-connected.ico'), ico(traySizes.map(size => connected.get(size))));
  await fs.writeFile(path.join(assets, 'tray-disconnected.ico'), ico(traySizes.map(size => disconnected.get(size))));
  await preview(connected, disconnected);
  console.log(JSON.stringify({
    source: path.relative(root, sourcePath),
    assets: path.relative(root, assets),
    viewBox: [288, 98, 512, 512],
    appSizes,
    traySizes,
    offlineColors,
    alpha: Object.fromEntries([...connected].map(([size, frame]) => [size, frame.alpha])),
    preview: path.relative(root, previewPath),
  }, null, 2));
}

main().catch(error => {
  console.error(error);
  process.exitCode = 1;
});
