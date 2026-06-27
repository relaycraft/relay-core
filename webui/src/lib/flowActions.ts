import { getFlow, replayFlow } from '@/lib/api';
import type { Flow, HttpLayer } from '@/types/api';

export function buildCurlCommand(flow: Flow): string | null {
  const http = (flow.layer as HttpLayer)?.data;
  if (!http?.request) return null;

  const req = http.request;
  const parts = [`curl -X ${req.method}`];
  for (const [name, value] of req.headers) {
    const escaped = value.replace(/'/g, `'\\''`);
    parts.push(`-H '${name}: ${escaped}'`);
  }
  if (req.body?.content) {
    const body =
      req.body.encoding === 'base64'
        ? atob(req.body.content)
        : req.body.content;
    const escaped = body.replace(/'/g, `'\\''`);
    parts.push(`--data-raw '${escaped}'`);
  }
  parts.push(`'${req.url}'`);
  return parts.join(' ');
}

export async function copyCurlForFlow(flowId: string): Promise<boolean> {
  try {
    const flow = await getFlow(flowId);
    const cmd = buildCurlCommand(flow);
    if (!cmd) return false;
    await navigator.clipboard.writeText(cmd);
    return true;
  } catch {
    return false;
  }
}

export async function replayFlowById(flowId: string): Promise<boolean> {
  try {
    await replayFlow(flowId);
    return true;
  } catch {
    return false;
  }
}
