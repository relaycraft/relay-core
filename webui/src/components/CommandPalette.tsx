import { createSignal, For, onMount } from 'solid-js';
import { store } from '@/lib/store';

interface Command {
  id: string;
  label: string;
  group: string;
  action: () => void;
}

export default function CommandPalette() {
  let inputRef!: HTMLInputElement;
  const [query, setQuery] = createSignal('');

  const commands: Command[] = [
    { id: 'view-flows', label: 'Flows: Traffic Observer', group: 'Navigate', action: () => store.setActiveView('flows') },
    { id: 'view-workshop', label: 'Workshop: Intercept', group: 'Navigate', action: () => store.setActiveView('workshop') },
    { id: 'view-rules', label: 'Rules: Manage Rules', group: 'Navigate', action: () => store.setActiveView('rules') },
    { id: 'view-scripts', label: 'Scripts: Script Engine', group: 'Navigate', action: () => store.setActiveView('scripts') },
    { id: 'view-settings', label: 'Settings: Proxy Policy', group: 'Navigate', action: () => store.setActiveView('settings') },
    { id: 'clear-flows', label: 'Clear flow list', group: 'Actions', action: () => store.clearFlows() },
    { id: 'show-help', label: 'Show keyboard shortcuts', group: 'Help', action: () => store.setState('showHelp', true) },
  ];

  const filtered = () => {
    const q = query().toLowerCase();
    if (!q) return commands;
    return commands.filter((c) => c.label.toLowerCase().includes(q));
  };

  function execute(cmd: Command) {
    cmd.action();
    store.setState('showCommandPalette', false);
  }

  function handleKeyDown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      store.setState('showCommandPalette', false);
    }
  }

  onMount(() => {
    inputRef.focus();
  });

  return (
    <div
      class="fixed inset-0 z-50 flex items-start justify-center pt-[20vh] bg-black/60"
      onClick={() => store.setState('showCommandPalette', false)}
    >
      <div
        class="w-[480px] max-h-[400px] bg-surface border border-border rounded-lg shadow-2xl overflow-hidden"
        onClick={(e) => e.stopPropagation()}
      >
        <div class="p-3 border-b border-border">
          <input
            ref={inputRef}
            class="w-full bg-transparent text-sm text-text placeholder-text-dim focus:outline-none"
            placeholder="Type a command..."
            value={query()}
            onInput={(e) => setQuery(e.currentTarget.value)}
            onKeyDown={handleKeyDown}
          />
        </div>
        <div class="overflow-y-auto max-h-[300px]">
          <For each={filtered()}>
            {(cmd) => (
              <button
                class="w-full px-3 py-2 text-left text-sm hover:bg-hover flex items-center gap-3 transition-colors"
                onClick={() => execute(cmd)}
              >
                <span class="text-[10px] text-text-dim/60 w-16">{cmd.group}</span>
                <span class="text-text">{cmd.label}</span>
              </button>
            )}
          </For>
        </div>
      </div>
    </div>
  );
}
