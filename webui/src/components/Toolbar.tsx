import { store } from '@/lib/store';

const viewLabels: Record<string, string> = {
  flows: 'Flows',
  workshop: 'Workshop',
  rules: 'Rules',
  scripts: 'Scripts',
  settings: 'Settings',
};

export default function Toolbar() {
  return (
    <div class="h-8 flex items-center px-3 bg-surface border-b border-border shrink-0 gap-3">
      <span class="text-xs text-accent font-bold tracking-wider uppercase">
        {viewLabels[store.state.activeView] ?? ''}
      </span>

      {store.state.activeView === 'flows' && (
        <input
          id="flow-filter"
          class="flex-1 bg-transparent border border-border rounded px-2 py-0.5 text-xs text-text placeholder-text-dim focus:outline-none focus:border-accent/50 font-mono"
          placeholder="Filter: host:api method:POST status:>=400 err ws ..."
          value={store.state.filterText}
          onInput={(e) => store.setState('filterText', e.currentTarget.value)}
        />
      )}

      <div class="flex-1" />

      <span class="text-[10px] text-text-dim/50">
        <kbd class="px-1 py-0.5 rounded bg-subtle border border-border text-[10px]">Cmd+K</kbd> Commands
      </span>
    </div>
  );
}
