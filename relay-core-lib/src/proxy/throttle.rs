use crate::interceptor::{BoxError, HttpBody};
use hyper::body::{Body, Bytes, Frame, SizeHint};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::time::Instant;

/// Wraps a body stream with bandwidth throttling (bytes/sec).
/// Inserts artificial delays between data frames to ensure
/// throughput does not exceed the configured rate.
pub struct ThrottleBody {
    inner: HttpBody,
    bytes_per_sec: u64,
    last_frame_at: Option<Instant>,
}

impl ThrottleBody {
    pub fn new(inner: HttpBody, bytes_per_sec: u64) -> Self {
        Self {
            inner,
            bytes_per_sec,
            last_frame_at: None,
        }
    }
}

impl Body for ThrottleBody {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let frame = match Pin::new(&mut self.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => frame,
            other => return other,
        };

        // Calculate per-frame delay based on data size
        if let Some(data) = frame.data_ref() {
            let bytes = data.len() as u64;
            if bytes > 0 && self.bytes_per_sec > 0 {
                let frame_dur = Duration::from_micros(bytes * 1_000_000 / self.bytes_per_sec);
                let now = Instant::now();

                if let Some(last) = self.last_frame_at {
                    let elapsed = now.duration_since(last);
                    if elapsed < frame_dur {
                        let remaining = frame_dur - elapsed;
                        // Since we can't .await in poll_frame, schedule a wake
                        let waker = cx.waker().clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(remaining).await;
                            waker.wake();
                        });
                        return Poll::Pending;
                    }
                }
                self.last_frame_at = Some(now);
            }
        }

        Poll::Ready(Some(Ok(frame)))
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
    use http_body_util::{BodyExt, Full};
    use hyper::body::Frame;
    use std::pin::Pin;
    use std::task::{Context, Poll, Waker};

    #[tokio::test]
    async fn test_throttle_body_preserves_data() {
        let data = Bytes::from("test-body-data");
        let body: HttpBody = Full::new(data.clone())
            .map_err(|e| -> BoxError { Box::new(e) })
            .boxed();
        // High rate limit — no effective throttling
        let throttled = ThrottleBody::new(body, 1_000_000);
        let collected = throttled.collect().await.unwrap().to_bytes();
        assert_eq!(collected, data);
    }

    #[tokio::test]
    async fn test_throttle_body_passthrough_empty() {
        let body: HttpBody = Full::new(Bytes::new())
            .map_err(|e| -> BoxError { Box::new(e) })
            .boxed();
        let throttled = ThrottleBody::new(body, 1000);
        let collected = throttled.collect().await.unwrap().to_bytes();
        assert_eq!(collected.len(), 0);
    }

    /// Verify ThrottleBody passes trailers through unchanged.
    #[tokio::test]
    async fn test_throttle_body_passes_trailers() {
        /// A body that yields data then trailers then EOF.
        struct TrailerBody {
            phase: u8,
        }

        impl Body for TrailerBody {
            type Data = Bytes;
            type Error = BoxError;

            fn poll_frame(
                mut self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
                match self.phase {
                    0 => {
                        self.phase = 1;
                        Poll::Ready(Some(Ok(Frame::data(Bytes::from("body-data")))))
                    }
                    1 => {
                        self.phase = 2;
                        let mut trailers = hyper::HeaderMap::new();
                        trailers.insert("x-trailer", "present".parse().unwrap());
                        trailers.insert("x-end-stream", "true".parse().unwrap());
                        Poll::Ready(Some(Ok(Frame::trailers(trailers))))
                    }
                    _ => Poll::Ready(None),
                }
            }
        }

        let body: HttpBody = TrailerBody { phase: 0 }
            .map_err(|e| -> BoxError { e })
            .boxed();
        let mut throttled = ThrottleBody::new(body, 1_000_000);

        let mut poll_count = 0;
        let mut data_frames = 0;
        let mut trailer_frames = 0;
        let mut trailers: Option<hyper::HeaderMap> = None;

        let waker = Waker::noop();
        let mut cx = Context::from_waker(&waker);
        loop {
            match Pin::new(&mut throttled).poll_frame(&mut cx) {
                Poll::Ready(Some(Ok(frame))) => {
                    poll_count += 1;
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
                Poll::Pending => panic!("ThrottleBody should not pend at full speed"),
            }
        }

        assert_eq!(poll_count, 2, "should yield data + trailers = 2 frames");
        assert_eq!(data_frames, 1, "should have 1 data frame");
        assert_eq!(trailer_frames, 1, "should have 1 trailers frame");
        let trailers = trailers.expect("trailers should be present");
        assert_eq!(
            trailers.get("x-trailer").and_then(|v| v.to_str().ok()),
            Some("present")
        );
        assert_eq!(
            trailers.get("x-end-stream").and_then(|v| v.to_str().ok()),
            Some("true")
        );
    }
}
