# RelayCore vs mitmproxy — 策略对标

> 对照 mitmproxy **12.2.3**（macOS arm64 官方二进制，Python 3.14.4）的**实测行为**，
> 评估 RelayCore 在 body 观察、Content-Encoding、流式与改写语义上的取舍。

- **Status**: Active / 对标参考
- **Created**: 2026-09-12
- **Method**: 全部结论来自本机 mitmproxy 12.2.3 的**实跑探测**（addon 打印 hook 时序与 body 状态，
  并用 `curl` 校验客户端实际收到的字节），不是文档转述或记忆。
- **Related**: [`l7-first-engine-evolution-roadmap.md`](./l7-first-engine-evolution-roadmap.md)（§24.3、§24.2）

---

## 1. 实测得到的 mitmproxy 行为

### 1.1 响应 body 在 headers hook 阶段不可用

```
HDRS  hook: raw_len=0 text_prop=None content-encoding='gzip'
HDRS  get_text(strict=True)  -> None
HDRS  get_text(strict=False) -> None
RESP  hook: text='original-compressed-payload'
RESP  raw first6=b'\x1f\x8b\x08\x00\x00\x00' len=47
```

- `responseheaders` 时 `raw_content` 为空、`text` 为 `None`。
- 到 `response` hook 时 `text` 已是**明文**，而 `raw_content` 仍是**压缩字节**。

**结论**：mitmproxy 不在 header 阶段做 body 决策，而是把「body 可用」推迟到 body hook，
因此**不需要**为「响应体规则能否匹配」做提前缓冲。

### 1.2 自动解码 + 写回自动重编码，且 header 始终与所发字节一致

对 `gzip` 响应执行 `resp.text = "REWRITTEN-CONTENT"`：

```
AFTER set text: content-encoding='gzip' content-length='35' raw first6=b'\x1f\x8b\x08...'
client body: gzip 字节，解压后 = b'REWRITTEN-CONTENT'，content-length 与实际长度一致
```

对真实 `br` 上游同样通过：

```
ce='br' text='original-brotli-payload'
after set text: ce='br' cl='16'
client body: 合法 brotli，解压后 = "REWRITTEN-BR"
```

对 `zstd` 亦支持（解码失败时报 `ZstdError`，说明走的是真实 zstd 解码器而非跳过）：

```
ce='zstd' -> get_text raised ValueError: ZstdError
after set: ce='zstd'，写回为合法 zstd
```

**结论**：mitmproxy 支持 **gzip / deflate / br / zstd**，读取时解码、写入时重编码，
并同步 `content-length`。header 从不与实际字节矛盾。

### 1.3 默认缓冲，流式为显式选项

```
stream_setting_default=False
set stream=True
RESP stream=True text=None raw_len=0      # 流式时 body 完全不暴露
client body: 未改动，47 字节
```

**结论**：mitmproxy **默认完整缓冲** body，`flow.response.stream = True` 才流式；
一旦流式，body 就不可供 addon 读取（二者互斥）。

---

## 2. RelayCore 现状对照

| 维度 | mitmproxy 12.2.3（实测） | RelayCore 现状 |
|---|---|---|
| body 在 header 阶段 | 不可用（推迟到 body hook） | **已可用**（需提前声明意图，代理在 header 投影后保留） |
| 默认是否缓冲 | **默认全量缓冲** | **默认流式**（`BodyObservation::Off`，零拷贝） |
| 流式与可读性 | 互斥（流式即不可读） | 不互斥（`Capture` 保留有界前缀且保持流式） |
| 解码编码集 | gzip / deflate / **br** / **zstd** | gzip / deflate（br、zstd 未解码） |
| 写回行为 | 自动重编码，`content-length` 同步 | gzip/deflate 重编码；br/zstd **丢弃编码头发明文** |
| header 与字节一致性 | 始终一致 | gzip/deflate 一致；br/zstd 一致但**能力降级** |
| 大 body 内存策略 | 默认全量进内存（含 `max_body_size` 类限制） | 有界前缀 + 预算，超预算打标签 |

---

## 3. 结论与影响

### 3.1 RelayCore 的设计选择是可辩护的，且部分是刻意的改进

- **默认流式**优于 mitmproxy 的默认全量缓冲：Roadmap §22 要求「无修改场景不无谓解压/缓冲」。
  mitmproxy 的默认行为在大 body 上是有代价的，它用「流式」作为逃生口。
- **流式与可读性不互斥**优于 mitmproxy：`Capture` 让 body 在有界内存内可观察，同时保持流式。
  mitmproxy 一旦流式就放弃可读性。
- **提前声明意图**是 RelayCore 特有约束的产物：因为我们允许 body 阶段规则在 body hook 之前
  参与决策，所以必须在 header 时刻就知道是否要保留。这与 mitmproxy 的「推迟」是两条路。

### 3.2 明确的能力差距（不应掩饰）

1. **`br` / `zstd` 未解码**：mitmproxy 能解能写回，RelayCore 在改写时会**丢弃 `Content-Encoding`
   并发明文**。这是**能力降级**，不是等价实现——虽然 header 与 body 一致（不会骗客户端），
   但与「保持原编码」的预期不符，且对要求 br 的客户端（如某些 CDN 链路）不等价。
2. **压缩 body 上的「匹配」链路不完整**：Roadmap §24.3 记录的
   「解压 → 匹配 → 重编码」目前只在**重写**路径生效；body 过滤器仍可能看到压缩字节。
3. mitmproxy 的 `raw_content`/`text` **双视图**（wire 字节与语义视图分离）比 RelayCore 更干净：
   RelayCore 用 `BodyData.encoding` 单字段承载两种语义，历史上已因此出过 base64 误用（§24.3）。

### 3.3 建议的后续取舍

| 项 | 建议 | 理由 |
|---|---|---|
| 补 `br` / `zstd` 解码重编码 | **建议做** | 用纯 Rust crate（`brotli`、`zstd`）避免 C 工具链；这是 mitmproxy 有而我们没有的真实能力 |
| 引入 raw/wire 与语义双视图 | **建议评估** | 能一次性消除 `BodyData.encoding` 的歧义与相关内容 bug 类 |
| 把观察默认改为缓冲 | **不建议** | 违背 §22；且 RelayCore 的 `Capture` 已提供更好的第三条路 |
| 提前声明意图改为推迟到 body hook | **不建议** | 会改变规则的阶段语义（现有用户可见行为） |

---

## 4. 可复现步骤

```bash
# 1. 上游：gzip / br 两种响应
python3 - <<'PY' &   # 见本文 §1 的 upstream 片段
PY

# 2. 探测 addon：打印 hook 时序 + body 状态 + 改写后的 header
mitmdump -q -p 19500 --scripts probe.py

# 3. 校验客户端实际收到的字节
curl -s -x http://127.0.0.1:19500 http://127.0.0.1:<upstream>/gzip -o out.bin -D hdr.txt
gzip -dc out.bin     # 必须等于 addon 写入的文本
```

探测要点：`raw_content` 与 `text` 的差异、`content-length` 是否随重编码更新、
流式设定后 `text` 是否变为 `None`。
