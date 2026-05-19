use crate::interceptor::{BoxError, HttpBody};
use crate::proxy::body_codec::process_body;
use hyper::body::{Body, Bytes, Frame, SizeHint};
use relay_core_api::flow::{BodyData, Direction, FlowUpdate};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::mpsc::Sender;

pub struct TapBody {
    inner: HttpBody,
    flow_id: String,
    on_flow: Sender<FlowUpdate>,
    direction: Direction,
    buffer: Vec<u8>,
    limit: usize,
    headers: Vec<(String, String)>,
}

impl TapBody {
    pub fn new(
        inner: HttpBody,
        flow_id: String,
        on_flow: Sender<FlowUpdate>,
        direction: Direction,
        limit: usize,
        headers: Vec<(String, String)>,
    ) -> Self {
        Self {
            inner,
            flow_id,
            on_flow,
            direction,
            buffer: Vec::new(),
            limit,
            headers,
        }
    }
}

impl Body for TapBody {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        match Pin::new(&mut self.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref()
                    && self.buffer.len() < self.limit
                {
                    let len = std::cmp::min(data.len(), self.limit - self.buffer.len());
                    self.buffer.extend_from_slice(&data[..len]);
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(None) => {
                let (encoding, content) = process_body(&self.buffer, &self.headers);
                let body_data = BodyData {
                    encoding,
                    content,
                    size: self.buffer.len() as u64,
                };

                let _ = self.on_flow.try_send(FlowUpdate::HttpBody {
                    flow_id: self.flow_id.clone(),
                    direction: self.direction.clone(),
                    body: body_data,
                });

                Poll::Ready(None)
            }
            other => other,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}
