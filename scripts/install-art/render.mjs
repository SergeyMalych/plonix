#!/usr/bin/env node
// Draws the pictures of the install guide (docs/install.md) from scenes.html:
// each scene in light and dark, written to site/assets/install-<scene>-<theme>.webp.
//
//   npm i --no-save playwright && node scripts/install-art/render.mjs
//
// Needs Playwright's Chromium, and Python with Pillow to write WebP.

import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import pw from 'playwright';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const OUT = path.resolve(HERE, '../../site/assets');
const SCENES = ['mac-1', 'mac-2', 'mac-3', 'mac-4', 'mac-terminal', 'win-1', 'win-2'];

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'install-art-'));
const browser = await pw.chromium.launch();
const page = await browser.newPage({ viewport: { width: 960, height: 540 }, deviceScaleFactor: 2 });
for (const scene of SCENES) {
  for (const theme of ['light', 'dark']) {
    await page.goto(`file://${HERE}/scenes.html?scene=${scene}&theme=${theme}`);
    await page.waitForSelector('body[data-ready]');
    await page.locator('#stage').screenshot({ path: path.join(tmp, `install-${scene}-${theme}.png`) });
  }
}
await browser.close();
execFileSync('python3', ['-c', `
import glob, os, sys
from PIL import Image
for f in glob.glob(os.path.join(sys.argv[1], '*.png')):
    Image.open(f).convert('RGB').save(os.path.join(sys.argv[2], os.path.basename(f)[:-4] + '.webp'), 'WEBP', quality=86, method=6)
`, tmp, OUT]);
console.log(`wrote ${SCENES.length * 2} pictures to ${path.relative(process.cwd(), OUT)}`);
