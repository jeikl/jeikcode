import { defineConfig } from 'vite';
import preact from '@preact/preset-vite';
import fs from 'node:fs';
import path from 'node:path';

function getAppVersion(): string {
  try {
    const cargoTomlPath = path.resolve(__dirname, '../Cargo.toml');
    const tomlContent = fs.readFileSync(cargoTomlPath, 'utf8');
    const match = tomlContent.match(/\[workspace\.package\][\s\S]*?version\s*=\s*"([^"]+)"/);
    if (match && match[1]) {
      return match[1];
    }
  } catch {}
  return '6.0.30';
}

export default defineConfig({
  plugins: [preact()],
  base: './',
  define: {
    __APP_VERSION__: JSON.stringify(getAppVersion()),
  },
  build: { outDir: 'dist', emptyOutDir: true },
  server: { port: 5173 },
});
