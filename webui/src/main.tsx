import { render } from 'preact';
import { App } from './app';
import { SettingsProvider } from './settings';
import { ensureRandomUUIDPolyfill } from './lib/randomId';
// Bundled serif for the landing greeting (close to claude.ai's display serif).
import '@fontsource/source-serif-4/400.css';
import '@fontsource/source-serif-4/500.css';
import './styles/theme.css';
import './styles/app.css';
import './index.css';

// http://LAN-IP (atomcode serve remote clients) is not a secure context —
// crypto.randomUUID is missing there. Polyfill before any component mounts.
ensureRandomUUIDPolyfill();

render(
  <SettingsProvider>
    <App />
  </SettingsProvider>,
  document.getElementById('app')!,
);
