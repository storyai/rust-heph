#![allow(dead_code, unused_macros)] // Not all tests use all functions/types.

use std::env::temp_dir;
use std::fmt;
use std::fs::{create_dir_all, remove_dir_all};
use std::future::Future;
use std::mem::size_of;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::stream::Stream;
use std::sync::Once;
use std::task::{self, Poll};

macro_rules! limited_loop {
    ($($arg: tt)*) => {{
        let mut range = (0..1_000);
        while range.next().is_some() {
            $($arg)*
        }

        if range.is_empty() {
            panic!("looped too many iterations");
        }
    }}
}

pub fn assert_send<T: Send>() {}

pub fn assert_sync<T: Sync>() {}

#[track_caller]
pub fn assert_size<T>(expected: usize) {
    assert_eq!(size_of::<T>(), expected);
}

/// Bind to any IPv4 port on localhost.
pub fn any_local_address() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

/// Bind to any IPv6 port on localhost.
pub fn any_local_ipv6_address() -> SocketAddr {
    "[::1]:0".parse().unwrap()
}

/// Returns an address to which the connection will be refused.
pub fn refused_address() -> SocketAddr {
    "0.0.0.0:1".parse().unwrap()
}

/// Returns a path to a non-existing temporary file.
pub fn temp_file(name: &str) -> PathBuf {
    static CLEANUP: Once = Once::new();

    let mut dir = temp_dir();
    dir.push("heph.test/");

    CLEANUP.call_once(|| {
        let _ = remove_dir_all(&dir);
        if let Err(err) = create_dir_all(&dir) {
            panic!("failed to create temporary directory: {}", err);
        }
    });

    dir.push(name);
    dir
}

#[track_caller]
pub fn expect_pending<T>(poll: Poll<T>)
where
    T: fmt::Debug,
{
    match poll {
        Poll::Pending => {} // Ok.
        Poll::Ready(value) => panic!("expected pending, got `Poll::Ready({:?})`", value),
    }
}

#[track_caller]
pub fn expect_ready<T>(poll: Poll<T>, expected: T)
where
    T: fmt::Debug + PartialEq,
{
    match poll {
        Poll::Pending => panic!("unexpected `Poll::Pending`"),
        Poll::Ready(value) => assert_eq!(value, expected),
    }
}

#[track_caller]
pub fn expect_ready_ok<T, E>(poll: Poll<Result<T, E>>, expected: T)
where
    T: fmt::Debug + PartialEq,
    E: fmt::Display,
{
    match poll {
        Poll::Pending => panic!("unexpected `Poll::Pending`"),
        Poll::Ready(Ok(value)) => assert_eq!(value, expected),
        Poll::Ready(Err(err)) => panic!("unexpected error: {}", err),
    }
}

#[track_caller]
pub fn is_ready<E>(poll: Poll<Result<(), E>>) -> bool
where
    E: fmt::Display,
{
    match poll {
        Poll::Ready(Ok(())) => true,
        Poll::Ready(Err(err)) => panic!("unexpected error: {}", err),
        Poll::Pending => false,
    }
}

/// Returns a [`Future`] that return [`Poll::Pending`] once, without waking
/// itself.
pub const fn pending_once() -> PendingOnce {
    PendingOnce(false)
}

pub struct PendingOnce(bool);

impl Future for PendingOnce {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, _: &mut task::Context<'_>) -> Poll<Self::Output> {
        if self.0 {
            Poll::Ready(())
        } else {
            self.0 = true;
            Poll::Pending
        }
    }
}

/// Returns a [`Future`] or [`Stream`] that counts the number of times it is
/// polled before returning a value.
///
/// # Notes
///
/// For streams it always returns the total number of polls, not the count
/// in between return two items.
pub const fn count_polls<T>(inner: T) -> CountPolls<T> {
    CountPolls { count: 0, inner }
}

pub struct CountPolls<T> {
    count: usize,
    inner: T,
}

impl<Fut> Future for CountPolls<Fut>
where
    Fut: Future,
{
    type Output = (Fut::Output, usize);

    fn poll(self: Pin<&mut Self>, ctx: &mut task::Context<'_>) -> Poll<Self::Output> {
        // Safety: this is safe because we're not moving the future.
        let this = unsafe { Pin::into_inner_unchecked(self) };
        this.count += 1;
        let future = unsafe { Pin::new_unchecked(&mut this.inner) };
        future.poll(ctx).map(|out| (out, this.count))
    }
}

impl<S> Stream for CountPolls<S>
where
    S: Stream,
{
    type Item = (S::Item, usize);

    fn poll_next(self: Pin<&mut Self>, ctx: &mut task::Context<'_>) -> Poll<Option<Self::Item>> {
        // Safety: this is safe because we're not moving the future.
        let this = unsafe { Pin::into_inner_unchecked(self) };
        this.count += 1;
        let stream = unsafe { Pin::new_unchecked(&mut this.inner) };
        stream
            .poll_next(ctx)
            .map(|out| out.map(|out| (out, this.count)))
    }
}
