// Exporte le logo en PNG (fond transparent) : node export.js
// Version détaillée (fox.svg) à partir de 48 px, simplifiée (fox-small.svg) en dessous.
const { chromium } = require('playwright');
const fs = require('fs');
(async () => {
  const b = await chromium.launch();
  for (const s of [512, 256, 128, 64, 48, 32, 24, 16]) {
    const svg = fs.readFileSync(s >= 48 ? 'fox.svg' : 'fox-small.svg', 'utf8');
    const p = await b.newPage({ viewport: { width: s, height: s }, deviceScaleFactor: 1 });
    await p.setContent(`<html><body style="margin:0;background:transparent"><img src="data:image/svg+xml;base64,${Buffer.from(svg).toString('base64')}" width="${s}" height="${s}" style="display:block"></body></html>`);
    await p.waitForTimeout(100);
    await p.screenshot({ path: `fox-${s}.png`, omitBackground: true });
    await p.close();
  }
  await b.close();
})();
