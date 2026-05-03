#!/usr/bin/env python3
"""Print benchmark smoke summary in GitHub step summary format."""
import json
import sys
from pathlib import Path

path = Path(sys.argv[1])
data = json.loads(path.read_text(encoding="utf-8"))
m = data.get("metrics", {})
s = data.get("status", {})

print("## Benchmark Smoke Result")
print("")
print(f"- Report: `{path}`")
print(f"- Mode: `{data.get('mode')}`")
print(f"- Duration: `{data.get('duration_seconds')}s`")
print(f"- Tool: `{data.get('tool')}`")
print("")
print("### S1")
print(f"- Cold start: `{m.get('cold_start_ms')}ms` [{s.get('cold_start')}]")
print(f"- Idle RSS: `{m.get('idle_rss_mb')}MB` [{s.get('idle_rss')}]")
print(f"- Throughput: `{m.get('throughput_rps')} req/s` [{s.get('throughput_s1')}]")
print(f"- Latency P99: `{m.get('latency_p99_ms')}ms` [{s.get('latency_p99_s1')}]")
print("")
print("### API Paths")
print(f"- /flows: `{m.get('api_flows_query_ms')}ms`")
print(f"- /flows/{{id}}: `{m.get('api_flow_detail_ms')}ms` [{s.get('api_flow_detail')}]")
print(f"- /events first event: `{m.get('api_sse_first_event_ms')}ms` [{s.get('api_sse')}]")
