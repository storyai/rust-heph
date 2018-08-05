//! TODO: docs

use std::io;
use std::future::FutureObj;
use std::task::{Executor, SpawnObjError, SpawnErrorKind};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use log::{debug, trace, log};
use mio_st::event::{Events, Evented, EventedId, Ready};
use mio_st::poll::{Poller, PollOption};
use mio_st::registration::Registration;

use crate::actor::{Actor, ActorContext, NewActor};
use crate::actor_ref::LocalActorRef;
use crate::error::{AddActorError, AddActorErrorReason, AddInitiatorError, AddInitiatorErrorReason, RuntimeError, ERR_SYSTEM_SHUTDOWN};
use crate::initiator::Initiator;
use crate::mailbox::MailBox;
use crate::process::{ProcessId, ActorProcess, InitiatorProcess, TaskProcess};
use crate::scheduler::{Scheduler, SchedulerRef, Priority};
use crate::util::{Shared, WeakShared};
use crate::waker::new_waker;

mod builder;

pub mod options;

pub use self::builder::ActorSystemBuilder;
pub use self::options::{ActorOptions, InitiatorOptions};

/// The system that runs all actors.
#[derive(Debug)]
pub struct ActorSystem {
    /// Inside of the system, shared (via weak references) with
    /// `ActorSystemRef`s.
    inner: Shared<ActorSystemInner>,
    /// Scheduler that hold the processes, schedules and runs them.
    scheduler: Scheduler,
    /// Whether or not the system has initiators.
    // FIXME: This is currently required mostly for tests and example 1 and 1b.
    // Try to remove it.
    has_initiators: bool,
    /// Receiving side of the channel for `Waker` notifications.
    waker_notifications: Receiver<ProcessId>,
}

impl ActorSystem {
    /// Add a new actor to the system.
    // TODO: remove `'static` lifetime.
    pub fn add_actor<N, I, A>(&mut self, new_actor: N, item: I, options: ActorOptions) -> LocalActorRef<N::Message>
        where N: NewActor<Item = I, Actor = A>,
              A: Actor + 'static,
    {
        let system_ref = self.create_ref();
        self.inner.borrow_mut().add_actor(options, new_actor, item, system_ref)
    }

    /// Add a new initiator to the system.
    // TODO: remove `'static` lifetime.
    pub fn add_initiator<I>(&mut self, initiator: I, options: InitiatorOptions) -> Result<(), AddInitiatorError<I>>
        where I: Initiator + 'static,
    {
        match self.inner.borrow_mut().add_initiator(initiator, options) {
            Ok(()) => {
                self.has_initiators = true;
                Ok(())
            },
            err => err,
        }
    }

    /// Create a new reference to this actor system.
    pub fn create_ref(&self) -> ActorSystemRef {
        ActorSystemRef {
            inner: self.inner.downgrade(),
        }
    }

    /// Run the actor system.
    pub fn run(mut self) -> Result<(), RuntimeError> {
        debug!("running actor system");

        // Empty set of events, to be filled by the system poller.
        let mut events = Events::new();
        // System reference used in running the processes.
        let mut system_ref = self.create_ref();

        // TODO: find a good balance between polling, polling user space events
        // only and running processes, the current one is not good. It leans far
        // too much to polling.
        loop {
            self.schedule_processes(&mut events)?;

            if !self.scheduler.run_process(&mut system_ref) && events.is_empty() {
                debug!("no events, no processes to run, stopping actor system");
                return Ok(())
            }
        }
    }

    /// Schedule processes.
    ///
    /// This polls the system poller and the waker notifications and schedules
    /// the processes notified.
    fn schedule_processes(&mut self, events: &mut Events) -> Result<(), RuntimeError> {
        let timeout = if !self.has_initiators || self.scheduler.process_ready() {
            Some(Duration::from_millis(0))
        } else {
            None
        };

        trace!("polling system poller for events");
        self.inner.borrow_mut().poller.poll(events, timeout)
            .map_err(RuntimeError::Poll)?;

        // Schedule all processes with a notification.
        for event in events {
            self.scheduler.schedule(event.id().into());
        }

        trace!("receiving waker events");
        while let Some(pid) = self.waker_notifications.try_recv() {
            self.scheduler.schedule(pid);
        }

        Ok(())
    }
}

/// A reference to an [`ActorSystem`].
///
/// This reference can be shared by cloning it, a very cheap operation.
///
/// [`ActorSystem`]: struct.ActorSystem.html
#[derive(Debug)]
pub struct ActorSystemRef {
    /// A non-owning reference to the actor system internals.
    inner: WeakShared<ActorSystemInner>,
}

impl ActorSystemRef {
    /// Create a new `ActorSystemRef` that can be used in unit testing.
    ///
    /// # Notes
    ///
    /// All methods will always return a system shutdown error.
    #[cfg(feature = "test")]
    pub fn test_ref() -> ActorSystemRef  {
        ActorSystemRef {
            inner: WeakShared::new(),
        }
    }

    /// Add a new actor to the system.
    ///
    /// See [`ActorSystem.add_actor`].
    ///
    /// [`ActorSystem.add_actor`]: struct.ActorSystem.html#method.add_actor
    // TODO: keep this in sync with `ActorSystemRef.add_actor`.
    // TODO: remove `'static` lifetime,
    pub fn add_actor<N, I, A>(&mut self, new_actor: N, item: I, options: ActorOptions) -> Result<LocalActorRef<N::Message>, AddActorError<N>>
        where N: NewActor<Item = I, Actor = A>,
              A: Actor + 'static,
    {
        let system_ref = self.clone();
        match self.inner.upgrade() {
            Some(mut inner) => Ok(inner.borrow_mut().add_actor(options, new_actor, item, system_ref)),
            None => Err(AddActorError::new(new_actor, AddActorErrorReason::SystemShutdown)),
        }
    }

    /// Add an actor that needs to be initialised.
    ///
    /// This is used by the `Initiator`s to register with the system poller with
    /// using same pid.
    pub(crate) fn add_actor_setup<F, A, M>(&mut self, options: ActorOptions, f: F) -> io::Result<()>
        where F: FnOnce(ActorContext<M>, ProcessId, &mut Poller) -> io::Result<A>,
              A: Actor + 'static,
    {
        let system_ref = self.clone();
        match self.inner.upgrade() {
            Some(mut inner) => inner.borrow_mut().add_actor_setup(options, f, system_ref)
                .map(|_| ()),
            None => Err(AddActorError::new((), AddActorErrorReason::SystemShutdown).into()),
        }
    }

    /// Register an `Evented` handle, see `Poll.register`.
    pub(crate) fn poller_register<E>(&mut self, handle: &mut E, id: EventedId, interests: Ready, opt: PollOption) -> io::Result<()>
    where
        E: Evented + ?Sized,
    {
        match self.inner.upgrade() {
            Some(mut inner) => inner.borrow_mut().poller.register(handle, id, interests, opt),
            None => Err(io::Error::new(io::ErrorKind::Other, ERR_SYSTEM_SHUTDOWN)),
        }
    }

    /// Deregister an `Evented` handle, see `Poll.deregister`.
    pub(crate) fn poller_deregister<E>(&mut self, handle: &mut E) -> io::Result<()>
    where
        E: Evented + ?Sized,
    {
        match self.inner.upgrade() {
            Some(mut inner) => inner.borrow_mut().poller.deregister(handle),
            None => Err(io::Error::new(io::ErrorKind::Other, ERR_SYSTEM_SHUTDOWN)),
        }
    }

    /// Get an sending end of the notification channel.
    ///
    /// Returns `None` if the system is shutdown.
    pub(crate) fn get_notification_sender(&mut self) -> Option<Sender<ProcessId>> {
        self.inner.upgrade()
            .map(|mut inner| inner.borrow_mut().waker_notifications.clone())
    }

    pub(crate) fn add_deadline(&mut self, pid: ProcessId, deadline: Instant) -> io::Result<()> {
        self.inner.upgrade()
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, ERR_SYSTEM_SHUTDOWN))
            .and_then(|mut inner| inner.borrow_mut().poller.add_deadline(pid.into(), deadline))
    }
}

impl Executor for ActorSystemRef {
    fn spawn_obj(&mut self, task: FutureObj<'static, ()>) -> Result<(), SpawnObjError> {
        match self.inner.upgrade() {
            Some(mut inner) => {
                inner.borrow_mut().add_task(task);
                Ok(())
            },
            None => Err(SpawnObjError {
                kind: SpawnErrorKind::shutdown(),
                future: task
            }),
        }
    }

    fn status(&self) -> Result<(), SpawnErrorKind> {
        match self.inner.upgrade() {
            Some(_) => Ok(()),
            None => Err(SpawnErrorKind::shutdown()),
        }
    }
}

impl Clone for ActorSystemRef {
    fn clone(&self) -> ActorSystemRef {
        ActorSystemRef {
            inner: self.inner.clone(),
        }
    }
}

/// Inside of the `ActorSystem`, to which `ActorSystemRef`s have a reference to.
#[derive(Debug)]
struct ActorSystemInner {
    /// A reference to the scheduler to add new processes to.
    scheduler_ref: SchedulerRef,
    /// System poller, used for event notifications to support non-blocking I/O.
    poller: Poller,
    /// Sending side of the channel for `Waker` notifications.
    waker_notifications: Sender<ProcessId>,
}

impl ActorSystemInner {
    fn add_actor<N, I, A>(&mut self, options: ActorOptions, mut new_actor: N, item: I, system_ref: ActorSystemRef) -> LocalActorRef<N::Message>
        where N: NewActor<Item = I, Actor = A>,
              A: Actor + 'static,
    {
        self.add_actor_setup(options, move |ctx, _, _| Ok(new_actor.new(ctx, item)), system_ref)
            .unwrap()
    }

    fn add_actor_setup<F, A, M>(&mut self, options: ActorOptions, f: F, system_ref: ActorSystemRef) -> io::Result<LocalActorRef<M>>
        where F: FnOnce(ActorContext<M>, ProcessId, &mut Poller) -> io::Result<A>,
              A: Actor + 'static,
    {
        // Setup adding a new process to the scheduler.
        let process_entry = self.scheduler_ref.add_process();
        let pid = process_entry.id();
        debug!("adding actor to actor system: pid={}", pid);

        // Create a user space registration for the actor. Used in the mailbox
        // and for futures' `Waker`.
        let (mut registration, notifier) = Registration::new();
        self.poller.register(&mut registration, pid.into(), Ready::READABLE, PollOption::Edge)?;

        // Create our waker, mailbox and actor reference.
        let waker = new_waker(pid, self.waker_notifications.clone());
        let mailbox = Shared::new(MailBox::new(pid, notifier));
        let actor_ref = LocalActorRef::new(mailbox.downgrade());

        // Create the actor context and create an actor with it.
        let ctx = ActorContext::new(pid, system_ref, mailbox);
        let actor = f(ctx, pid, &mut self.poller)?;

        // Create an actor process and add finally add it to the scheduler.
        let process = ActorProcess::new(actor, registration, waker);
        process_entry.add(process, options.priority);
        Ok(actor_ref)
    }

    fn add_initiator<I>(&mut self, mut initiator: I, _options: InitiatorOptions) -> Result<(), AddInitiatorError<I>>
        where I: Initiator + 'static,
    {
        // Setup adding a new process to the scheduler.
        let process_entry = self.scheduler_ref.add_process();
        let pid = process_entry.id();
        debug!("adding initiator to actor system: pid={}", pid);

        // Initialise the initiator.
        if let Err(err) = initiator.init(&mut self.poller, pid) {
            return Err(AddInitiatorError {
                initiator,
                reason: AddInitiatorErrorReason::InitFailed(err),
            });
        }

        // Create a new initiator process.
        let process = InitiatorProcess::new(initiator);

        // Actually add the process.
        // Initiators will always have a low priority this way requests in
        // progress are first handled before new requests are accepted and
        // possibly overload the system.
        process_entry.add(process, Priority::LOW);
        Ok(())
    }

    fn add_task(&mut self, task: FutureObj<'static, ()>) {
        // Setup adding a new process to the scheduler.
        let process_entry = self.scheduler_ref.add_process();
        let pid = process_entry.id();
        debug!("adding task to actor system: pid={}", pid);

        // Create a new task process.
        let waker = new_waker(pid, self.waker_notifications.clone());
        let process = TaskProcess::new(task, waker);

        // Actually add the process.
        // TODO: add an option to the `ActorSystemBuilder` to change the
        // priority.
        process_entry.add(process, Priority::NORMAL);
    }
}
