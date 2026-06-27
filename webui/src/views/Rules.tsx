import { createSignal, createEffect, For, Show } from 'solid-js';
import { store } from '@/lib/store';
import { listRules, putRule, deleteRule, quickMock } from '@/lib/api';
import type { Rule, RuleStage } from '@/types/api';

const EMPTY_RULE: Rule = {
  id: '',
  name: 'new-rule',
  active: true,
  stage: 'RequestHeaders',
  priority: 0,
  termination: 'Continue',
  filter: { type: 'All' },
  actions: [],
};

export default function RulesView() {
  const [rules, setRules] = createSignal<Rule[]>([]);
  const [selectedId, setSelectedId] = createSignal<string | null>(null);
  const [editorContent, setEditorContent] = createSignal('');
  const [error, setError] = createSignal('');
  const [editingNew, setEditingNew] = createSignal(false);

  // Quick mock form
  const [mockUrl, setMockUrl] = createSignal('');
  const [mockStatus, setMockStatus] = createSignal(200);
  const [mockBody, setMockBody] = createSignal('');

  createEffect(() => {
    if (store.state.activeView === 'rules') {
      refreshRules();
    }
  });

  async function refreshRules() {
    try {
      const list = await listRules();
      setRules(list);
    } catch {}
  }

  function selectRule(rule: Rule) {
    setSelectedId(rule.id);
    setEditingNew(false);
    setEditorContent(JSON.stringify(rule, null, 2));
    setError('');
  }

  function newRule() {
    setSelectedId(null);
    setEditingNew(true);
    setEditorContent(JSON.stringify(EMPTY_RULE, null, 2));
    setError('');
  }

  async function handleSave() {
    setError('');
    try {
      const rule: Rule = JSON.parse(editorContent());
      if (!rule.id) rule.id = 'rule-' + Math.random().toString(36).slice(2, 10);
      await putRule(rule);
      setEditorContent(JSON.stringify(rule, null, 2));
      setSelectedId(rule.id);
      setEditingNew(false);
      refreshRules();
    } catch (e: unknown) {
      if (e instanceof SyntaxError) {
        setError('Invalid JSON: ' + e.message);
      } else {
        setError(String(e));
      }
    }
  }

  async function handleDelete() {
    const id = selectedId();
    if (!id) return;
    try {
      await deleteRule(id);
      setSelectedId(null);
      setEditorContent('');
      refreshRules();
    } catch (e: unknown) {
      setError(String(e));
    }
  }

  async function handleToggleActive(rule: Rule) {
    try {
      const updated = { ...rule, active: !rule.active };
      await putRule(updated);
      refreshRules();
    } catch {}
  }

  async function handleQuickMock() {
    if (!mockUrl().trim()) return;
    try {
      await quickMock({
        url_pattern: mockUrl().trim(),
        status: mockStatus(),
        body: mockBody() || undefined,
      });
      setMockUrl('');
      setMockBody('');
      refreshRules();
    } catch (e: unknown) {
      setError(String(e));
    }
  }

  function handleKeyDown(e: KeyboardEvent) {
    if ((e.metaKey || e.ctrlKey) && e.key === 's') {
      e.preventDefault();
      handleSave();
    }
  }

  return (
    <div class="h-full flex">
      {/* Left: Rule list */}
      <div class="w-[35%] min-w-[220px] flex flex-col border-r border-border">
        <div class="p-2 border-b border-border flex gap-1">
          <button
            class="flex-1 px-2 py-1 bg-accent/20 text-accent text-xs rounded hover:bg-accent/30 transition-colors"
            onClick={newRule}
          >
            + New Rule
          </button>
        </div>

        {/* Quick Mock */}
        <div class="p-2 border-b border-border space-y-1">
          <span class="text-[10px] text-text-dim font-bold">Quick Mock</span>
          <input
            class="w-full bg-surface border border-border rounded px-2 py-1 text-xs text-text placeholder-text-dim"
            placeholder="URL pattern"
            value={mockUrl()}
            onInput={(e) => setMockUrl(e.currentTarget.value)}
          />
          <div class="flex gap-1">
            <input
              class="w-16 bg-surface border border-border rounded px-2 py-1 text-xs text-text"
              type="number"
              value={mockStatus()}
              onChange={(e) => setMockStatus(parseInt(e.currentTarget.value))}
            />
            <input
              class="flex-1 bg-surface border border-border rounded px-2 py-1 text-xs text-text placeholder-text-dim"
              placeholder="Body (optional)"
              value={mockBody()}
              onInput={(e) => setMockBody(e.currentTarget.value)}
            />
          </div>
          <button
            class="w-full px-2 py-1 bg-warn/20 text-warn text-xs rounded hover:bg-warn/30 transition-colors"
            onClick={handleQuickMock}
          >
            Mock
          </button>
        </div>

        {/* Rule list */}
        <div class="flex-1 overflow-y-auto">
          <For each={rules()}>
            {(rule) => (
              <button
                class={`w-full text-left p-2 border-b border-border/30 text-xs transition-colors ${
                  selectedId() === rule.id ? 'bg-accent/15 text-text' : 'hover:bg-hover text-text-dim'
                }`}
                onClick={() => selectRule(rule)}
              >
                <div class="flex items-center gap-2">
                  <span
                    class={`w-2 h-2 rounded-full ${rule.active ? 'bg-success' : 'bg-text-dim'}`}
                    onClick={(e) => {
                      e.stopPropagation();
                      handleToggleActive(rule);
                    }}
                    title="Toggle active"
                  />
                  <span class="font-bold">{rule.name}</span>
                </div>
                <div class="flex gap-1 mt-0.5 text-[10px] text-text-dim/60">
                  <span>{rule.stage}</span>
                  <span>P:{rule.priority}</span>
                  <span>{rule.termination}</span>
                </div>
              </button>
            )}
          </For>
        </div>
      </div>

      {/* Right: JSON Editor */}
      <div class="flex-1 flex flex-col min-w-0">
        <div class="h-7 flex items-center px-2 bg-surface border-b border-border shrink-0 text-xs gap-2">
          <span class="text-accent font-bold">{editingNew() ? 'New Rule' : selectedId() ?? 'No selection'}</span>
          <div class="flex-1" />
          <Show when={selectedId() && !editingNew()}>
            <button
              class="px-2 py-0.5 text-error/70 hover:text-error text-[10px] transition-colors"
              onClick={handleDelete}
            >
              Delete
            </button>
          </Show>
          <button
            class="px-3 py-0.5 bg-accent/20 text-accent text-[10px] rounded hover:bg-accent/30 transition-colors"
            onClick={handleSave}
          >
            Save (Cmd+S)
          </button>
        </div>

        <textarea
          class="flex-1 bg-transparent text-text font-mono text-xs p-3 resize-none focus:outline-none"
          value={editorContent()}
          onInput={(e) => setEditorContent(e.currentTarget.value)}
          onKeyDown={handleKeyDown}
          placeholder={JSON.stringify(EMPTY_RULE, null, 2)}
          spellcheck={false}
        />

        <Show when={error()}>
          <div class="p-2 bg-error/10 border-t border-error/30 text-error text-xs">{error()}</div>
        </Show>
      </div>
    </div>
  );
}
