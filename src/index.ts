import { createApp } from 'vue';
import App from './App.vue';
import './index.css';

createApp(App).mount('#root');

if (process.env.NODE_ENV !== 'development') {
  window.addEventListener('contextmenu', (e) => {
    e.preventDefault();
  });
  document.addEventListener('keydown', function (event) {
    // Prevent F5 or Ctrl+R (Windows/Linux) and Command+R (Mac) from refreshing the page
    if (
      event.key === 'F5' ||
      (event.ctrlKey && event.key === 'r') ||
      (event.metaKey && event.key === 'r')
    ) {
      event.preventDefault();
    }
  });
}
