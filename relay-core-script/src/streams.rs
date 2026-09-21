//! Body bytes handed to a script, and the body the proxy keeps for forwarding.
//!
//! Script hooks run on a dedicated thread with its own Tokio runtime. Polling a live
//! proxy body there never wakes the socket, so the hook buffers on the proxy runtime
//! and the isolate only ever sees memory.

use bytes::Bytes;
use deno_core::{AsyncResult, BufView, Resource};
use http_body::{Body, Frame};
use http_body_util::{BodyExt, Full};
use relay_core_lib::interceptor::{BoxError, HttpBody};
use std::borrow::Cow;
use std::collections::VecDeque;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll};

/// What the script may read, plus the body that must still be forwarded.
pub struct PreparedScriptBody {
    /// Prefix the script can read. Never longer than the budget.
    pub visible: Bytes,
    /// `visible` is not the whole body. `text()` / `json()` must fail closed.
    pub truncated: bool,
    /// Complete payload for the wire when the script does not replace the body.
    ///
    /// When `truncated` is false this is an in-memory copy. When it is true, the unread
    /// tail is still the original stream and must be polled on the proxy runtime.
    pub forward: HttpBody,
}

/// Copy up to `budget` bytes for the script without dropping the rest.
pub async fn prepare_script_body(
    mut body: HttpBody,
    budget: usize,
) -> Result<PreparedScriptBody, BoxError> {
    if budget == 0 {
        return Ok(PreparedScriptBody {
            visible: Bytes::new(),
            truncated: true,
            forward: body,
        });
    }

    let mut frames: Vec<Bytes> = Vec::new();
    let mut visible: Vec<u8> = Vec::new();
    let mut truncated = false;

    while let Some(frame) = body.frame().await {
        let frame = frame?;
        let data = match frame.into_data() {
            Ok(data) => data,
            Err(_) => continue,
        };
        if data.is_empty() {
            continue;
        }

        if visible.len() >= budget {
            // Already pulled this frame off the stream, so it has to travel with the prefix.
            frames.push(data);
            truncated = true;
            break;
        }

        let room = budget - visible.len();
        if data.len() <= room {
            visible.extend_from_slice(&data);
            frames.push(data);
        } else {
            visible.extend_from_slice(&data[..room]);
            frames.push(data);
            truncated = true;
            break;
        }
    }

    let visible = Bytes::from(visible);
    let forward = if truncated {
        ReplayThenRest {
            prefix: VecDeque::from(frames),
            rest: body,
        }
        .boxed()
    } else {
        full_body(visible.clone())
    };

    Ok(PreparedScriptBody {
        visible,
        truncated,
        forward,
    })
}

pub fn full_body(bytes: Bytes) -> HttpBody {
    Full::new(bytes)
        .map_err(|e| -> BoxError { e.into() })
        .boxed()
}

/// Frames already pulled off the stream, followed by whatever was not read.
struct ReplayThenRest {
    prefix: VecDeque<Bytes>,
    rest: HttpBody,
}

impl Body for ReplayThenRest {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        if let Some(chunk) = this.prefix.pop_front() {
            return Poll::Ready(Some(Ok(Frame::data(chunk))));
        }
        Pin::new(&mut this.rest).poll_frame(cx)
    }
}

/// In-memory body resource. `op_read_body` never touches a socket.
pub struct MemoryBodyResource {
    data: Bytes,
    pos: std::cell::Cell<usize>,
}

impl MemoryBodyResource {
    pub fn new(data: Bytes) -> Self {
        Self {
            data,
            pos: std::cell::Cell::new(0),
        }
    }
}

impl Resource for MemoryBodyResource {
    fn name(&self) -> Cow<'_, str> {
        "memoryBody".into()
    }

    fn read(self: Rc<Self>, limit: usize) -> AsyncResult<BufView> {
        let start = self.pos.get();
        let end = start.saturating_add(limit).min(self.data.len());
        self.pos.set(end);
        let slice = self.data.slice(start..end);
        Box::pin(async move { Ok(BufView::from(slice.to_vec())) })
    }
}

#[cfg(test)]
mod tests {
    use super::prepare_script_body;
    use bytes::Bytes;
    use http_body_util::{BodyExt, Full, StreamBody};
    use relay_core_lib::interceptor::BoxError;

    #[tokio::test]
    async fn prepare_within_budget_forwards_the_same_bytes() {
        let body = Full::new(Bytes::from_static(b"hello-body"))
            .map_err(|e| -> BoxError { e.into() })
            .boxed();
        let prepared = prepare_script_body(body, 1024).await.unwrap();
        assert!(!prepared.truncated);
        assert_eq!(&prepared.visible[..], b"hello-body");
        let forwarded = prepared.forward.collect().await.unwrap().to_bytes();
        assert_eq!(&forwarded[..], b"hello-body");
    }

    #[tokio::test]
    async fn prepare_over_budget_keeps_the_unread_tail() {
        let chunks = vec![
            Ok::<_, BoxError>(http_body::Frame::data(Bytes::from_static(b"abcd"))),
            Ok::<_, BoxError>(http_body::Frame::data(Bytes::from_static(b"efghij"))),
        ];
        let body = StreamBody::new(futures_util::stream::iter(chunks)).boxed();
        let prepared = prepare_script_body(body, 4).await.unwrap();
        assert!(prepared.truncated);
        assert_eq!(&prepared.visible[..], b"abcd");
        let forwarded = prepared.forward.collect().await.unwrap().to_bytes();
        assert_eq!(&forwarded[..], b"abcdefghij");
    }
}
