// Copyright 2017 Thomas de Zeeuw
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT
// or http://opensource.org/licenses/MIT>, at your option. This file may not be
// used, copied, modified, or distributed except according to those terms.

//! All actors must implement the [`Actor`] trait, which defines how an actor
//! handles messages. However the system needs a way to create (and recreate)
//! these actors, which is defined in the [`NewActor`] trait. Helper structs are
//! provided to easily implement this trait, see [`ActorFactory`] and
//! [`ReusableActorFactory`].
//!
//! [`Actor`]: trait.Actor.html
//! [`NewActor`]: trait.NewActor.html
//! [`ActorFactory`]: struct.ActorFactory.html
//! [`ReusableActorFactory`]: struct.ReusableActorFactory.html

use std::mem;

use futures::Future;

/// The main actor trait, which defines how an actor handles messages.
pub trait Actor {
    /// The user defined message that this actor can handle.
    ///
    /// Use an enum to allow an actor to handle multiple types of messages.
    type Message;

    /// An error the actor can return to it's supervisor. This error will be
    /// considered terminal for this actor and should **not** not be an error of
    /// regular processing of a message.
    ///
    /// How to process non-terminal errors that happen during regular processing
    /// of messages is up to the user.
    type Error;

    /// The future returned by the actor to handle a message.
    ///
    /// The returned item is discarded, while the returned error is passed to
    /// the actor's supervisor.
    type Future: Future<Item = (), Error = Self::Error>;

    /// Handle a message, the core of this trait.
    ///
    /// # Note
    ///
    /// The returned future will be completed before another message is handled
    /// by this actor, effectively blocking this actor until the future is
    /// completed. If the returned future does any blocking operations, e.g.
    /// I/O, it's recommended to make an actor specific to that blocking
    /// operation, e.g. a unqiue actor per request to handle the reading and
    /// writing of the request/response to a socket.
    fn handle(&mut self, message: Self::Message) -> Self::Future;

    /// The method that will be called once the actor is created, but not yet
    /// has received it's first message.
    ///
    /// The default is to do nothing.
    fn pre_start(&mut self) { }

    /// The method that will be called after the actor received it's final
    /// message, just before it's dropped.
    ///
    /// The default is to do nothing.
    fn post_stop(&mut self) { }

    /// The method that will be called once an actor will be restarted, but just
    /// before actually stopping the actor.
    ///
    /// The default is to call the [`post_stop`] function.
    ///
    /// [`post_stop`]: trait.Actor.html#method.post_stop
    fn pre_restart(&mut self) {
        self.post_stop();
    }

    /// The method that will be called once an actor is restarted, but just
    /// before it will accept it's first message.
    ///
    /// The default is to call the [`pre_start`] function.
    ///
    /// [`pre_start`]: trait.Actor.html#method.pre_start
    fn post_restart(&mut self) {
        self.pre_start();
    }
}

/// The trait that defines how to create a new actor.
pub trait NewActor {
    /// The type of the message the actor can handle, see
    /// [`Actor.Message`].
    ///
    /// [`Actor.Message`]: trait.Actor.html#associatedtype.Message
    type Message;

    /// The type of error the actor can return to it's supervisor, see
    /// [`Actor.Error`].
    ///
    /// [`Actor.Error`]: trait.Actor.html#associatedtype.Error
    type Error;

    /// The type of the future the actor will return when handling a message,
    /// see [`Actor.Future`].
    ///
    /// [`Actor.Future`]: trait.Actor.html#associatedtype.Future
    type Future: Future<Item = (), Error = Self::Error>;

    /// The type of the actor, see [`Actor`].
    ///
    /// [`Actor`]: trait.Actor.html
    type Actor: Actor<Message = Self::Message, Error = Self::Error, Future = Self::Future>;

    /// The method that gets called to create a new actor.
    fn new(&self) -> Self::Actor;

    /// Reuse an already allocated actor. The default implementation will create
    /// a new actor (by calling [`new`]) and replace `old_actor` with it.
    ///
    /// This is a performance optimization to allow the allocations of an actor
    /// to be reused.
    ///
    /// [`new`]: trait.NewActor.html#tymethod.new
    fn reuse(&self, old_actor: &mut Self::Actor) {
        mem::replace(old_actor, self.new());
    }
}

/// A contruct that allows [`NewActor`] to be implemented by means of a
/// function. If a custom [reuse] function is needed see [`ReusableActorFactory`].
///
/// # Example
///
/// ```
///
/// # extern crate actor;
/// # extern crate futures;
/// # use actor::actor::{Actor, NewActor};
/// # use futures::Future;
/// use actor::actor::ActorFactory;
///
/// // Our actor that implements the `Actor` trait.
/// struct MyActor;
///
/// # impl Actor for MyActor {
/// #    type Message = ();
/// #    type Error = ();
/// #    type Future = Box<Future<Item = (), Error = ()>>;
/// #    fn handle(&mut self, _: ()) -> Self::Future { unimplemented!(); }
/// # }
/// #
/// # fn main() {
/// // Our `NewActor` implementation that returns our actor.
/// let new_actor = ActorFactory(|| MyActor);
/// #
/// # fn use_new_actor<A: NewActor>(new_actor: A) { }
/// # use_new_actor(new_actor);
/// # }
/// ```
///
/// [`NewActor`]: trait.NewActor.html
/// [reuse]: trait.NewActor.html#method.reuse
/// [`ReusableActorFactory`]: struct.ReusableActorFactory.html
pub struct ActorFactory<N, A>(pub N)
    where N: Fn() -> A,
          A: Actor;

impl<N, A> NewActor for ActorFactory<N, A>
    where N: Fn() -> A,
          A: Actor,
{
    type Message = A::Message;
    type Error = A::Error;
    type Future = A::Future;
    type Actor = A;
    fn new(&self) -> Self::Actor {
        (self.0)()
    }
}

/// A contruct that allows [`NewActor`] to be implemented by means of a
/// function, including the reuse of an actor.
///
/// # Example
///
/// ```
/// # extern crate actor;
/// # extern crate futures;
/// # use actor::actor::{Actor, NewActor};
/// # use futures::Future;
/// use actor::actor::ReusableActorFactory;
///
/// // Our actor that implements the `Actor` trait.
/// struct MyActor;
///
/// # impl Actor for MyActor {
/// #    type Message = ();
/// #    type Error = ();
/// #    type Future = Box<Future<Item = (), Error = ()>>;
/// #    fn handle(&mut self, _: ()) -> Self::Future { unimplemented!(); }
/// # }
/// #
/// impl MyActor {
///     fn reset(&mut self) { /* Reset our actor. */ }
/// }
///
/// # fn main() {
/// // Our `NewActor` implementation that returns our actor.
/// let new_actor = ReusableActorFactory(|| MyActor, |actor| actor.reset());
/// #
/// # fn use_new_actor<A: NewActor>(new_actor: A) { }
/// # use_new_actor(new_actor);
/// # }
/// ```
///
/// [`NewActor`]: trait.NewActor.html
pub struct ReusableActorFactory<N, R, A>(pub N, pub R)
    where N: Fn() -> A,
          R: Fn(&mut A),
          A: Actor;

impl<N, R, A> NewActor for ReusableActorFactory<N, R, A>
    where N: Fn() -> A,
          R: Fn(&mut A),
          A: Actor,
{
    type Message = A::Message;
    type Error = A::Error;
    type Future = A::Future;
    type Actor = A;
    fn new(&self) -> Self::Actor {
        (self.0)()
    }
    fn reuse(&self, old_actor: &mut Self::Actor) {
        (self.1)(old_actor)
    }
}
