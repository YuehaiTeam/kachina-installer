use pin_project::pin_project;
use std::{pin::Pin, task::{Context, Poll}};
use tokio::io::{AsyncRead, ReadBuf};

#[pin_project]
pub struct ReadWithCallback<R, F>
where
    R: AsyncRead,
    F: FnMut(usize),
{
    #[pin]
    pub reader: R,
    pub callback: F,
}

impl<R, F> AsyncRead for ReadWithCallback<R, F>
where
    R: AsyncRead,
    F: FnMut(usize),
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<tokio::io::Result<()>> {
        let this = self.project();
        let res = this.reader.poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = res {
            if !buf.filled().is_empty() {
                (this.callback)(buf.filled().len());
            }
        }
        res
    }
}
