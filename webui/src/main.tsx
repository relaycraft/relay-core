import { render } from 'solid-js/web';
import App from './App';
import './index.css';

const root = document.getElementById('app');
if (root) {
  render(() => <App />, root);
}
