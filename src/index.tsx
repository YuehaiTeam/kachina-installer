import { render } from 'preact';
import { App } from './App';
import { startPluginHost } from './plugin-host';
import './index.css';
import './layout.css';

const pluginHost = new URLSearchParams(window.location.search).has('pluginHost');

if (pluginHost) {
  void startPluginHost();
} else {
  const root = document.getElementById('root');
  if (root) {
    render(<App />, root);
  }

  if (process.env.NODE_ENV !== 'development') {
    window.addEventListener('contextmenu', (e) => {
      e.preventDefault();
    });
    document.addEventListener('keydown', (event) => {
      if (
        event.key === 'F5' ||
        (event.ctrlKey && event.key === 'r') ||
        (event.metaKey && event.key === 'r')
      ) {
        event.preventDefault();
      }
    });
  }
}
