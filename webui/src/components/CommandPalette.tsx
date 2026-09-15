import { createEffect, createSignal, For, onMount, Show } from 'solid-js';
import { store } from '@/lib/store';

interface Command {
  id: string;
  label: string;
  group: string;
  action: () => void;
}

export default function CommandPalette() {
  let inputRef!: HTMLInputElement;
  let listRef!: HTMLDivElement;
  const [query, setQuery] = createSignal('');
  // A command palette that cannot be driven from the keyboard is not a command palette: it had only
  // Escape, so Enter did nothing and there was no way to tell which command it would run.
  const [selected, setSelected] = createSignal(0);

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

  // Re-filtering can leave the cursor past the end of the list.
  createEffect(() => {
    const count = filtered().length;
    if (count === 0) {
      setSelected(0);
    } else if (selected() >= count) {
      setSelected(count - 1);
    }
  });

  // Keep the highlighted command visible when the list scrolls.
  createEffect(() => {
    const index = selected();
    const el = listRef?.querySelectorAll('button')[index];
    el?.scrollIntoView({ block: 'nearest' });
  });

  function runSelected() {
    const cmd = filtered()[selected()];
    if (cmd) execute(cmd);
  }

  function handleKeyDown(e: KeyboardEvent) {
    const count = filtered().length;
    if (e.key === 'Escape') {
      store.setState('showCommandPalette', false);
      return;
    }
    if (e.key === 'Enter') {
      e.preventDefault();
      runSelected();
      return;
    }
    if (count === 0) return;
    if (e.key === 'ArrowDown' || (e.key === 'n' && e.ctrlKey)) {
      e.preventDefault();
      setSelected((i) => (i + 1) % count);
      return;
    }
    if (e.key === 'ArrowUp' || (e.key === 'p' && e.ctrlKey)) {
      e.preventDefault();
      setSelected((i) => (i - 1 + count) % count);
    }
  }

  onMount(() => {
    inputRef.focus();
  });

  return (
    <div
      class="fixed inset-0 z-50 flex items-start justify-center pt-[20vh] bg-scrim"
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
            onInput={(e) => {
              setQuery(e.currentTarget.value);
              setSelected(0);
            }}
            onKeyDown={handleKeyDown}
          />
        </div>
        <div ref={listRef} class="overflow-y-auto max-h-[300px]">
          <Show when={filtered().length === 0}>
            <div class="px-3 py-3 text-[13px] text-text-dim">No matching command.</div>
          </Show>
          <For each={filtered()}>
            {(cmd, index) => (
              <button
                class={`w-full px-3 py-2 text-left text-[13px] flex items-center gap-3 transition-colors ${
                  index() === selected() ? 'bg-accent/20 text-text' : 'hover:bg-hover'
                }`}
                onMouseEnter={() => setSelected(index())}
                onClick={() => execute(cmd)}
              >
                <span class="text-[12px] text-text-dim w-16">{cmd.group}</span>
                <span class="text-text">{cmd.label}</span>
              </button>
            )}
          </For>
        </div>
      </div>
    </div>
  );
}
