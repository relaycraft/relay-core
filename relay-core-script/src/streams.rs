use bytes::Bytes;
use deno_core::{AsyncResult, BufView, Resource};
use futures_util::Stream;
use http_body::{Body, Frame};
use http_body_util::{BodyExt, StreamBody};
use relay_core_lib::interceptor::{BoxError, HttpBody};
use std::borrow::Cow;
use std::io;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::io::AsyncReadExt;
use tokio::sync::Mutex;
use tokio_util::io::StreamReader;

pub struct BodyStreamAdapter {
    body: HttpBody,
}

impl BodyStreamAdapter {
    pub fn new(body: HttpBody) -> Self {
        Self { body }
    }
}

impl Stream for BodyStreamAdapter {
    type Item = io::Result<Bytes>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            match Pin::new(&mut self.body).poll_frame(cx) {
                Poll::Ready(Some(Ok(frame))) => {
                    if let Ok(data) = frame.into_data() {
                        return Poll::Ready(Some(Ok(data)));
                    } else {
                        // Skip non-data frames (trailers)
                        continue;
                    }
                }
                Poll::Ready(Some(Err(e))) => return Poll::Ready(Some(Err(io::Error::other(e)))),
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

type SharedReader = Arc<Mutex<StreamReader<BodyStreamAdapter, Bytes>>>;

pub struct HttpBodyResource {
    reader: SharedReader,
}

impl HttpBodyResource {
    pub fn new(body: HttpBody) -> Self {
        let adapter = BodyStreamAdapter::new(body);
        let reader = StreamReader::new(adapter);
        Self {
            reader: Arc::new(Mutex::new(reader)),
        }
    }

    pub fn clone_reader(&self) -> SharedReader {
        self.reader.clone()
    }
}

impl Resource for HttpBodyResource {
    fn name(&self) -> Cow<'_, str> {
        "httpBody".into()
    }

    fn read(self: Rc<Self>, limit: usize) -> AsyncResult<BufView> {
        let reader = self.reader.clone();
        Box::pin(async move {
            let mut reader = reader.lock().await;
            let mut buf = vec![0; limit];
            let n = reader.read(&mut buf).await?;
            buf.truncate(n);
            Ok(BufView::from(buf))
        })
    }
}

pub fn create_body_from_resource(resource: &HttpBodyResource) -> HttpBody {
    let reader = resource.clone_reader();
    let stream = futures_util::stream::unfold(reader, |reader| async move {
        let mut locked = reader.lock().await;
        let mut buf = vec![0; 4096];
        match locked.read(&mut buf).await {
            Ok(0) => None, // EOF
            Ok(n) => {
                buf.truncate(n);
                Some((Ok(Frame::data(Bytes::from(buf))), reader.clone()))
            }
            Err(e) => Some((Err(Box::new(e) as BoxError), reader.clone())),
        }
    });

    // StreamBody expects Frame<D, E>.
    StreamBody::new(stream).boxed()
}
