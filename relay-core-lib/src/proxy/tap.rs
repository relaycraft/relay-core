use crate::interceptor::{BoxError, HttpBody};
use crate::proxy::body_codec::process_body_with_framing;
use crate::proxy::body_plan::{PrefixBuffer, buffer_prefix};
use hyper::body::{Body, Bytes, Frame, SizeHint};
use relay_core_api::flow::{BodyData, Direction, FlowUpdate};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::mpsc::Sender;

/// Streaming body observer: forwards every frame untouched while retaining a bounded prefix.
///
/// Prefix retention is delegated to [`crate::proxy::body_plan::buffer_prefix`] so this observation
/// path and [`BodyPlan::Capture`](relay_core_api::body_plan::BodyPlan) share one implementation.
pub struct TapBody {
    inner: PrefixBuffer,
    flow_id: String,
    on_flow: Sender<FlowUpdate>,
    direction: Direction,
    headers: Vec<(String, String)>,
    /// Whether the recorded body has been reported yet.
    ///
    /// The report must happen exactly once, and it must happen even when the consumer never polls for
    /// the final `None`: hyper finishes a body as soon as `is_end_stream()` says so, so relying on the
    /// end-of-stream poll alone silently loses the body from the capture.
    reported: std::sync::atomic::AtomicBool,
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
            inner: buffer_prefix(inner, limit),
            flow_id,
            on_flow,
            direction,
            headers,
            reported: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Report the body once, from whichever signal arrives first.
    ///
    /// Two signals because either can be the last one a consumer sees: the end-of-stream poll, and
    /// `is_end_stream()` (which hyper consults to decide the body is finished, and which does not
    /// require another poll). Reporting from only one of them loses bodies from the capture.
    fn report_once(&self) {
        use std::sync::atomic::Ordering;
        if self.reported.swap(true, Ordering::Relaxed) {
            return;
        }

        let snapshot = self.inner.snapshot();
        // Framing is reported alongside the representation: this is the path that feeds captures of
        // *streamed* bodies, so a gRPC call would otherwise be visible only as base64 even though the
        // request never needed buffering.
        let (encoding, content, grpc) = process_body_with_framing(&snapshot.bytes, &self.headers);
        let body_data = BodyData {
            encoding,
            content,
            // Report the observed transfer size, not the truncated buffer length.
            size: snapshot.total_bytes,
            grpc,
        };

        let _ = self.on_flow.try_send(FlowUpdate::HttpBody {
            flow_id: self.flow_id.clone(),
            direction: self.direction.clone(),
            body: body_data,
        });

        // P1: Notify budget exceeded for streaming-first pipeline
        if snapshot.truncated {
            crate::metrics::inc_proxy_body_degraded();
            crate::metrics::inc_proxy_stream_mode_degrade();
            let _ = self.on_flow.try_send(FlowUpdate::BodyBudgetExceeded {
                flow_id: self.flow_id.clone(),
                direction: self.direction.clone(),
            });
        }
    }

    /// Whether the retention budget was reached, meaning the recorded body is a prefix.
    pub fn budget_exceeded(&self) -> bool {
        self.inner.truncated()
    }

    /// Bytes observed so far.
    pub fn total_bytes(&self) -> u64 {
        self.inner.observed_bytes()
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
                    match self.direction {
                        Direction::ClientToServer => crate::metrics::add_bytes_sent(len),
                        Direction::ServerToClient => crate::metrics::add_bytes_recv(len),
                    }
                }

                // Trailers arrive after the body, so they cannot be part of the original snapshot and
                // are reported as an incremental update. Only the response direction is recorded:
                // gRPC puts the outcome of a call there, and `HttpRequest` has no trailers field yet.
                if self.direction == Direction::ServerToClient
                    && let Some(trailers) = frame.trailers_ref()
                {
                    let trailers: Vec<(String, String)> = trailers
                        .iter()
                        .map(|(k, v)| {
                            (
                                k.as_str().to_string(),
                                String::from_utf8_lossy(v.as_bytes()).to_string(),
                            )
                        })
                        .collect();
                    if !trailers.is_empty() {
                        // Non-blocking on purpose: a slow consumer must not stall the body stream.
                        let _ = self.on_flow.try_send(FlowUpdate::ResponseTrailers {
                            flow_id: self.flow_id.clone(),
                            trailers,
                        });
                    }
                }

                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(None) => {
                self.report_once();
                Poll::Ready(None)
            }
            other => other,
        }
    }

    fn is_end_stream(&self) -> bool {
        let ended = self.inner.is_end_stream();
        // Only for a body that actually streamed something. `is_end_stream` is also true before
        // anything is read (an empty body, or a length-delimited one at the moment it is built), and
        // reporting there would record an empty body and then suppress the real report through the
        // once-guard — trading a missing body for a wrong one.
        if ended && self.inner.observed_bytes() > 0 {
            // hyper may treat this as the last word on the body and never poll again, so the capture
            // is written here rather than waiting for a poll that will not come.
            self.report_once();
        }
        ended
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

        // TapBody must report both what it observed: the body, and — separately, because they arrive
        // after it — the trailers. gRPC puts the outcome of a call in the trailers, so a capture that
        // forwards them to the client but never records them cannot say whether the call succeeded.
        let mut saw_body = false;
        let mut recorded_trailers: Option<Vec<(String, String)>> = None;
        while let Ok(event) = rx.try_recv() {
            match event {
                FlowUpdate::HttpBody { body, .. } => {
                    assert_eq!(body.size, 5, "body size should match data");
                    saw_body = true;
                }
                FlowUpdate::ResponseTrailers { trailers, .. } => {
                    recorded_trailers = Some(trailers);
                }
                other => panic!("unexpected update: {other:?}"),
            }
        }

        assert!(saw_body, "TapBody must still report the body it observed");
        assert_eq!(
            recorded_trailers,
            Some(vec![("x-trailer".to_string(), "value".to_string())]),
            "trailers must be reported as an incremental update, not only forwarded"
        );
    }
}

#[cfg(test)]
mod report_once_tests {
    use super::TapBody;
    use http_body_util::BodyExt;
    use hyper::body::{Body, Bytes, Frame};
    use relay_core_api::flow::{Direction, FlowUpdate};
    use std::pin::Pin;
    use std::task::{Context, Poll};

    /// A body that says it is finished without ever yielding the final `None`.
    ///
    /// This is what hyper does with a length-delimited body: it consults `is_end_stream` and stops
    /// polling. Reporting only from `Poll::Ready(None)` therefore lost the body from the capture.
    struct EndsByHint {
        sent: bool,
    }

    impl Body for EndsByHint {
        type Data = Bytes;
        type Error = crate::interceptor::BoxError;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
            if self.sent {
                return Poll::Ready(None);
            }
            self.sent = true;
            Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"payload")))))
        }

        fn is_end_stream(&self) -> bool {
            self.sent
        }
    }

    #[tokio::test]
    async fn a_body_that_ends_by_hint_is_still_reported() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<FlowUpdate>(8);
        let body = TapBody::new(
            EndsByHint { sent: false }
                .map_err(|e| -> crate::interceptor::BoxError { e })
                .boxed(),
            "flow-hint".to_string(),
            tx,
            Direction::ServerToClient,
            1024,
            vec![("content-type".to_string(), "text/plain".to_string())],
        );

        // Read the data frame, then stop — hyper does exactly this and then consults
        // `is_end_stream` instead of polling for the final `None`, so the report must come from there.
        let mut body = body;
        {
            let mut cx = Context::from_waker(std::task::Waker::noop());
            let frame = Pin::new(&mut body).poll_frame(&mut cx);
            assert!(
                matches!(frame, Poll::Ready(Some(Ok(_)))),
                "precondition: the data frame is available"
            );
        }
        assert!(
            body.is_end_stream(),
            "precondition: the body now reports finished"
        );

        let update = rx.try_recv().expect("the body must still be reported");
        match update {
            FlowUpdate::HttpBody { body, .. } => {
                assert_eq!(body.content, "payload");
            }
            other => panic!("expected an HttpBody update, got {other:?}"),
        }
        assert!(rx.try_recv().is_err(), "a body must be reported once");
    }
}
