//! Module containing the `Context` and related types.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::task::{self, Poll};

use heph_inbox::{self as inbox, Receiver, RecvValue};

use crate::actor_ref::ActorRef;

/// The context in which an actor is executed.
///
/// This context can be used for a number of things including receiving
/// messages and getting access to the runtime.
///
/// The `actor::Context` comes in two flavours:
/// * [`ThreadLocal`] (default) is the optimised version, but doesn't allow the
///   actor to move between threads. Actor started with
///   [`RuntimeRef::try_spawn_local`] will get this flavour of context.
/// * [`ThreadSafe`] is the flavour that allows the actor to be moved between
///   threads. Actor started with [`RuntimeRef::try_spawn`] will get this
///   flavour of context.
///
/// [`ThreadLocal`]: crate::rt::ThreadLocal
/// [`RuntimeRef::try_spawn_local`]: crate::rt::RuntimeRef::try_spawn_local
/// [`ThreadSafe`]: crate::rt::ThreadSafe
/// [`RuntimeRef::try_spawn`]: crate::rt::RuntimeRef::try_spawn
#[derive(Debug)]
pub struct Context<M, RT> {
    /// Inbox of the actor, shared between this and zero or more actor
    /// references.
    ///
    /// This field is public because it is used by `TcpServer`, as we don't need
    /// entire context there.
    pub(crate) inbox: Receiver<M>,
    /// Runtime access.
    rt: RT,
}

impl<M, RT> Context<M, RT> {
    /// Create a new `actor::Context`.
    pub(crate) const fn new(inbox: Receiver<M>, rt: RT) -> Context<M, RT> {
        Context { inbox, rt }
    }

    /// Attempt to receive the next message.
    ///
    /// This will attempt to receive next message if one is available. If the
    /// actor wants to wait until a message is received [`receive_next`] can be
    /// used, which returns a `Future<Output = M>`.
    ///
    /// [`receive_next`]: Context::receive_next
    ///
    /// # Examples
    ///
    /// An actor that receives a name to greet, or greets the entire world.
    ///
    /// ```
    /// use heph::actor;
    /// use heph::rt::ThreadLocal;
    ///
    /// async fn greeter_actor(mut ctx: actor::Context<String, ThreadLocal>) {
    ///     if let Ok(name) = ctx.try_receive_next() {
    ///         println!("Hello: {}", name);
    ///     } else {
    ///         println!("Hello world");
    ///     }
    /// }
    ///
    /// # // Use the `greeter_actor` function to silence dead code warning.
    /// # drop(greeter_actor);
    /// ```
    pub fn try_receive_next(&mut self) -> Result<M, RecvError> {
        self.inbox.try_recv().map_err(RecvError::from)
    }

    /// Receive the next message.
    ///
    /// This returns a [`Future`] that will complete once a message is ready.
    ///
    /// # Examples
    ///
    /// An actor that await a message and prints it.
    ///
    /// ```
    /// use heph::actor;
    /// use heph::rt::ThreadLocal;
    ///
    /// async fn print_actor(mut ctx: actor::Context<String, ThreadLocal>) {
    ///     if let Ok(msg) = ctx.receive_next().await {
    ///         println!("Got a message: {}", msg);
    ///     }
    /// }
    ///
    /// # // Use the `print_actor` function to silence dead code warning.
    /// # drop(print_actor);
    /// ```
    ///
    /// Same as the example above, but this actor will only wait for a limited
    /// amount of time.
    ///
    /// ```
    /// use std::time::Duration;
    ///
    /// use heph::actor;
    /// use heph::timer::Timer;
    /// use heph::util::either;
    /// use heph::rt::ThreadLocal;
    ///
    /// async fn print_actor(mut ctx: actor::Context<String, ThreadLocal>) {
    ///     // Create a timer, this will be ready once the timeout has
    ///     // passed.
    ///     let timeout = Timer::after(&mut ctx, Duration::from_millis(100));
    ///     // Create a future to receive a message.
    ///     let msg_future = ctx.receive_next();
    ///
    ///     // Now let them race!
    ///     match either(msg_future, timeout).await {
    ///         Ok(Ok(msg)) => println!("Got a message: {}", msg),
    ///         Ok(Err(_)) => println!("No message"),
    ///         Err(_) => println!("Timed out receiving message"),
    ///     }
    /// }
    ///
    /// # // Use the `print_actor` function to silence dead code warning.
    /// # drop(print_actor);
    /// ```
    pub fn receive_next<'ctx>(&'ctx mut self) -> ReceiveMessage<'ctx, M> {
        ReceiveMessage {
            recv: self.inbox.recv(),
        }
    }

    /// Returns a reference to this actor.
    pub fn actor_ref(&self) -> ActorRef<M> {
        ActorRef::local(self.inbox.new_sender())
    }

    /// Get access to the runtime this actor is running in.
    pub fn runtime(&mut self) -> &mut RT {
        &mut self.rt
    }

    pub(crate) const fn runtime_ref(&self) -> &RT {
        &self.rt
    }

    /// Sets the waker of the inbox to `waker`.
    pub(crate) fn register_inbox_waker(&mut self, waker: &task::Waker) {
        let _ = self.inbox.register_waker(waker);
    }
}

/// Error returned in case receiving a value from an actor's inbox fails.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RecvError {
    /// Inbox is empty.
    Empty,
    /// All [`ActorRef`]s  are disconnected and the inbox is empty.
    Disconnected,
}

impl RecvError {
    pub(crate) const fn from(err: inbox::RecvError) -> RecvError {
        match err {
            inbox::RecvError::Empty => RecvError::Empty,
            inbox::RecvError::Disconnected => RecvError::Disconnected,
        }
    }
}

/// Future to receive a single message.
///
/// The implementation behind and [`actor::Context::receive_next`].
///
/// [`actor::Context::receive_next`]: crate::actor::Context::receive_next
#[derive(Debug)]
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct ReceiveMessage<'ctx, M> {
    recv: RecvValue<'ctx, M>,
}

impl<'ctx, M> Future for ReceiveMessage<'ctx, M> {
    type Output = Result<M, NoMessages>;

    fn poll(mut self: Pin<&mut Self>, ctx: &mut task::Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.recv)
            .poll(ctx)
            .map(|r| r.ok_or(NoMessages))
    }
}

/// Returned when an actor's inbox has no messages and no references to the
/// actor exists.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct NoMessages;

impl fmt::Display for NoMessages {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("no messages in inbox")
    }
}
