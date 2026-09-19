import { render } from 'solid-js/web';
import { initAuth } from '@/lib/auth';
import { initTheme } from '@/lib/theme';
import App from './App';
import './index.css';

const root = document.getElementById('app');
if (root) {
  // Before anything fetches: the control API needs the token this page was opened with.
  initAuth();
  initTheme();

render(() => <App />, root);
}
