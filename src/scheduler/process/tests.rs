//! Unit tests for the process module.

// TODO: test deregistration of actor in Actor Registry.

use std::cmp::Ordering;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{self, AtomicBool, AtomicUsize};
use std::thread::sleep;
use std::time::Duration;

use crossbeam_channel as channel;
use mio_st::event::EventedId;
use mio_st::poll::Poller;

use crate::actor::ActorContext;
use crate::initiator::Initiator;
use crate::scheduler::process::{ActorProcess, InitiatorProcess, Priority, Process, ProcessId, ProcessResult};
use crate::supervisor::{NoopSupervisor, SupervisorStrategy};
use crate::system::ActorSystemRef;
use crate::test;
use crate::waker::new_waker;

#[test]
fn pid() {
    assert_eq!(ProcessId(0), ProcessId(0));
    assert_eq!(ProcessId(100), ProcessId(100));

    assert!(ProcessId(0) < ProcessId(100));

    assert_eq!(ProcessId(0).to_string(), "0");
    assert_eq!(ProcessId(100).to_string(), "100");
    assert_eq!(ProcessId(8000).to_string(), "8000");
}

#[test]
fn pid_and_evented_id() {
    let pid = ProcessId(0);
    let id: EventedId = pid.into();
    assert_eq!(id, EventedId(0));

    let id = EventedId(0);
    let pid: ProcessId = id.into();
    assert_eq!(pid, ProcessId(0));
}

#[test]
fn priority() {
    assert!(Priority::HIGH > Priority::NORMAL);
    assert!(Priority::NORMAL > Priority::LOW);
    assert!(Priority::HIGH > Priority::LOW);

    assert_eq!(Priority::HIGH, Priority::HIGH);
    assert_ne!(Priority::HIGH, Priority::NORMAL);

    assert_eq!(Priority::default(), Priority::NORMAL);
}

#[test]
fn priority_duration_multiplication() {
    let high = Duration::from_millis(1) * Priority::HIGH;
    let normal = Duration::from_millis(1) * Priority::NORMAL;
    let low = Duration::from_millis(1) * Priority::LOW;

    assert!(high < normal);
    assert!(normal < low);
    assert!(high < low);
}

#[derive(Debug)]
struct TestProcess(pub ProcessId, pub Priority, pub Duration);

impl Process for TestProcess {
    fn id(&self) -> ProcessId {
        self.0
    }

    fn priority(&self) -> Priority {
        self.1
    }

    fn runtime(&self) -> Duration {
        self.2
    }

    fn run(self: Pin<&mut Self>, _system_ref: &mut ActorSystemRef) -> ProcessResult {
        unimplemented!();
    }
}

#[test]
fn process_equality() {
    let process1 = TestProcess(ProcessId(0), Priority::LOW, Duration::from_millis(0));
    let process1: &dyn Process = &process1;
    let process2 = TestProcess(ProcessId(0), Priority::NORMAL, Duration::from_millis(0));
    let process2: &dyn Process = &process2;
    let process3 = TestProcess(ProcessId(1), Priority::HIGH, Duration::from_millis(0));
    let process3: &dyn Process = &process3;

    // Equality is only based on id alone.
    assert_eq!(process1, process1);
    assert_eq!(process2, process2);
    assert_eq!(process1, process2);

    assert_ne!(process1, process3);
    assert_ne!(process2, process3);
}

#[test]
fn process_ordering() {
    let process1 = TestProcess(ProcessId(0), Priority::NORMAL, Duration::from_millis(10));
    let process1: &dyn Process = &process1;
    let process2 = TestProcess(ProcessId(0), Priority::NORMAL, Duration::from_millis(11));
    let process2: &dyn Process = &process2;
    let process3 = TestProcess(ProcessId(0), Priority::HIGH, Duration::from_millis(10));
    let process3: &dyn Process = &process3;

    // Ordering is based only on runtime and priority.
    assert_eq!(process1.cmp(process1), Ordering::Equal);
    assert_eq!(process1.cmp(process2), Ordering::Greater);
    assert_eq!(process1.cmp(process3), Ordering::Less);

    assert_eq!(process2.cmp(process1), Ordering::Less);
    assert_eq!(process2.cmp(process2), Ordering::Equal);
    assert_eq!(process2.cmp(process3), Ordering::Less);

    assert_eq!(process3.cmp(process1), Ordering::Greater);
    assert_eq!(process3.cmp(process2), Ordering::Greater);
    assert_eq!(process3.cmp(process3), Ordering::Equal);

    let process1 = TestProcess(ProcessId(0), Priority::LOW, Duration::from_millis(0));
    let process1: &dyn Process = &process1;
    let process2 = TestProcess(ProcessId(0), Priority::NORMAL, Duration::from_millis(0));
    let process2: &dyn Process = &process2;
    let process3 = TestProcess(ProcessId(0), Priority::HIGH, Duration::from_millis(0));
    let process3: &dyn Process = &process3;

    // If all the "fair runtimes" are equal we only compare based on the
    // priority.
    assert_eq!(process1.runtime() * process1.priority(), process2.runtime() * process2.priority());
    assert_eq!(process1.runtime() * process1.priority(), process3.runtime() * process3.priority());
    assert_eq!(process1.cmp(process2), Ordering::Less);
    assert_eq!(process1.cmp(process3), Ordering::Less);
    assert_eq!(process2.cmp(process3), Ordering::Less);
}

async fn ok_actor(mut ctx: ActorContext<()>) -> Result<(), !> {
    let _msg = await!(ctx.receive());
    Ok(())
}

#[test]
fn actor_process() {
    // Create our actor.
    #[allow(trivial_casts)]
    let new_actor = ok_actor as fn(_) -> _;
    let (actor, mut actor_ref) = test::init_actor(new_actor, ());

    // Create the waker.
    let pid = ProcessId(0);
    let (sender, _) = channel::unbounded();
    let waker = new_waker(pid, sender);

    // Create our process.
    let inbox = actor_ref.get_inbox().unwrap();
    let process = ActorProcess::new(pid, Priority::NORMAL, NoopSupervisor,
        new_actor, actor, inbox, waker);
    let mut process = Box::pin(process);

    assert_eq!(process.id(), ProcessId(0));
    assert_eq!(process.priority(), Priority::NORMAL);
    assert_eq!(process.runtime(), Duration::from_millis(0));

    // Actor should return `Poll::Pending` in the first call, since no message
    // is available.
    let mut system_ref = test::system_ref();
    assert_eq!(process.as_mut().run(&mut system_ref), ProcessResult::Pending);

    // Runtime must be increased after each call to run.
    let runtime_after_1_run = process.runtime();
    assert!(runtime_after_1_run > Duration::from_millis(0));

    // Send the message and the actor should return Ok.
    actor_ref.send(()).unwrap();
    assert_eq!(process.as_mut().run(&mut system_ref), ProcessResult::Complete);
    assert!(process.runtime() > runtime_after_1_run);
}

async fn error_actor(mut ctx: ActorContext<()>, fail: bool) -> Result<(), ()> {
    if !fail {
        let _msg = await!(ctx.receive());
        Ok(())
    } else {
        Err(())
    }
}

#[test]
fn erroneous_actor_process() {
    // Create our actor.
    #[allow(trivial_casts)]
    let new_actor = error_actor as fn(_, _) -> _;
    let (actor, mut actor_ref) = test::init_actor(new_actor, true);

    // Create the waker.
    let pid = ProcessId(0);
    let (sender, _) = channel::unbounded();
    let waker = new_waker(pid, sender);

    // Create our process.
    let inbox = actor_ref.get_inbox().unwrap();
    let process = ActorProcess::new(pid, Priority::NORMAL,
        |_err| SupervisorStrategy::Stop, new_actor, actor, inbox, waker);
    let mut process = Box::pin(process);

    assert_eq!(process.id(), ProcessId(0));
    assert_eq!(process.priority(), Priority::NORMAL);
    assert_eq!(process.runtime(), Duration::from_millis(0));

    // Actor should return Err.
    let mut system_ref = test::system_ref();
    assert_eq!(process.as_mut().run(&mut system_ref), ProcessResult::Complete);
    assert!(process.runtime() > Duration::from_millis(0));
}

#[test]
fn restarting_erroneous_actor_process() {
    // Create our actor.
    #[allow(trivial_casts)]
    let new_actor = error_actor as fn(_, _) -> _;
    let (actor, mut actor_ref) = test::init_actor(new_actor, true);

    // Create the waker.
    let pid = ProcessId(0);
    let (sender, _) = channel::unbounded();
    let waker = new_waker(pid, sender);

    let supervisor_check = Arc::new(AtomicBool::new(false));
    let supervisor_called = Arc::clone(&supervisor_check);
    let supervisor = move |_err| {
        supervisor_called.store(true, atomic::Ordering::SeqCst);
        SupervisorStrategy::Restart(false)
    };

    // Create our process.
    let inbox = actor_ref.get_inbox().unwrap();
    let process = ActorProcess::new(pid, Priority::NORMAL, supervisor, new_actor,
        actor, inbox, waker);
    let mut process: Pin<Box<dyn Process>> = Box::pin(process);

    assert_eq!(process.id(), ProcessId(0));
    assert_eq!(process.priority(), Priority::NORMAL);
    assert_eq!(process.runtime(), Duration::from_millis(0));

    // In the first call to run the actor should return an error. Then it should
    // be restarted. The restarted actor waits for a message, returning
    // `Poll::Pending`.
    let mut system_ref = test::system_ref();
    assert_eq!(process.as_mut().run(&mut system_ref), ProcessResult::Pending);
    // Runtime must be increased after each call to run.
    let runtime_after_1_run = process.runtime();
    assert!(runtime_after_1_run > Duration::from_millis(0));
    // Supervisor must be called and the actor restarted.
    assert!(supervisor_check.load(atomic::Ordering::SeqCst));

    // Now we send a message to the restarted actor, which should return `Ok`.
    actor_ref.send(()).unwrap();
    assert_eq!(process.as_mut().run(&mut system_ref), ProcessResult::Complete);
    assert!(process.runtime() > runtime_after_1_run);
}

async fn sleepy_actor(ctx: ActorContext<()>, sleep_time: Duration) -> Result<(), !> {
    sleep(sleep_time);
    drop(ctx);
    Ok(())
}

#[test]
fn actor_process_runtime_increase() {
    const SLEEP_TIME: Duration = Duration::from_millis(10);

    // Create our actor.
    #[allow(trivial_casts)]
    let new_actor = sleepy_actor as fn(_, _) -> _;
    let (actor, mut actor_ref) = test::init_actor(new_actor, SLEEP_TIME);

    // Create the waker.
    let pid = ProcessId(0);
    let (sender, _) = channel::unbounded();
    let waker = new_waker(pid, sender);

    // Create our process.
    let inbox = actor_ref.get_inbox().unwrap();
    let process = ActorProcess::new(pid, Priority::NORMAL, NoopSupervisor,
        new_actor, actor, inbox, waker);
    let mut process = Box::pin(process);

    assert_eq!(process.id(), ProcessId(0));
    assert_eq!(process.priority(), Priority::NORMAL);
    assert_eq!(process.runtime(), Duration::from_millis(0));

    // Runtime must increase after running.
    let mut system_ref = test::system_ref();
    assert_eq!(process.as_mut().run(&mut system_ref), ProcessResult::Complete);
    assert!(process.runtime() >= SLEEP_TIME);
}

struct SimpleInitiator {
    called: Arc<AtomicUsize>,
}

impl Initiator for SimpleInitiator {
    fn clone_threaded(&self) -> io::Result<Self> {
        unreachable!();
    }

    fn init(&mut self, _: &mut Poller, _: ProcessId) -> io::Result<()> {
        unreachable!();
    }

    fn poll(&mut self, _: &mut ActorSystemRef) -> io::Result<()> {
        match self.called.fetch_add(1, atomic::Ordering::SeqCst) {
            0 => Ok(()),
            1 => Err(io::ErrorKind::Other.into()),
            _ => unreachable!(),
        }
    }
}

#[test]
fn initiator_process() {
    let called = Arc::new(AtomicUsize::new(0));
    let initiator = SimpleInitiator { called: Arc::clone(&called) };
    let mut process = InitiatorProcess::new(ProcessId(0), initiator);
    let mut process = Pin::new(&mut process);

    assert_eq!(process.id(), ProcessId(0));
    assert_eq!(process.priority(), Priority::LOW);
    assert_eq!(process.runtime(), Duration::from_millis(0));

    // Ok run.
    let mut system_ref = test::system_ref();
    assert_eq!(process.as_mut().run(&mut system_ref), ProcessResult::Pending);
    assert_eq!(called.load(atomic::Ordering::SeqCst), 1);
    // Runtime must be increased.
    let runtime_after_1_run = process.runtime();
    assert!(runtime_after_1_run > Duration::from_millis(0));

    // Error run.
    assert_eq!(process.as_mut().run(&mut system_ref), ProcessResult::Complete);
    assert_eq!(called.load(atomic::Ordering::SeqCst), 2);
    assert!(process.runtime() > runtime_after_1_run);
}

struct SleepyInitiator(Duration);

impl Initiator for SleepyInitiator {
    fn clone_threaded(&self) -> io::Result<Self> {
        unreachable!();
    }

    fn init(&mut self, _: &mut Poller, _: ProcessId) -> io::Result<()> {
        unreachable!();
    }

    fn poll(&mut self, _: &mut ActorSystemRef) -> io::Result<()> {
        sleep(self.0);
        Ok(())
    }
}

#[test]
fn initiator_process_runtime_increase() {
    const SLEEP_TIME: Duration = Duration::from_millis(10);

    let initiator = SleepyInitiator(SLEEP_TIME);
    let mut process = InitiatorProcess::new(ProcessId(0), initiator);
    let mut process = Pin::new(&mut process);

    assert_eq!(process.id(), ProcessId(0));
    assert_eq!(process.priority(), Priority::LOW);
    assert_eq!(process.runtime(), Duration::from_millis(0));

    let mut system_ref = test::system_ref();
    assert_eq!(process.as_mut().run(&mut system_ref), ProcessResult::Pending);
    assert!(process.runtime() >= SLEEP_TIME);
}
