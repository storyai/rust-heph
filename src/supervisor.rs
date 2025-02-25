//! The module with the supervisor and related types.
//!
//! # Supervisor
//!
//! A supervisor supervises an actor and handles any errors it encounters. A
//! supervisor generally does two things; logging of the error and deciding
//! whether to stop or restart the actor. Its advised to keep a supervisor small
//! and simple.
//!
//! When encountering an error it usually means someone has to be notified (to
//! fix it), something often done via logging.
//!
//! Next the supervisor needs to decide if the actor needs to be [stopped] or
//! [restarted]. If the supervisor decides to restart the actor it needs to
//! provide the argument to create a new actor (used in calling the
//! [`NewActor::new`] method).
//!
//! The restarted actor will have the same message inbox as the old (stopped)
//! actor. Note however that if an actor retrieved a message from its inbox, and
//! returned an error when processing it, the new (restarted) actor won't
//! retrieve that message again (messages aren't cloned after all).
//!
//! # Restarting or stopping?
//!
//! Sometimes just restarting an actor is the easiest way to deal with errors.
//! Starting the actor from a clean slate will often allow it to continue
//! processing. However this is not possible in all cases, for example when a
//! new argument can't be provided (think actors started by a [`TcpServer`]). In
//! those cases the supervisor should still log the error encountered.
//!
//! [stopped]: crate::supervisor::SupervisorStrategy::Stop
//! [restarted]: crate::supervisor::SupervisorStrategy::Restart
//! [`TcpServer`]: crate::net::TcpServer
//!
//! # Actors and sync actors
//!
//! As actors come in two flavours, [regular/asynchronous actors] and
//! [synchronous actors], thus so do the supervisor traits, [`Supervisor`] and
//! [`SyncSupervisor`].
//!
//! [regular/asynchronous actors]: crate::actor::Actor
//! [synchronous actors]: crate::actor::SyncActor
//! [`Supervisor`]: crate::supervisor::Supervisor
//! [`SyncSupervisor`]: crate::supervisor::SyncSupervisor
//!
//!
//! # Provided implementations
//!
//! There are two [`Supervisor`] implementations provided by Heph. First, the
//! [`NoSupervisor`] can be used when the actor never returns an error (i.e.
//! `Result<(), !>` or no return type) and thus doesn't need supervision.
//!
//! Second, the [`restart_supervisor!`] macro, which can be used to easily
//! create a supervisor implementation that restarts the actor.
//!
//! # Examples
//!
//! Supervisor that logs the errors of a badly behaving actor and stops it.
//!
//! ```
//! #![feature(never_type)]
//!
//! use heph::actor;
//! use heph::rt::{self, ThreadLocal, Runtime};
//! use heph::spawn::ActorOptions;
//! use heph::supervisor::SupervisorStrategy;
//! use log::error;
//!
//! fn main() -> Result<(), rt::Error> {
//!     // Enable logging so we can see the error message.
//!     std_logger::init();
//!
//!     let mut runtime = Runtime::new()?;
//!     runtime.run_on_workers(|mut runtime_ref| -> Result<(), !> {
//!         runtime_ref.spawn_local(supervisor, bad_actor as fn(_) -> _, (), ActorOptions::default());
//!         Ok(())
//!     })?;
//!     runtime.start()
//! }
//!
//! /// The error returned by our actor.
//! struct Error;
//!
//! /// Supervisor that gets called if the actor returns an error.
//! fn supervisor(err: Error) -> SupervisorStrategy<()> {
//! #   drop(err); // Silence dead code warnings.
//!     error!("Actor encountered an error!");
//!     SupervisorStrategy::Stop
//! }
//!
//! /// Our badly behaving actor.
//! async fn bad_actor(_: actor::Context<!, ThreadLocal>) -> Result<(), Error> {
//!     Err(Error)
//! }
//! ```

use crate::actor::SyncActor;
use crate::actor::{Actor, NewActor};

/// The supervisor of an [actor].
///
/// For more information about supervisors see the [module documentation], here
/// only the design of the trait is discussed.
///
/// The trait is designed to be generic over the [`NewActor`] implementation
/// (`NA`). This means that the same type can implement supervision for a number
/// of different actors. But a word of caution, supervisors should generally be
/// small and simple, which means that having a different supervisor for each
/// actor is often a good thing.
///
/// The [`restart_supervisor!`] macro can be used to easily create a supervisor
/// implementation that restarts the actor.
///
/// `Supervisor` can be implemented using a simple function if the `NewActor`
/// implementation doesn't return an error (i.e. `NewActor::Error = !`), which
/// is the case for asynchronous functions. See the [module documentation] for
/// an example of this.
///
/// [actor]: crate::actor
/// [module documentation]: crate::supervisor
pub trait Supervisor<NA>
where
    NA: NewActor,
{
    /// Decide what happens to the actor that returned `error`.
    ///
    /// # Notes
    ///
    /// A restarted actor will immediately will be run again. This has the
    /// benefit that the actor can setup any asynchronous operations without
    /// having to wait to be run again.
    ///
    /// However it also has a downside: *it's easy to create create an infinite
    /// loop*. When a supervisor always restarts an actor that always returns an
    /// error, we've got an effective infinite loop of restarting and running
    /// the actor, the actor crashes and is restarted and run again, etc.
    ///
    /// To avoid creating such an infinite loop limit the amount times an actor
    /// can be restarted. Or use the [`restart_supervisor!`] macro to
    /// automatically create a supervisor that handles this for you.
    fn decide(&mut self, error: <NA::Actor as Actor>::Error) -> SupervisorStrategy<NA::Argument>;

    /// Decide what happens when an actor is restarted and the [`NewActor`]
    /// implementation returns an `error`.
    ///
    /// Read the documentation of [`decide`] to avoid an *infinite loop*.
    ///
    /// [`decide`]: Supervisor::decide
    fn decide_on_restart_error(&mut self, error: NA::Error) -> SupervisorStrategy<NA::Argument>;

    /// Method that gets call if an actor fails to restart twice.
    ///
    /// This is only called if [`decide`] returns a restart strategy, the actors
    /// fails to restart, after which [`decide_on_restart_error`] is called and
    /// also returns a restart strategy and restarting the actor a second time
    /// also fails. We will not create an endless loop of restarting failures
    /// and instead call this function before stopping the actor (which can't be
    /// restarted any more).
    ///
    /// [`decide`]: Supervisor::decide
    /// [`decide_on_restart_error`]: Supervisor::decide_on_restart_error
    // TODO: a better name.
    fn second_restart_error(&mut self, error: NA::Error);
}

impl<F, NA> Supervisor<NA> for F
where
    F: FnMut(<NA::Actor as Actor>::Error) -> SupervisorStrategy<NA::Argument>,
    NA: NewActor<Error = !>,
{
    fn decide(&mut self, err: <NA::Actor as Actor>::Error) -> SupervisorStrategy<NA::Argument> {
        (self)(err)
    }

    fn decide_on_restart_error(&mut self, _: !) -> SupervisorStrategy<NA::Argument> {
        // This can't be called.
        SupervisorStrategy::Stop
    }

    fn second_restart_error(&mut self, _: !) {
        // This can't be called.
    }
}

/// The strategy to use when handling an error from an actor.
///
/// See the [module documentation] for deciding on whether to restart an or not.
///
/// [module documentation]: index.html#restarting-or-stopping
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SupervisorStrategy<Arg> {
    /// Restart the actor with the provided argument `Arg`.
    Restart(Arg),
    /// Stop the actor.
    Stop,
}

/// Supervisor for [synchronous actors].
///
/// For more information about supervisors see the [module documentation], here
/// only the design of the trait is discussed.
///
/// The trait is designed to be generic over the actor (`A`). This means that
/// the same type can implement supervision for a number of different actors.
/// But a word of caution, supervisors should generally be small and simple,
/// which means that having a different supervisor for each actor is often a
/// good thing.
///
/// `SyncSupervisor` is implemented for any function that takes an error `E` and
/// returns `SupervisorStrategy<Arg>` automatically.
///
/// [synchronous actors]: crate::actor::SyncActor
/// [module documentation]: crate::supervisor
pub trait SyncSupervisor<A>
where
    A: SyncActor,
{
    /// Decide what happens to the actor that returned `error`.
    fn decide(&mut self, error: A::Error) -> SupervisorStrategy<A::Argument>;
}

impl<F, A> SyncSupervisor<A> for F
where
    F: FnMut(A::Error) -> SupervisorStrategy<A::Argument>,
    A: SyncActor,
{
    fn decide(&mut self, err: A::Error) -> SupervisorStrategy<A::Argument> {
        (self)(err)
    }
}

/// A supervisor implementation for actors that never return an error.
///
/// This supervisor does nothing and can't actually be called, it can only serve
/// as supervisor for actors that never return an error, i.e. actor that use the
/// never type (`!`) as error type.
///
/// # Example
///
/// ```
/// #![feature(never_type)]
///
/// use heph::actor;
/// use heph::rt::{self, ThreadLocal, Runtime};
/// use heph::spawn::ActorOptions;
/// use heph::supervisor::NoSupervisor;
///
/// fn main() -> Result<(), rt::Error> {
///     let mut runtime = Runtime::new()?;
///     runtime.run_on_workers(|mut runtime_ref| -> Result<(), !> {
///         runtime_ref.spawn_local(NoSupervisor, actor as fn(_) -> _, (), ActorOptions::default());
///         Ok(())
///     })?;
///     runtime.start()
/// }
///
/// /// Our actor that never returns an error.
/// async fn actor(ctx: actor::Context<&'static str, ThreadLocal>) {
///     println!("Hello world!");
/// #   drop(ctx); // Silence dead code warnings.
/// }
/// ```
#[derive(Copy, Clone, Debug)]
pub struct NoSupervisor;

impl<NA> Supervisor<NA> for NoSupervisor
where
    NA: NewActor<Error = !>,
    NA::Actor: Actor<Error = !>,
{
    fn decide(&mut self, _: !) -> SupervisorStrategy<NA::Argument> {
        // This can't be called.
        SupervisorStrategy::Stop
    }

    fn decide_on_restart_error(&mut self, _: !) -> SupervisorStrategy<NA::Argument> {
        // This can't be called.
        SupervisorStrategy::Stop
    }

    fn second_restart_error(&mut self, _: !) {
        // This can't be called.
    }
}

impl<A> SyncSupervisor<A> for NoSupervisor
where
    A: SyncActor<Error = !>,
{
    fn decide(&mut self, _: !) -> SupervisorStrategy<A::Argument> {
        // This can't be called.
        SupervisorStrategy::Stop
    }
}

/// Macro to create a supervisor that logs the error and restarts the actor.
///
/// This creates a new type that implements the [`Supervisor`] and
/// [`SyncSupervisor`] traits. The macro accepts the following arguments:
///
/// * Visibility indicator (*optional*), defaults to private (i.e. no
///   indicator).
/// * Name of the new supervisor type.
/// * Log friendly name of the actor, used in logging.
/// * Type of the argument(s) used to restart the actor. Multiple arguments must
///   be in the tuple format (same as for the [`NewActor::Argument`] type).
/// * Maximum number of restarts (*optional*), defaults to 5.
/// * Maximum duration before the restart counter get reset (*optional*),
///   defaults to 5 seconds.
/// * Additional logging message, defaults to nothing extra. This uses normal
///   [rust formatting rules] and is added at the end of the default message,
///   after the error. The `args` keyword gives access to the arguments. See
///   example 7 Restart Supervisor (in the example directory of the source
///   code).
///
/// The new type can be created using the `new` function, e.g.
/// `MySupervisor::new(args)`, see the example below.
///
/// [rust formatting rules]: std::fmt
///
/// # Logged messages
///
/// When the actor failed and is restarted:
///
/// ```text
/// $actor_name failed, restarting it ($left/$max restarts left): ${error}$log_extra
/// # For example, using the supervisor created in the example below.
/// my_actor failed, restarting it (1/2 restarts left): some I/O error: actor arguments (true, 0): (true, 0)
/// ```
///
/// If the actor failed too many times to quickly it will log the following.
///
/// ```text
/// $actor_name failed, stopping it (no restarts left): ${error}$log_extra
/// # For example, using the supervisor created in the example below.
/// my_actor failed, stopping it (no restarts left): some I/O error: actor arguments (true, 0): (true, 0)
/// ```
///
/// Similar messages will be logged if the actor fails to restart.
///
/// # Examples
///
/// The example below shows the simplest usage of the `restart_supervisor`
/// macro. Example 7 Restart Supervisor (in the example directory of the source
/// code) has a more complete example.
///
/// ```
/// use std::time::Duration;
///
/// use heph::restart_supervisor;
///
/// // Creates the `MySupervisor` type.
/// restart_supervisor!(
///     pub                      // Visibility indicator.
///     MySupervisor,            // Name of the supervisor type.
///     "my actor",              // Name of the actor.
///     (bool, u32),             // Type of the arguments for the actor.
///     2,                       // Maximum number of restarts.
///     Duration::from_secs(30), // Maximum duration before the restart counter
///                              // get reset, defaults to 5 seconds (optional).
///     // This string is added to the log message after the error. `args`
///     // gives access to the arguments.
///     ": actor arguments {:?} ({}, {})",
///     args,
///     args.0,
///     args.1,
/// );
///
/// // Create a new supervisor.
/// let supervisor = MySupervisor::new(true, 23);
/// # drop(supervisor);
/// ```
#[macro_export]
macro_rules! restart_supervisor {
    // No non-optional arguments, unit `NewActor::Argument`.
    ($vis: vis $supervisor_name: ident, $actor_name: expr $(,)*) => {
        $crate::__heph_restart_supervisor_impl!($vis $supervisor_name, $actor_name, (), 5, std::time::Duration::from_secs(5), "",);
    };
    ($vis: vis $supervisor_name: ident, $actor_name: expr, () $(,)*) => {
        $crate::__heph_restart_supervisor_impl!($vis $supervisor_name, $actor_name, (), 5, std::time::Duration::from_secs(5), "",);
    };
    // No non-optional arguments, tuple `NewActor::Argument`.
    ($vis: vis $supervisor_name: ident, $actor_name: expr, ( $( $arg: ty),* ) $(,)*) => {
        $crate::__heph_restart_supervisor_impl!($vis $supervisor_name, $actor_name, ( $( $arg ),* ), 5, std::time::Duration::from_secs(5), "",);
    };
    // No non-optional arguments, single `NewActor::Argument`.
    ($vis: vis $supervisor_name: ident, $actor_name: expr, $arg: ty $(,)*) => {
        $crate::__heph_restart_supervisor_impl!($vis $supervisor_name, $actor_name, ( $arg ), 5, std::time::Duration::from_secs(5), "",);
    };

    // No log extra, unit `NewActor::Argument`.
    ($vis: vis $supervisor_name: ident, $actor_name: expr, (), $max_restarts: expr, $max_duration: expr $(,)*) => {
        $crate::__heph_restart_supervisor_impl!($vis $supervisor_name, $actor_name, (), $max_restarts, $max_duration, "",);
    };
    // No log extra, tuple `NewActor::Argument`.
    ($vis: vis $supervisor_name: ident, $actor_name: expr, ( $( $arg: ty ),* ), $max_restarts: expr, $max_duration: expr $(,)*) => {
        $crate::__heph_restart_supervisor_impl!($vis $supervisor_name, $actor_name, ( $( $arg ),* ), $max_restarts, $max_duration, "",);
    };
    // No log extra, single `NewActor::Argument`.
    ($vis: vis $supervisor_name: ident, $actor_name: expr, $arg: ty, $max_restarts: expr, $max_duration: expr $(,)*) => {
        $crate::__heph_restart_supervisor_impl!($vis $supervisor_name, $actor_name, ( $arg ), $max_restarts, $max_duration, "",);
    };

    // All arguments, unit `NewActor::Argument`.
    ($vis: vis $supervisor_name: ident, $actor_name: expr, (), $max_restarts: expr, $max_duration: expr, $log_extra: expr, $( args $(. $log_arg_field: tt )* ),* $(,)*) => {
        $crate::__heph_restart_supervisor_impl!($vis $supervisor_name, $actor_name, (), $max_restarts, $max_duration, $log_extra, $( args $(. $log_arg_field )* ),*);
    };
    // All arguments, tuple `NewActor::Argument`.
    ($vis: vis $supervisor_name: ident, $actor_name: expr, ( $( $arg: ty ),* ), $max_restarts: expr, $max_duration: expr, $log_extra: expr, $( args $(. $log_arg_field: tt )* ),* $(,)*) => {
        $crate::__heph_restart_supervisor_impl!($vis $supervisor_name, $actor_name, ( $( $arg ),* ), $max_restarts, $max_duration, $log_extra, $( args $(. $log_arg_field )* ),*);
    };
    // All arguments, single `NewActor::Argument`.
    ($vis: vis $supervisor_name: ident, $actor_name: expr, $arg: ty, $max_restarts: expr, $max_duration: expr, $log_extra: expr, $( args $(. $log_arg_field: tt )* ),* $(,)*) => {
        $crate::__heph_restart_supervisor_impl!($vis $supervisor_name, $actor_name, ( $arg ), $max_restarts, $max_duration, $log_extra, $( args $(. $log_arg_field )* ),*);
    };
}

/// Private macro to implement the [`restart_supervisor`].
#[doc(hidden)]
#[macro_export]
macro_rules! __heph_restart_supervisor_impl {
    (
        $vis: vis
        $supervisor_name: ident,
        $actor_name: expr,
        ( $( $arg: ty ),* ),
        $max_restarts: expr,
        $max_duration: expr,
        $log_extra: expr,
        $( args $(. $log_arg_field: tt )* ),*
        $(,)*
    ) => {
        $crate::__heph_doc!(
            std::concat!(
                "Supervisor for ", $actor_name, ".\n\n",
                "Maximum number of restarts: `", stringify!($max_restarts), "`, ",
                "within a duration of: `", stringify!($max_duration), "`.",
            ),
            #[derive(Debug)]
            #[allow(unused_qualifications)]
            $vis struct $supervisor_name {
                /// The number of restarts left.
                restarts_left: std::primitive::usize,
                /// Time of the last restart.
                last_restart: std::option::Option<std::time::Instant>,
                /// Arguments used to restart the actor.
                args: ( $( $arg ),* ),
            }
        );

        #[allow(unused_qualifications)]
        impl $supervisor_name {
            /// Maximum number of restarts within a [`Self::MAX_DURATION`] time
            /// period before the actor is stopped.
            $vis const MAX_RESTARTS: std::primitive::usize = $max_restarts;

            /// Maximum duration between errors to be considered of the same
            /// cause. If `MAX_DURATION` has elapsed between errors the restart
            /// counter gets reset to [`MAX_RESTARTS`].
            ///
            /// [`MAX_RESTARTS`]: Self::MAX_RESTARTS
            $vis const MAX_DURATION: std::time::Duration = $max_duration;

            $crate::__heph_restart_supervisor_impl!(impl_new $vis $supervisor_name, ( $( $arg ),* ));
        }

        impl<NA> $crate::supervisor::Supervisor<NA> for $supervisor_name
        where
            NA: $crate::NewActor<Argument = ( $( $arg ),* )>,
            NA::Error: std::fmt::Display,
            <NA::Actor as $crate::Actor>::Error: std::fmt::Display,
        {
            fn decide(&mut self, err: <NA::Actor as $crate::Actor>::Error) -> $crate::SupervisorStrategy<NA::Argument> {
                $crate::__heph_restart_supervisor_impl!{decide_impl self, err, $actor_name, $max_restarts, $log_extra, $( args $(. $log_arg_field )* ),*}
            }

            fn decide_on_restart_error(&mut self, err: NA::Error) -> $crate::SupervisorStrategy<NA::Argument> {
                self.last_restart = Some(std::time::Instant::now());

                if self.restarts_left >= 1 {
                    self.restarts_left -= 1;
                    $crate::log::_private::warn!(
                        std::concat!($actor_name, " actor failed to restart, trying again ({}/{} restarts left): {}", $log_extra),
                        self.restarts_left, $max_restarts, err, $( self.args $(. $log_arg_field )* ),*
                    );
                    $crate::SupervisorStrategy::Restart(self.args.clone())
                } else {
                    $crate::log::_private::warn!(
                        std::concat!($actor_name, " actor failed to restart, stopping it (no restarts left): {}", $log_extra),
                        err, $( self.args $(. $log_arg_field )* ),*
                    );
                    $crate::SupervisorStrategy::Stop
                }
            }

            fn second_restart_error(&mut self, err: NA::Error) {
                $crate::log::_private::warn!(
                    std::concat!($actor_name, " actor failed to restart a second time, stopping it: {}", $log_extra),
                    err, $( self.args $(. $log_arg_field )* ),*
                );
            }
        }

        impl<A> $crate::supervisor::SyncSupervisor<A> for $supervisor_name
        where
            A: $crate::actor::SyncActor<Argument = ( $( $arg ),* )>,
            A::Error: std::fmt::Display,
        {
            fn decide(&mut self, err: A::Error) -> $crate::SupervisorStrategy<A::Argument> {
                $crate::__heph_restart_supervisor_impl!{decide_impl self, err, $actor_name, $max_restarts, $log_extra, $( args $(. $log_arg_field )* ),*}
            }
        }
    };

    // The `decide` implementation of `Supervisor` and `SyncSupervisor`.
    (
        decide_impl
        $self: ident,
        $err: ident,
        $actor_name: expr,
        $max_restarts: expr,
        $log_extra: expr,
        $( args $(. $log_arg_field: tt )* ),*
        $(,)*
    ) => {
        let now = std::time::Instant::now();
        let last_restart = $self.last_restart.replace(now);

        // If enough time has passed between the last restart and now we
        // reset the `restarts_left` left counter.
        if let Some(last_restart) = last_restart {
            let duration_since_last_crash = now - last_restart;
            if duration_since_last_crash > Self::MAX_DURATION {
                $self.restarts_left = Self::MAX_RESTARTS;
            }
        }

        if $self.restarts_left >= 1 {
            $self.restarts_left -= 1;
            $crate::log::_private::warn!(
                std::concat!($actor_name, " failed, restarting it ({}/{} restarts left): {}", $log_extra),
                $self.restarts_left, $max_restarts, $err, $( $self.args $(. $log_arg_field )* ),*
            );
            $crate::SupervisorStrategy::Restart($self.args.clone())
        } else {
            $crate::log::_private::warn!(
                std::concat!($actor_name, " failed, stopping it (no restarts left): {}", $log_extra),
                $err, $( $self.args $(. $log_arg_field )* ),*
            );
            $crate::SupervisorStrategy::Stop
        }
    };

    // TODO: DRY the implementations below.

    // Unit (`()`) type as argument.
    (impl_new $vis: vis $supervisor_name: ident, ()) => {
        $crate::__heph_doc!(
            std::concat!("Create a new `", stringify!($supervisor_name), "`."),
            #[allow(dead_code)]
            $vis const fn new() -> $supervisor_name {
                $supervisor_name {
                    restarts_left: Self::MAX_RESTARTS,
                    last_restart: None,
                    args: (),
                }
            }
        );
    };
    // Single argument.
    (impl_new $vis: vis $supervisor_name: ident, ( $arg: ty )) => {
        $crate::__heph_doc!(
            std::concat!("Create a new `", stringify!($supervisor_name), "`."),
            #[allow(dead_code)]
            $vis const fn new(arg: $arg) -> $supervisor_name {
                $supervisor_name {
                    restarts_left: Self::MAX_RESTARTS,
                    last_restart: None,
                    args: (arg),
                }
            }
        );
    };
    // Tuple type as argument.
    // Tuple of 2.
    (impl_new $vis: vis $supervisor_name: ident, ($arg0: ty, $arg1: ty)) => {
        $crate::__heph_doc!(
            std::concat!("Create a new `", stringify!($supervisor_name), "`."),
            #[allow(dead_code)]
            $vis const fn new(arg0: $arg0, arg1: $arg1) -> $supervisor_name {
                $supervisor_name {
                    restarts_left: Self::MAX_RESTARTS,
                    last_restart: None,
                    args: (arg0, arg1),
                }
            }
        );
    };
    // Tuple of 3.
    (impl_new $vis: vis $supervisor_name: ident, ($arg0: ty, $arg1: ty, $arg2: ty)) => {
        $crate::__heph_doc!(
            std::concat!("Create a new `", stringify!($supervisor_name), "`."),
            #[allow(dead_code)]
            $vis const fn new(arg0: $arg0, arg1: $arg1, arg2: $arg2) -> $supervisor_name {
                $supervisor_name {
                    restarts_left: Self::MAX_RESTARTS,
                    last_restart: None,
                    args: (arg0, arg1, arg2),
                }
            }
        );
    };
    // Tuple of 4.
    (impl_new $vis: vis $supervisor_name: ident, ($arg0: ty, $arg1: ty, $arg2: ty, $arg3: ty)) => {
        $crate::__heph_doc!(
            std::concat!("Create a new `", stringify!($supervisor_name), "`."),
            #[allow(dead_code)]
            $vis const fn new(arg0: $arg0, arg1: $arg1, arg2: $arg2, arg3: $arg3) -> $supervisor_name {
                $supervisor_name {
                    restarts_left: Self::MAX_RESTARTS,
                    last_restart: None,
                    args: (arg0, arg1, arg2, arg3),
                }
            }
        );
    };
    // Tuple of 5.
    (impl_new $vis: vis $supervisor_name: ident, ($arg0: ty, $arg1: ty, $arg2: ty, $arg3: ty, $arg4: ty)) => {
        $crate::__heph_doc!(
            std::concat!("Create a new `", stringify!($supervisor_name), "`."),
            #[allow(dead_code)]
            $vis const fn new(arg0: $arg0, arg1: $arg1, arg2: $arg2, arg3: $arg3, arg4: $arg4) -> $supervisor_name {
                $supervisor_name {
                    restarts_left: Self::MAX_RESTARTS,
                    last_restart: None,
                    args: (arg0, arg1, arg2, arg3, arg4),
                }
            }
        );
    };
    // Tuple of 6.
    (impl_new $vis: vis $supervisor_name: ident, ($arg0: ty, $arg1: ty, $arg2: ty, $arg3: ty, $arg4: ty, $arg5: ty)) => {
        $crate::__heph_doc!(
            std::concat!("Create a new `", stringify!($supervisor_name), "`."),
            #[allow(dead_code)]
            $vis const fn new(arg0: $arg0, arg1: $arg1, arg2: $arg2, arg3: $arg3, arg4: $arg4, arg5: $arg5) -> $supervisor_name {
                $supervisor_name {
                    restarts_left: Self::MAX_RESTARTS,
                    last_restart: None,
                    args: (arg0, arg1, arg2, arg3, arg4, arg5),
                }
            }
        );
    };
    // Tuple of 6+.
    (impl_new $vis: vis $supervisor_name: ident, ( $( $arg: ty ),* )) => {
        $crate::__heph_doc!(
            std::concat!("Create a new `", stringify!($supervisor_name), "`."),
            #[allow(dead_code)]
            $vis const fn new(args: ( $( $arg ),* )) -> $supervisor_name {
                $supervisor_name {
                    restarts_left: Self::MAX_RESTARTS,
                    last_restart: None,
                    args,
                }
            }
        );
    };
}

/// Helper macro to document type created in [`restart_supervisor`].
#[doc(hidden)]
#[macro_export]
macro_rules! __heph_doc {
    ($doc: expr, $( $tt: tt )*) => {
        #[doc = $doc]
        $($tt)*
    };
}
