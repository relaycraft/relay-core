# 0014 · Content-Encoding 是注册表，且按栈顺序处理

- **Status**: Accepted
- **Date**: 2026-11-10
- **Related**: [`../l7-first-engine-evolution-roadmap.md`](../l7-first-engine-evolution-roadmap.md)（§24.3、§19 Phase C）、
  [`../engine-capability-status.md`](../engine-capability-status.md)（§14）

## 背景

`content_encoding.rs` 原本是一个 `match`：

```rust
match encoding.as_str() {
    "gzip" | "x-gzip" => read_all(GzDecoder::new(body)),
    ...
    _ => None,
}
```

两个问题：

1. **「支持」与实际解码是两套真相。** `SUPPORTED_ENCODINGS` 是常量数组，`is_supported` 查它，
   解码查 `match`。新增一个 codec 需要同时改两处，漏一处就出现「报告支持但解码失败」。
2. **栈式编码被当成不支持。** `Content-Encoding: gzip, br` 是真实存在的（先 gzip 再 br）。
   旧实现把整串当一个名字查，查不到就整体跳过——**body 规则在这类流量上静默不生效**，
   而且不会有任何提示。这比崩溃更糟：用户以为规则跑过了。

## 决策

### 1. `ContentCodec` trait + 注册表

```rust
pub trait ContentCodec: Send + Sync {
    fn name(&self) -> &'static str;
    fn decode(&self, body: &[u8]) -> Option<Vec<u8>>;
    fn encode(&self, body: &[u8]) -> Option<Vec<u8>>;
}
```

内置 gzip / deflate / br / zstd 以该 trait 实现，`CodecRegistry` 按名字索引，`x-gzip` 为别名。
`register_codec` 允许宿主在运行时加入 codec；`is_supported` 与解码路径查**同一个注册表**，
因此「报告支持」与「真能解」不会再分叉。

### 2. 栈按顺序处理

`Content-Encoding: gzip, br` 表示 gzip 先应用、br 后应用：

- **解码从后往前**（先解 br，再解 gzip）；
- **编码从前往后**，重新声明 `gzip, br`。

任何一段未知或解码失败 → 整条链放弃，原字节透传，且**绝不**只解码已知的一半——
半解码的字节没有任何人能读。

### 3. `identity` 表示「没有编码」

`identity` 与空串不参与链，`is_supported` 对它返回 true：不需要 codec 的编码天然可往返。
旧实现把它列为「不支持」，让一个无害的头看起来像故障。

## 后果

- 新增 codec 只需实现 trait 并注册，不需要改解码器或支持列表。
- 栈式编码的流量现在能被规则看到，且重写后 header 与 body 一致。
- 仍然**不**支持 `Content-Encoding` 之外的传输层编码协商（如 `Transfer-Encoding` 组合），
  也不做请求方向的自动解压注入——那是另一件事，见 §24.3 的未决项。
- 注册表是全局的，测试里注册的 codec 会留在进程内。这在一份测试二进制里是可接受的，
  文档与代码注释都写明了这一点。
