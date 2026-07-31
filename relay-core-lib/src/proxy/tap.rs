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
    /// Set to true when accumulated bytes exceed the limit.
    pub budget_exceeded: bool,
    /// Total bytes passed through.
    pub total_bytes: u64,
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
        crate::metrics::inc_proxy_stream_mode_tap();
        Self {
            inner,
            flow_id,
            on_flow,
            direction,
            buffer: Vec::new(),
            limit,
            headers,
            budget_exceeded: false,
            total_bytes: 0,
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
                if let Some(data) = frame.data_ref() {
                    let len = data.len() as u64;
                    self.total_bytes += len;
                    match self.direction {
                        Direction::ClientToServer => crate::metrics::add_bytes_sent(len),
                        Direction::ServerToClient => crate::metrics::add_bytes_recv(len),
                    }
                    if self.buffer.len() < self.limit {
                        let len = std::cmp::min(data.len(), self.limit - self.buffer.len());
                        self.buffer.extend_from_slice(&data[..len]);
                    }
                    if self.buffer.len() >= self.limit {
                        self.budget_exceeded = true;
                    }
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(None) => {
                let (encoding, content) = process_body(&self.buffer, &self.headers);
                let body_data = BodyData {
                    encoding,
                    content,
                    size: self.total_bytes, // Report actual transfer size, not truncated buffer
                };

                let _ = self.on_flow.try_send(FlowUpdate::HttpBody {
                    flow_id: self.flow_id.clone(),
                    direction: self.direction.clone(),
                    body: body_data,
                });

                // P1: Notify budget exceeded for streaming-first pipeline
                if self.budget_exceeded {
                    crate::metrics::inc_proxy_body_degraded();
                    crate::metrics::inc_proxy_stream_mode_degrade();
                    let _ = self.on_flow.try_send(FlowUpdate::BodyBudgetExceeded {
                        flow_id: self.flow_id.clone(),
                        direction: self.direction.clone(),
                    });
                }

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

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use http_body_util::BodyExt;
    use hyper::body::Frame;
    use relay_core_api::flow::Direction;
    use std::pin::Pin;
    use std::task::{Context, Poll, Waker};

    /// Simple Data + Trailers body used in both tests below.
    struct DataThenTrailers {
        phase: u8,
    }

    impl Body for DataThenTrailers {
        type Data = Bytes;
        type Error = BoxError;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
            match self.phase {
                0 => {
                    self.phase = 1;
                    Poll::Ready(Some(Ok(Frame::data(Bytes::from("hello")))))
                }
                1 => {
                    self.phase = 2;
                    let mut trailers = hyper::HeaderMap::new();
                    trailers.insert("x-trailer", "value".parse().unwrap());
                    Poll::Ready(Some(Ok(Frame::trailers(trailers))))
                }
                _ => Poll::Ready(None),
            }
        }
    }

    /// Verify TapBody passes trailers through while still correctly
    /// buffering body data and emitting HttpBody/BodyBudgetExceeded events.
    #[tokio::test]
    async fn test_tap_body_passes_trailers() {
        let body: HttpBody = DataThenTrailers { phase: 0 }.boxed();
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);

        let mut tap = TapBody::new(
            body,
            "test-flow".to_string(),
            tx,
            Direction::ServerToClient,
            4096,
            vec![],
        );

        // Collect frames and FlowUpdate events
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);

        let mut data_frames = 0;
        let mut trailer_frames = 0;
        let mut trailers: Option<hyper::HeaderMap> = None;

        loop {
            match Pin::new(&mut tap).poll_frame(&mut cx) {
                Poll::Ready(Some(Ok(frame))) => {
                    if frame.data_ref().is_some() {
                        data_frames += 1;
                    }
                    if let Some(t) = frame.trailers_ref() {
                        trailer_frames += 1;
                        trailers = Some(t.clone());
                    }
                }
                Poll::Ready(Some(Err(e))) => panic!("unexpected error: {}", e),
                Poll::Ready(None) => break,
                Poll::Pending => panic!("unexpected pending"),
            }
        }

        // Verify trailers forwarded
        assert_eq!(data_frames, 1, "should forward 1 data frame");
        assert_eq!(trailer_frames, 1, "should forward 1 trailers frame");
        let trailers = trailers.expect("trailers should be present");
        assert_eq!(
            trailers.get("x-trailer").and_then(|v| v.to_str().ok()),
            Some("value"),
            "trailer x-trailer should be preserved"
        );

        // Verify TapBody still sent HttpBody event
        let event = rx.try_recv().expect("should emit HttpBody event");
        match event {
            FlowUpdate::HttpBody { body, .. } => {
                assert_eq!(body.size, 5, "body size should match data");
            }
            other => panic!("expected HttpBody, got {:?}", other),
        }
    }
}
