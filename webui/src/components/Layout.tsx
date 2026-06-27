import { onMount, onCleanup } from 'solid-js';
import { store } from '@/lib/store';
import ActivityBar from './ActivityBar';
import StatusBar from './StatusBar';
import CommandPalette from './CommandPalette';
import HelpOverlay from './HelpOverlay';
import Toolbar from './Toolbar';
import FlowsView from '@/views/Flows';
import WorkshopView from '@/views/Workshop';
import RulesView from '@/views/Rules';
import ScriptsView from '@/views/Scripts';
import SettingsView from '@/views/Settings';
import { isEditingTarget } from '@/lib/editing';

export default function Layout() {
  function handleKeyDown(e: KeyboardEvent) {
    const mod = e.metaKey || e.ctrlKey;

    if (mod && (e.key === 'k' || e.key === '/')) {
      e.preventDefault();
      store.setState('showCommandPalette', (v) => !v);
      return;
    }

    if (mod && e.key >= '1' && e.key <= '5') {
      e.preventDefault();
      const views = ['flows', 'workshop', 'rules', 'scripts', 'settings'] as const;
      store.setActiveView(views[Number(e.key) - 1]);
      return;
    }

    if (mod && e.key === ',') {
      e.preventDefault();
      store.setActiveView('settings');
      return;
    }

    if (e.key === '?' && !isEditingTarget(e.target)) {
      e.preventDefault();
      store.setState('showHelp', (v) => !v);
      return;
    }

    if (e.key === 'Escape') {
      if (store.state.showHelp) {
        store.setState('showHelp', false);
      } else if (store.state.showCommandPalette) {
        store.setState('showCommandPalette', false);
      }
    }
  }

  onMount(() => {
    document.addEventListener('keydown', handleKeyDown);
  });

  onCleanup(() => {
    document.removeEventListener('keydown', handleKeyDown);
  });

  return (
    <div class="h-full flex flex-col bg-black text-text select-none">
      <div class="flex-1 flex overflow-hidden">
        <ActivityBar />
        <div class="flex-1 flex flex-col min-w-0">
          <Toolbar />
          <div class="flex-1 flex overflow-hidden">
            <WorkspaceContent />
          </div>
        </div>
      </div>
      <StatusBar />
      {store.state.showCommandPalette && <CommandPalette />}
      {store.state.showHelp && <HelpOverlay />}
    </div>
  );
}

function WorkspaceContent() {
  const view = () => store.state.activeView;
  return (
    <div class="flex-1 overflow-hidden">
      {view() === 'flows' && <FlowsView />}
      {view() === 'workshop' && <WorkshopView />}
      {view() === 'rules' && <RulesView />}
      {view() === 'scripts' && <ScriptsView />}
      {view() === 'settings' && <SettingsView />}
    </div>
  );
}
