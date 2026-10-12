use crate::interceptor::{BoxError, HttpBody};
use crate::proxy::body_codec::{process_arrived_body, process_body_with_framing};
use crate::proxy::body_plan::{PrefixBuffer, buffer_prefix};
use hyper::body::{Body, Bytes, Frame, SizeHint};
use relay_core_api::flow::{BodyData, Direction, FlowUpdate};
use relay_core_api::sse_stream::SseParser;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::mpsc::Sender;

/// Is this `Content-Type` an event stream?
///
/// Parameters are allowed (`text/event-stream; charset=utf-8`), and the comparison is on the media
/// type only — a stream whose type carries a charset is still a stream, and refusing it would leave
/// the one body shape that most needs parsing unparsed.
pub fn is_event_stream_content_type(value: &str) -> bool {
    value
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .eq_ignore_ascii_case("text/event-stream")
}

/// Streaming body observer: forwards every frame untouched while retaining a bounded prefix.
///
/// Prefix retention is delegated to [`crate::proxy::body_plan::buffer_prefix`] so this observation
/// path and [`BodyPlan::Capture`](relay_core_api::body_plan::BodyPlan) share one implementation.
/// Bytes observed on a body, handed to a task that does the framing work.
pub struct ObservedBody {
    pub flow_id: String,
    pub direction: Direction,
    pub bytes: Vec<u8>,
    pub total_bytes: u64,
    pub truncated: bool,
    pub headers: Vec<(String, String)>,
}

pub struct TapBody {
    inner: PrefixBuffer,
    flow_id: String,
    on_flow: Sender<FlowUpdate>,
    direction: Direction,
    headers: Vec<(String, String)>,
    /// Where observed bytes are handed off, so the framing work happens outside this body's callbacks.
    observed: Option<tokio::sync::mpsc::UnboundedSender<ObservedBody>>,
    /// Whether the recorded body has been reported yet.
    ///
    /// The report must happen exactly once, and it must happen even when the consumer never polls for
    /// the final `None`: hyper finishes a body as soon as `is_end_stream()` says so, so relying on the
    /// end-of-stream poll alone silently loses the body from the capture.
    reported: std::sync::atomic::AtomicBool,
    /// Complete gRPC messages already pushed while the stream is still open.
    grpc_noted: std::sync::atomic::AtomicU32,
    /// Incremental parser for a `text/event-stream` response (roadmap §7.2).
    ///
    /// `None` for every other body, so the common path pays nothing for a feature only SSE needs.
    /// The parser is here rather than in the reporting task because its state is per-byte-stream:
    /// splitting an event across chunks is the whole problem, and only the body sees the chunks.
    sse: Option<SseParser>,
}

impl TapBody {
    pub fn new(
        inner: HttpBody,
        flow_id: String,
        on_flow: Sender<FlowUpdate>,
        direction: Direction,
        limit: usize,
        headers: Vec<(String, String)>,
        observed_tx: Option<tokio::sync::mpsc::UnboundedSender<ObservedBody>>,
    ) -> Self {
        crate::metrics::inc_proxy_stream_mode_tap();
        Self {
            inner: buffer_prefix(inner, limit),
            flow_id,
            on_flow,
            direction,
            headers,
            reported: std::sync::atomic::AtomicBool::new(false),
            grpc_noted: std::sync::atomic::AtomicU32::new(0),
            observed: observed_tx,
            sse: None,
        }
    }

    /// Parse this body as a `text/event-stream` response.
    ///
    /// Only the response direction can be one, and only when the peer said so: the content type is
    /// the peer's claim, and parsing a body that is not an event stream would report nonsense as
    /// events. Anything that is not exactly `text/event-stream` (parameters allowed) is left alone.
    pub fn tap_event_stream(mut self) -> Self {
        let is_event_stream = self.direction == Direction::ServerToClient
            && self.headers.iter().any(|(name, value)| {
                name.eq_ignore_ascii_case("content-type") && is_event_stream_content_type(value)
            });
        if is_event_stream {
            self.sse = Some(SseParser::default());
            crate::metrics::inc_proxy_sse_streams();
        }
        self
    }

    /// Report the body once, from whichever signal arrives first.
    ///
    /// Two signals because either can be the last one a consumer sees: the end-of-stream poll, and
    /// `is_end_stream()` (which hyper consults to decide the body is finished, and which does not
    /// require another poll). Reporting from only one of them loses bodies from the capture.
    fn report_once(&mut self) {
        use std::sync::atomic::Ordering;
        if self.reported.swap(true, Ordering::Relaxed) {
            return;
        }

        // Close the event-stream parse. A trailing event with no blank line is not dispatched (that
        // is what the spec says a client does), but it is counted: a capture that stops mid-event is
        // exactly what someone debugging a dropped connection needs to see said out loud.
        if let Some(parser) = self.sse.as_mut() {
            let losses = parser.finish();
            if losses.truncated_at_end {
                crate::metrics::inc_proxy_sse_truncated();
            }
            for _ in 0..losses.oversized {
                crate::metrics::inc_proxy_sse_events_dropped();
            }
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
            truncated: false,
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

    /// Push complete gRPC messages as they arrive, without marking the body finished.
    ///
    /// The final report still happens once, from the end of the stream. Setting `reported` here would
    /// drop that final body.
    fn note_arrived_grpc(&self) {
        use std::sync::atomic::Ordering;
        if self.reported.load(Ordering::Relaxed) {
            return;
        }
        let snapshot = self.inner.snapshot();
        let (_, _, grpc) = process_arrived_body(&snapshot.bytes, &self.headers);
        let Some(grpc) = grpc else {
            return;
        };
        let count = grpc.messages.len() as u32;
        if count == 0 || count <= self.grpc_noted.load(Ordering::Relaxed) {
            return;
        }
        self.grpc_noted.store(count, Ordering::Relaxed);
        let (encoding, content, grpc) = process_arrived_body(&snapshot.bytes, &self.headers);
        let _ = self.on_flow.try_send(FlowUpdate::HttpBody {
            flow_id: self.flow_id.clone(),
            direction: self.direction.clone(),
            truncated: snapshot.truncated,
            body: BodyData {
                encoding,
                content,
                size: snapshot.total_bytes,
                grpc,
            },
        });
    }

    /// A stream reset is not a body that ran out of bytes. Record what arrived, then the reset.
    fn note_stream_reset(&self, detail: String) {
        use std::sync::atomic::Ordering;
        if !self.reported.swap(true, Ordering::Relaxed) {
            let snapshot = self.inner.snapshot();
            if snapshot.total_bytes > 0 {
                let (encoding, content, grpc) =
                    process_arrived_body(&snapshot.bytes, &self.headers);
                let _ = self.on_flow.try_send(FlowUpdate::HttpBody {
                    flow_id: self.flow_id.clone(),
                    direction: self.direction.clone(),
                    truncated: snapshot.truncated,
                    body: BodyData {
                        encoding,
                        content,
                        size: snapshot.total_bytes,
                        grpc,
                    },
                });
            }
        }
        let _ = self.on_flow.try_send(FlowUpdate::StreamReset {
            flow_id: self.flow_id.clone(),
            detail,
        });
    }
}

fn stream_reset_detail(err: &crate::interceptor::BoxError) -> Option<String> {
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(err.as_ref());
    while let Some(err) = current {
        if let Some(h2_err) = err.downcast_ref::<h2::Error>()
            && (h2_err.is_reset() || h2_err.reason() == Some(h2::Reason::CANCEL))
        {
            return Some(h2_err.to_string());
        }
        current = err.source();
    }
    None
}

impl Drop for TapBody {
    fn drop(&mut self) {
        // The signal that covers every body, including an h2 stream hyper ends without a final poll.
        // It only moves the retained buffer and hands it to a task: framing and the update happen
        // there, which keeps those bytes out of the connection task's critical path and keeps raw
        // bytes out of `FlowUpdate`, which is a public wire contract.
        if self.reported.load(std::sync::atomic::Ordering::Relaxed) {
            return; // already reported from a poll; handing over again would add an empty body
        }
        if let Some(tx) = &self.observed {
            let (bytes, total_bytes, truncated) = self.inner.take_retained();
            let _ = tx.send(ObservedBody {
                flow_id: self.flow_id.clone(),
                direction: self.direction.clone(),
                bytes,
                total_bytes,
                truncated,
                headers: self.headers.clone(),
            });
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

                // A body hyper finishes without a final poll — which is how it treats a
                // length-delimited body — never reached the `Ready(None)` branch below, so those
                // flows were once captured with no body at all. Completion is read from the size
                // hint, a pure query, and `Ready(None)` still covers everything else; an h2 stream
                // has no length, and for that case the report comes from `Drop` (see the impl
                // below), which every body reaches.
                //
                // This paragraph previously described the fix as still outstanding, which stopped
                // being true when the two signals landed — a comment that contradicts the code
                // beneath it is worse than no comment, because it is read as the reason the code
                // looks the way it does.
                //
                // The history is worth keeping because it cost four wrong conclusions: three
                // attempts were written up here as proof that calling `is_end_stream()` inside a
                // poll is unsafe, and they were not. The test they "hung" on handed the proxy a
                // ten-slot flow channel that nobody read, and `send(..).await` blocked once it
                // filled; the baseline sat just under the limit, so any change that added one
                // update looked like a deadlock in the proxy.
                if let Some(data) = frame.data_ref()
                    && !data.is_empty()
                {
                    self.note_arrived_grpc();

                    // Parse what arrived and hand on whatever became complete. The work is bounded by
                    // the bytes just seen, and the send is non-blocking for the same reason the
                    // trailers above are: a slow consumer must not stall the response.
                    let dispatched = match self.sse.as_mut() {
                        Some(parser) => parser.push(data),
                        None => Vec::new(),
                    };
                    for event in dispatched {
                        crate::metrics::inc_proxy_sse_events();
                        let _ = self.on_flow.try_send(FlowUpdate::SseEvent {
                            flow_id: self.flow_id.clone(),
                            event,
                        });
                    }
                }

                if self.inner.size_hint().exact() == Some(0) {
                    self.as_mut().get_mut().report_once();
                }

                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(None) => {
                self.as_mut().get_mut().report_once();
                Poll::Ready(None)
            }
            Poll::Ready(Some(Err(err))) => {
                if let Some(detail) = stream_reset_detail(&err) {
                    self.note_stream_reset(detail);
                }
                Poll::Ready(Some(Err(err)))
            }
            other => other,
        }
    }

    fn is_end_stream(&self) -> bool {
        // Deliberately a pure query. An earlier version reported the body from here too, to cover
        // bodies that hyper finishes via this signal without a final poll, but taking the prefix
        // buffer's lock inside a predicate that hyper may evaluate at any point deadlocked
        // `test_h1_concurrent_connections` (it hung indefinitely; reverting this one method made it
        // pass in 0.21s). Reporting from `poll_frame`'s end-of-stream branch is the safe half, and
        // bodies that end by hint are recorded by the next poll when one happens.
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

    /// A body that is never polled to `None` must still be recorded.
    ///
    /// hyper ends a length-delimited body once the size hint says nothing is left, and does not poll
    /// again, so the `Ready(None)` branch never runs and the capture had no body for those flows.
    ///
    /// It is also the regression test for the hang: deciding completion from `is_end_stream()` — in the
    /// predicate or from inside the poll — left `test_h1_concurrent_connections` idle forever, because
    /// that call is not the pure query it looks like.
    #[tokio::test]
    async fn a_body_that_ends_by_hint_is_reported() {
        /// One data frame, then a size hint of zero and a poll that never completes.
        struct EndsByHint {
            sent: bool,
        }

        impl hyper::body::Body for EndsByHint {
            type Data = Bytes;
            type Error = BoxError;

            fn poll_frame(
                mut self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
                if self.sent {
                    return Poll::Pending; // never reports an end-of-stream of its own
                }
                self.sent = true;
                Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"payload")))))
            }

            fn size_hint(&self) -> hyper::body::SizeHint {
                hyper::body::SizeHint::with_exact(if self.sent { 0 } else { 7 })
            }
        }

        let (tx, mut rx) = tokio::sync::mpsc::channel::<FlowUpdate>(8);
        let mut body = TapBody::new(
            EndsByHint { sent: false }.boxed(),
            "flow-hint".to_string(),
            tx,
            Direction::ServerToClient,
            1024,
            vec![("content-type".to_string(), "text/plain".to_string())],
            None,
        );

        // Exactly one poll, the way a consumer that trusts the size hint behaves.
        let mut cx = Context::from_waker(Waker::noop());
        let frame = Pin::new(&mut body).poll_frame(&mut cx);
        assert!(
            matches!(frame, Poll::Ready(Some(Ok(_)))),
            "the data frame is available"
        );

        let update = rx
            .try_recv()
            .expect("the body must be reported after the poll");
        match update {
            FlowUpdate::HttpBody { body, .. } => assert_eq!(body.content, "payload"),
            other => panic!("expected an HttpBody update, got {other:?}"),
        }
        assert!(rx.try_recv().is_err(), "and reported only once");
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
            None,
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

    fn grpc_frame(payload: &[u8]) -> Bytes {
        let mut out = vec![0u8];
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(payload);
        Bytes::from(out)
    }

    #[tokio::test]
    async fn grpc_messages_are_reported_as_they_arrive() {
        struct TwoMessages {
            n: u8,
        }

        impl hyper::body::Body for TwoMessages {
            type Data = Bytes;
            type Error = BoxError;

            fn poll_frame(
                mut self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
                let payload = match self.n {
                    0 => "one",
                    1 => "two",
                    _ => return Poll::Ready(None),
                };
                self.n += 1;
                Poll::Ready(Some(Ok(Frame::data(grpc_frame(payload.as_bytes())))))
            }
        }

        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let mut body = TapBody::new(
            TwoMessages { n: 0 }.boxed(),
            "flow-stream".to_string(),
            tx,
            Direction::ServerToClient,
            1024,
            vec![("content-type".to_string(), "application/grpc".to_string())],
            None,
        );
        let mut cx = Context::from_waker(Waker::noop());

        assert!(Pin::new(&mut body).poll_frame(&mut cx).is_ready());
        match rx.try_recv().expect("first message") {
            FlowUpdate::HttpBody { body, .. } => {
                assert_eq!(body.grpc.expect("framed").messages.len(), 1);
            }
            other => panic!("expected body, got {other:?}"),
        }

        assert!(Pin::new(&mut body).poll_frame(&mut cx).is_ready());
        match rx.try_recv().expect("second message") {
            FlowUpdate::HttpBody { body, .. } => {
                let grpc = body.grpc.expect("framed");
                assert_eq!(grpc.messages.len(), 2);
                assert_eq!(grpc.messages[1].text.as_deref(), Some("two"));
            }
            other => panic!("expected body, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_stream_reset_is_not_a_truncated_body() {
        struct ResetAfterMessage {
            sent: bool,
        }

        impl hyper::body::Body for ResetAfterMessage {
            type Data = Bytes;
            type Error = BoxError;

            fn poll_frame(
                mut self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
                if !self.sent {
                    self.sent = true;
                    return Poll::Ready(Some(Ok(Frame::data(grpc_frame(b"hi")))));
                }
                Poll::Ready(Some(Err(Box::new(h2::Error::from(h2::Reason::CANCEL)))))
            }
        }

        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let mut body = TapBody::new(
            ResetAfterMessage { sent: false }.boxed(),
            "flow-reset".to_string(),
            tx,
            Direction::ServerToClient,
            1024,
            vec![("content-type".to_string(), "application/grpc".to_string())],
            None,
        );
        let mut cx = Context::from_waker(Waker::noop());
        assert!(Pin::new(&mut body).poll_frame(&mut cx).is_ready());
        let _ = rx.try_recv();

        let err = Pin::new(&mut body).poll_frame(&mut cx);
        assert!(matches!(err, Poll::Ready(Some(Err(_)))));

        let mut saw_reset = false;
        let mut kept_message = false;
        while let Ok(update) = rx.try_recv() {
            match update {
                FlowUpdate::HttpBody { body, .. } => {
                    let grpc = body.grpc.expect("reset keeps arrived messages");
                    assert_eq!(grpc.messages.len(), 1);
                    assert_eq!(grpc.messages[0].text.as_deref(), Some("hi"));
                    kept_message = true;
                }
                FlowUpdate::StreamReset { detail, .. } => {
                    assert!(!detail.is_empty(), "a reset records the HTTP/2 reason");
                    saw_reset = true;
                }
                other => panic!("unexpected {other:?}"),
            }
        }
        assert!(
            kept_message,
            "the message that arrived before the reset is kept"
        );
        assert!(saw_reset, "the reset is recorded on its own");
    }
}
