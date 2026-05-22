use crate::interceptor::{BoxError, HttpBody};
use bytes::Bytes;
use hyper::body::{Body, Frame, SizeHint};
use std::pin::Pin;
use std::task::{Context, Poll};

/// Wraps an HttpBody and accumulates up to `budget` bytes while
/// passing through all data. Tracks whether the budget was exceeded.
pub struct BudgetedBody {
    inner: HttpBody,
    budget: usize,
    accumulated: Vec<u8>,
    /// Set to true when accumulated bytes exceed the budget.
    pub budget_exceeded: bool,
    /// Total bytes passed through.
    pub total_bytes: u64,
    done: bool,
}

impl BudgetedBody {
    pub fn new(inner: HttpBody, budget: usize) -> Self {
        Self {
            inner,
            budget,
            accumulated: Vec::with_capacity(budget.min(65_536)),
            budget_exceeded: false,
            total_bytes: 0,
            done: false,
        }
    }

    /// Returns a copy of the accumulated body data (up to budget).
    pub fn accumulated_data(&self) -> Bytes {
        Bytes::from(self.accumulated.clone())
    }
}

impl Body for BudgetedBody {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        if self.done {
            return Poll::Ready(None);
        }

        let frame = match Pin::new(&mut self.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => frame,
            other => {
                self.done = true;
                return other;
            }
        };

        // Track total bytes
        if let Some(data) = frame.data_ref() {
            self.total_bytes += data.len() as u64;

            // Accumulate up to budget for rule inspection
            let remaining = self.budget.saturating_sub(self.accumulated.len());
            if remaining > 0 {
                let take = data.len().min(remaining);
                self.accumulated.extend_from_slice(&data[..take]);
            }
            if self.accumulated.len() >= self.budget {
                self.budget_exceeded = true;
            }
        }

        // Check for end-of-stream via trailers frame
        if frame.is_trailers() {
            self.done = true;
        }

        Poll::Ready(Some(Ok(frame)))
    }

    fn is_end_stream(&self) -> bool {
        self.done || self.inner.is_end_stream()
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

    #[tokio::test]
    async fn test_budgeted_body_within_limit() {
        let data = Bytes::from("hello world");
        let body: HttpBody = Full::new(data.clone())
            .map_err(|e| -> BoxError { Box::new(e) })
            .boxed();
        let mut budgeted = BudgetedBody::new(body, 100);
        let collected = (&mut budgeted).collect().await.unwrap().to_bytes();
        assert_eq!(collected, data);
        assert!(!budgeted.budget_exceeded);
        assert_eq!(budgeted.accumulated_data(), data);
    }

    #[tokio::test]
    async fn test_budgeted_body_exceeds_limit() {
        let data = Bytes::from("this is a long message that exceeds the budget");
        let body: HttpBody = Full::new(data.clone())
            .map_err(|e| -> BoxError { Box::new(e) })
            .boxed();
        let mut budgeted = BudgetedBody::new(body, 10);
        let collected = (&mut budgeted).collect().await.unwrap().to_bytes();
        // All data still passes through
        assert_eq!(collected, data);
        // Budget was exceeded
        assert!(budgeted.budget_exceeded);
        // Only first 10 bytes accumulated
        assert_eq!(budgeted.accumulated_data(), Bytes::from("this is a "));
    }
}
