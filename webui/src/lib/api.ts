import type {
  Flow,
  FlowPage,
  FlowSummary,
  Rule,
  FlowModification,
  InterceptItem,
  CoreInterceptSnapshot,
  CoreMetrics,
  CoreStatusSnapshot,
  ProxyPolicy,
  ProxyPolicyPatch,
  CoreAuditSnapshot,
} from '@/types/api';

const BASE = '/api/v1';

async function request<T>(url: string, options?: RequestInit): Promise<T> {
  const res = await fetch(url, {
    headers: { 'Content-Type': 'application/json', ...options?.headers },
    ...options,
  });
  if (!res.ok) {
    const text = await res.text().catch(() => '');
    throw new Error(`${res.status}: ${text}`);
  }
  return res.json();
}

// Flows
export function searchFlows(params: Record<string, string>): Promise<FlowPage> {
  const qs = new URLSearchParams(params);
  return request<FlowPage>(`${BASE}/flows?${qs}`);
}

export function getFlow(id: string): Promise<Flow> {
  return request<Flow>(`${BASE}/flows/${encodeURIComponent(id)}`);
}

export function exportFlowHar(id: string): Promise<Response> {
  return fetch(`${BASE}/flows/${encodeURIComponent(id)}/har`);
}

export function exportFlowsHar(params: Record<string, string>): Promise<Response> {
  const qs = new URLSearchParams(params);
  return fetch(`${BASE}/flows/export/har?${qs}`);
}

export function replayFlow(id: string): Promise<unknown> {
  return request(`${BASE}/flows/${encodeURIComponent(id)}/replay`, { method: 'POST' });
}

// Rules
export function listRules(): Promise<Rule[]> {
  return request<Rule[]>(`${BASE}/rules`);
}

export function putRule(rule: Rule): Promise<Rule> {
  return request<Rule>(`${BASE}/rules`, {
    method: 'PUT',
    body: JSON.stringify(rule),
  });
}

export function deleteRule(id: string): Promise<unknown> {
  return request(`${BASE}/rules/${encodeURIComponent(id)}`, { method: 'DELETE' });
}

export function quickMock(params: {
  url_pattern: string;
  status: number;
  body?: string;
}): Promise<unknown> {
  return request(`${BASE}/mock`, {
    method: 'POST',
    body: JSON.stringify(params),
  });
}

// Intercepts
export function listIntercepts(): Promise<CoreInterceptSnapshot> {
  return request<CoreInterceptSnapshot>(`${BASE}/intercepts`);
}

export function setIntercept(params: {
  url_pattern: string;
  phase: 'request' | 'response';
}): Promise<unknown> {
  return request(`${BASE}/intercepts`, {
    method: 'POST',
    body: JSON.stringify(params),
  });
}

export function resumeIntercept(
  key: string,
  action: 'continue' | 'drop' | 'reject',
  modifications?: FlowModification,
): Promise<unknown> {
  const body: Record<string, unknown> = { action, ...modifications };
  return request(`${BASE}/intercepts/${encodeURIComponent(key)}/resume`, {
    method: 'POST',
    body: JSON.stringify(body),
  });
}

// Metrics & Status
export function getMetrics(): Promise<CoreMetrics> {
  return request<CoreMetrics>(`${BASE}/metrics`);
}

export function getStatus(): Promise<CoreStatusSnapshot> {
  return request<CoreStatusSnapshot>(`${BASE}/status`);
}

// Policy
export function getPolicy(): Promise<ProxyPolicy> {
  return request<ProxyPolicy>(`${BASE}/policy`);
}

export function patchPolicy(patch: ProxyPolicyPatch): Promise<ProxyPolicy> {
  return request<ProxyPolicy>(`${BASE}/policy`, {
    method: 'PATCH',
    body: JSON.stringify(patch),
  });
}

// Audit
export function getAudit(query?: Record<string, string>): Promise<CoreAuditSnapshot> {
  const qs = query ? new URLSearchParams(query) : '';
  return request<CoreAuditSnapshot>(`${BASE}/audit?${qs}`);
}

// Script (only POST currently)
export function loadScript(content: string): Promise<unknown> {
  return request(`${BASE}/script`, {
    method: 'POST',
    body: JSON.stringify({ script: content }),
  });
}
