import { For } from 'solid-js';
import { store, type ViewId } from '@/lib/store';

interface NavItem {
  id: ViewId;
  icon: string;
  label: string;
  shortcut: number;
}

const items: NavItem[] = [
  { id: 'flows', icon: '🚦', label: 'Flows', shortcut: 1 },
  { id: 'workshop', icon: '⏸', label: 'Workshop', shortcut: 2 },
  { id: 'rules', icon: '📜', label: 'Rules', shortcut: 3 },
  { id: 'scripts', icon: '⚡', label: 'Scripts', shortcut: 4 },
];

export default function ActivityBar() {
  return (
    <div class="w-12 flex flex-col items-center py-2 bg-surface border-r border-border shrink-0">
      <For each={items}>
        {(item) => (
          <button
            class={`w-10 h-10 flex items-center justify-center rounded-md mb-1 text-lg transition-colors ${
              store.state.activeView === item.id
                ? 'bg-accent/20 text-accent'
                : 'text-text-dim hover:text-text hover:bg-hover'
            }`}
            onClick={() => store.setActiveView(item.id)}
            title={`${item.label} (Cmd+${item.shortcut})`}
          >
            {item.icon}
          </button>
        )}
      </For>
      <div class="flex-1" />
      <button
        class={`w-10 h-10 flex items-center justify-center rounded-md mb-1 transition-colors ${
          store.state.activeView === 'settings'
            ? 'bg-accent/20 text-accent'
            : 'text-text-dim hover:text-text hover:bg-hover'
        }`}
        onClick={() => store.setActiveView('settings')}
        title="Settings (Cmd+,)"
      >
        ⚙
      </button>
      <button
        class="w-10 h-10 flex items-center justify-center rounded-md text-text-dim hover:text-text hover:bg-hover transition-colors"
        title="Help (?)"
        onClick={() => store.setState('showHelp', true)}
      >
        ?
      </button>
    </div>
  );
}
