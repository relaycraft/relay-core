import { For, type JSX } from 'solid-js';
import { store, type ViewId } from '@/lib/store';
import { createSignal } from 'solid-js';
import { effectiveTheme, setTheme } from '@/lib/theme';
import {
  IconFlows,
  IconHelp,
  IconRules,
  IconScripts,
  IconSettings,
  IconTheme,
  IconWorkshop,
} from '@/components/Icons';

interface NavItem {
  id: ViewId;
  /** Rendered as an inline component so the icon inherits the button's colour. */
  icon: (props: { size?: number }) => JSX.Element;
  label: string;
  shortcut: number;
}

const items: NavItem[] = [
  { id: 'flows', icon: IconFlows, label: 'Flows', shortcut: 1 },
  { id: 'workshop', icon: IconWorkshop, label: 'Workshop', shortcut: 2 },
  { id: 'rules', icon: IconRules, label: 'Rules', shortcut: 3 },
  { id: 'scripts', icon: IconScripts, label: 'Scripts', shortcut: 4 },
];

export default function ActivityBar() {
  // Mirrors what is on screen, so the button's tooltip and icon describe the current state rather
  // than the state this component would have chosen.
  const [theme, setThemeSignal] = createSignal(effectiveTheme());

  const toggleTheme = () => {
    const next = theme() === 'dark' ? 'light' : 'dark';
    setTheme(next);
    setThemeSignal(next);
  };

  return (
    <div class="w-12 flex flex-col items-center py-2 bg-surface border-r border-border shrink-0">
      <For each={items}>
        {(item) => (
          <button
            class={`w-10 h-10 flex items-center justify-center rounded-md mb-1 transition-colors ${
              store.state.activeView === item.id
                ? 'bg-accent/20 text-accent'
                : 'text-text-dim hover:text-text hover:bg-hover'
            }`}
            onClick={() => store.setActiveView(item.id)}
            title={`${item.label} (Cmd+${item.shortcut})`}
          >
            {item.icon({ size: 18 })}
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
        aria-label="Settings"
      >
        <IconSettings size={18} />
      </button>
      <button
        class="w-10 h-10 flex items-center justify-center rounded-md mb-1 text-text-dim hover:text-text hover:bg-hover transition-colors"
        onClick={toggleTheme}
        title={`Switch to ${theme() === 'dark' ? 'light' : 'dark'} theme`}
        aria-label="Toggle theme"
      >
        <IconTheme size={18} />
      </button>
      <button
        class="w-10 h-10 flex items-center justify-center rounded-md text-text-dim hover:text-text hover:bg-hover transition-colors"
        title="Help (?)"
        aria-label="Help"
        onClick={() => store.setState('showHelp', true)}
      >
        <IconHelp size={18} />
      </button>
    </div>
  );
}
