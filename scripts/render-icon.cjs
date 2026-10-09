// Development-only asset renderer. App builds use the checked-in outputs.
const fs = require('node:fs');
const path = require('node:path');
const {execFileSync} = require('node:child_process');
const {Resvg} = require('@resvg/resvg-js');
process.chdir(path.join(__dirname, '..'));
const svg = fs.readFileSync('assets/icon.svg');
fs.writeFileSync('assets/icon.png', new Resvg(svg, {fitTo: {mode: 'width', value: 1024}}).render().asPng());
execFileSync('magick', ['assets/icon.png', '-define', 'icon:auto-resize=256,128,64,48,32,16', 'assets/icon.ico']);
// Rasterize each UI size from the vector, avoiding repeated bitmap resampling.
for (const size of [64, 256]) {
  const png = new Resvg(svg, {fitTo: {mode: 'width', value: size}}).render().asPng();
  execFileSync('magick', ['png:-', '-depth', '8', `RGBA:assets/icon_${size}.rgba`], {input: png});
}
