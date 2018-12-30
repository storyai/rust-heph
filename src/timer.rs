//! Module with time related utilities.
//!
//! This module provides three types [`Timer`], [`Deadline`] and [`Interval`].
//!
//! `Timer` is a stand-alone future that returns [`DeadlinePassed`] once the
//! deadline has passed. `Deadline` wraps another `Future` and checks the
//! deadline each time it's polled, it returns `Err(DeadlinePassed)` once the
//! deadline has passed.
//!
//! `Interval` implements `Stream` which yields an item after the deadline has
//! passed each interval.
//!
//! [`Timer`]: struct.Timer.html
//! [`Deadline`]: struct.Deadline.html
//! [`Interval`]: struct.Interval.html
//! [`DeadlinePassed`]: struct.DeadlinePassed.html

use std::future::Future;
use std::pin::Pin;
use std::task::{LocalWaker, Poll};
use std::time::{Duration, Instant};

use futures_core::stream::{Stream, FusedStream};

use crate::actor::ActorContext;
use crate::process::ProcessId;
use crate::system::ActorSystemRef;

/// Type returned when the deadline has passed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DeadlinePassed;

/// A future that represents a timer.
///
/// If this future returns `Poll::Ready(DeadlinePassed)` it means that the
/// deadline has passed. If it returns `Poll::Pending` it's not yet passed.
///
/// # Examples
///
/// Using the `select!` macro to add a timeout to receiving a message.
///
/// ```ignore
/// #![feature(async_await, await_macro, futures_api, never_type)]
///
/// use std::time::Duration;
///
/// use futures_util::future::FutureExt;
/// use futures_util::select;
/// use heph::actor::ActorContext;
/// use heph::timer::Timer;
///
/// async fn print_actor(mut ctx: ActorContext<String>) -> Result<(), !> {
///     loop {
///         // Create a timer, this will be ready once the timeout has passed.
///         let mut timeout = Timer::timeout(&mut ctx, Duration::from_millis(100)).fuse();
///         // Create a future to receive a message.
///         let mut msg_future = ctx.receive().fuse();
///
///         // Now let them race!
///         // This is basically a match statement for futures, whichever
///         // future returns first will be the winner and we'll take that
///         // branch.
///         let msg = select! {
///             msg = msg_future => msg,
///             _ = timeout => {
///                 println!("Getting impatient!");
///                 continue;
///             },
///         };
///
///         println!("Got a message: {}", msg);
///     }
/// }
/// ```
#[derive(Debug)]
pub struct Timer {
    deadline: Instant,
}

impl Timer {
    /// Create a new `Timer`.
    pub fn new<M>(ctx: &mut ActorContext<M>, deadline: Instant) -> Timer {
        let pid = ctx.pid();
        ctx.system_ref().add_deadline(pid, deadline);
        Timer {
            deadline,
        }
    }

    /// Create a new timer, based on a timeout.
    ///
    /// Same as calling `Timer::new(&mut ctx, Instant::now() + timeout)`.
    pub fn timeout<M>(ctx: &mut ActorContext<M>, timeout: Duration) -> Timer {
        Timer::new(ctx, Instant::now() + timeout)
    }

    /// Returns the deadline set for this `Timer`.
    pub fn deadline(&self) -> Instant {
        self.deadline
    }
}

impl Future for Timer {
    type Output = DeadlinePassed;

    fn poll(self: Pin<&mut Self>, _waker: &LocalWaker) -> Poll<Self::Output> {
        if self.deadline <= Instant::now() {
            Poll::Ready(DeadlinePassed)
        } else {
            Poll::Pending
        }
    }
}

/// A future that represents a deadline.
///
/// When this future is polled it first checks if the deadline has passed, if so
/// it returns `Poll::Ready(Err(DeadlinePassed))`. Otherwise this will poll the
/// provided future.
///
/// # Examples
///
/// Setting a timeout for a future.
///
/// ```
/// #![feature(async_await, await_macro, futures_api, never_type)]
///
/// # use std::future::Future;
/// # use std::pin::Pin;
/// # use std::task::{LocalWaker, Poll};
/// use std::thread::sleep;
/// use std::time::Duration;
///
/// use futures_util::select;
/// use heph::actor::ActorContext;
/// use heph::timer::{DeadlinePassed, Deadline};
///
/// # struct OtherFuture;
/// #
/// # impl Future for OtherFuture {
/// #     type Output = ();
/// #     fn poll(self: Pin<&mut Self>, _waker: &LocalWaker) -> Poll<Self::Output> {
/// #         Poll::Pending
/// #     }
/// # }
/// #
/// async fn print_actor(mut ctx: ActorContext<String>) -> Result<(), !> {
///     // `OtherFuture` is a type that implements `Future`.
///     let future = OtherFuture;
///     // Create our deadline.
///     let deadline_future = Deadline::timeout(&mut ctx, Duration::from_millis(20), future);
///
///     // Now we await the results.
///     let result = await!(deadline_future);
///     // However the other future is rather slow, so the timeout will pass.
///     assert_eq!(result, Err(DeadlinePassed));
///     Ok(())
/// }
/// ```
#[derive(Debug)]
pub struct Deadline<Fut> {
    deadline: Instant,
    fut: Fut,
}

impl<Fut> Deadline<Fut> {
    /// Create a new `Deadline`.
    pub fn new<M>(ctx: &mut ActorContext<M>, deadline: Instant, fut: Fut) -> Deadline<Fut> {
        let pid = ctx.pid();
        ctx.system_ref().add_deadline(pid, deadline);
        Deadline {
            deadline,
            fut,
        }
    }

    /// Create a new deadline based on a timeout.
    ///
    /// Same as calling `Deadline::new(&mut ctx, Instant::now() + timeout, fut)`.
    pub fn timeout<M>(ctx: &mut ActorContext<M>, timeout: Duration, fut: Fut) -> Deadline<Fut> {
        Deadline::new(ctx, Instant::now() + timeout, fut)
    }

    /// Returns the deadline set for this `Deadline`.
    pub fn deadline(&self) -> Instant {
        self.deadline
    }
}

impl<Fut> Future for Deadline<Fut>
    where Fut: Future,
{
    type Output = Result<Fut::Output, DeadlinePassed>;

    fn poll(self: Pin<&mut Self>, waker: &LocalWaker) -> Poll<Self::Output> {
        if self.deadline <= Instant::now() {
            Poll::Ready(Err(DeadlinePassed))
        } else {
            let future = unsafe { Pin::map_unchecked_mut(self, |this| &mut this.fut) };
            future.poll(waker).map(Ok)
        }
    }
}

/// A stream that yields an item after a delay has passed.
///
/// This stream will never return `None`, it will always set another deadline
/// and yield another item after the deadline has passed.
///
/// # Notes
///
/// The next deadline will always will be set after this returns `Poll::Ready`.
/// This means that if the interval is very short and the stream is not polled
/// often enough it's possible that actual time between yielding two values can
/// become bigger then the specified interval.
///
/// # Examples
///
/// The following example will print hello world every second.
///
/// ```
/// #![feature(async_await, await_macro, futures_api, never_type)]
///
/// use std::time::Duration;
///
/// use futures_util::future::ready;
/// use futures_util::stream::StreamExt;
/// use heph::actor::ActorContext;
/// use heph::timer::Interval;
///
/// async fn print_actor(mut ctx: ActorContext<String>) -> Result<(), !> {
///     let interval = Interval::new(&mut ctx, Duration::from_secs(1));
///     await!(interval.for_each(|_| {
///         println!("Hello world");
///         ready(())
///     }));
///
///     Ok(())
/// }
/// ```
#[derive(Debug)]
pub struct Interval {
    interval: Duration,
    deadline: Instant,
    pid: ProcessId,
    system_ref: ActorSystemRef,
}

impl Interval {
    /// Create a new `Interval`.
    pub fn new<M>(ctx: &mut ActorContext<M>, interval: Duration) -> Interval {
        let deadline = Instant::now() + interval;
        let mut system_ref = ctx.system_ref().clone();
        let pid = ctx.pid();
        system_ref.add_deadline(pid, deadline);
        Interval {
            interval,
            deadline,
            pid,
            system_ref,
        }
    }

    /// Returns the next deadline for this `Interval`.
    pub fn next_deadline(&self) -> Instant {
        self.deadline
    }
}

impl Stream for Interval {
    type Item = DeadlinePassed;

    fn poll_next(self: Pin<&mut Self>, _waker: &LocalWaker) -> Poll<Option<Self::Item>> {
        if self.deadline <= Instant::now() {
            // Determine the next deadline.
            let next_deadline = Instant::now() + self.interval;
            let this = Pin::get_mut(self);
            this.deadline = next_deadline;
            this.system_ref.add_deadline(this.pid, next_deadline);
            Poll::Ready(Some(DeadlinePassed))
        } else {
            Poll::Pending
        }
    }
}

impl FusedStream for Interval {
    fn is_terminated(&self) -> bool {
        false
    }
}
