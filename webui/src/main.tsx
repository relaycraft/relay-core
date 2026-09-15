import { render } from 'solid-js/web';
import { initTheme } from '@/lib/theme';
import App from './App';
import './index.css';

const root = document.getElementById('app');
if (root) {
  initTheme();

render(() => <App />, root);
}
