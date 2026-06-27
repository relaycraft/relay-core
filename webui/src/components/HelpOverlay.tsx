import { For } from 'solid-js';
import { store } from '@/lib/store';

const shortcuts = [
  { group: 'Global', keys: [
    ['Cmd/Ctrl+K', 'Command palette'],
    ['Cmd/Ctrl+/', 'Command palette'],
    ['Cmd/Ctrl+1..5', 'Switch views'],
    ['Cmd/Ctrl+,', 'Settings'],
    ['?', 'This help'],
    ['Esc', 'Close overlay'],
  ]},
  { group: 'Flows', keys: [
    ['j / ↓', 'Next flow'],
    ['k / ↑', 'Previous flow'],
    ['g / Home', 'First flow'],
    ['G / End', 'Last flow'],
    ['/', 'Focus filter'],
    ['r', 'Replay'],
    ['c', 'Copy cURL'],
    ['x', 'Clear list (local)'],
  ]},
  { group: 'Workshop', keys: [
    ['Cmd/Ctrl+Enter', 'Accept (continue)'],
    ['Cmd/Ctrl+Shift+Enter', 'Drop'],
    ['R', 'Reject'],
    ['j / k', 'Switch pending intercept'],
  ]},
  { group: 'Editors', keys: [
    ['Cmd/Ctrl+S', 'Save to backend'],
  ]},
];

export default function HelpOverlay() {
  return (
    <div
      class="fixed inset-0 z-50 flex items-center justify-center bg-black/70"
      onClick={() => store.setState('showHelp', false)}
    >
      <div
        class="w-[560px] max-h-[80vh] overflow-y-auto bg-surface border border-border p-4"
        onClick={(e) => e.stopPropagation()}
      >
        <div class="flex items-center justify-between mb-3">
          <h2 class="text-accent font-bold text-sm">Keyboard Shortcuts</h2>
          <button
            class="text-text-dim hover:text-text text-xs"
            onClick={() => store.setState('showHelp', false)}
          >
            Esc to close
          </button>
        </div>
        <For each={shortcuts}>
          {(section) => (
            <section class="mb-4">
              <h3 class="text-[10px] uppercase tracking-wide text-text-dim mb-2">{section.group}</h3>
              <div class="space-y-1">
                <For each={section.keys}>
                  {([key, desc]) => (
                    <div class="flex gap-3 text-xs">
                      <kbd class="w-36 shrink-0 px-1 py-0.5 bg-subtle border border-border text-text-dim font-mono">
                        {key}
                      </kbd>
                      <span class="text-text">{desc}</span>
                    </div>
                  )}
                </For>
              </div>
            </section>
          )}
        </For>
      </div>
    </div>
  );
}
