//! Tracing utilities.

#![allow(dead_code, unused_imports, missing_docs, missing_debug_implementations)]

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::mem::size_of;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use log::warn;

use crate::rt::ProcessId;

/// Default buffer size of `Log`.
const BUF_SIZE: usize = 512;

/// Trace log.
#[derive(Debug)]
pub(crate) struct Log {
    /// File to write the trace to.
    ///
    /// This file is shared between one or more thread, thus writes to it should
    /// be atomic, e.g. no partial writes. Most OSs support atomic writes up to
    /// a page size (usally 4kB).
    file: File,
    /// Used to buffer writes for a single event.
    buf: Vec<u8>,
    /// Id of the stream, used in writing events.
    stream_id: u32,
    /// Time which we use as zero, or epoch, time for all events.
    epoch: Instant,
}

impl Log {
    /// Open a new trace `Log`.
    pub(super) fn open(path: &Path, stream_id: u32) -> io::Result<Log> {
        let timestamp = SystemTime::now();
        let epoch = Instant::now();

        let mut log = OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)
            .map(|file| Log {
                file,
                stream_id,
                buf: Vec::with_capacity(BUF_SIZE),
                epoch,
            })?;

        // Write the metadata for the trace log, currently that's only the time
        // at which it's recorded.
        // TODO: use a better format.
        write!(&mut log.buf, "{{\"timestamp\":\"{:?}\"}}\n", timestamp)
            .unwrap_or_else(|_| unreachable!());
        write_once(&log.file, &log.buf)?;

        Ok(log)
    }

    /// Create a new stream with `stream_id`, writing to the same (duplicated)
    /// file.
    pub(super) fn new_stream(&self, stream_id: u32) -> io::Result<Log> {
        self.file.try_clone().map(|file| Log {
            file,
            stream_id,
            buf: Vec::with_capacity(BUF_SIZE),
            epoch: self.epoch,
        })
    }

    /// Append `event` to trace `Log`.
    pub(crate) fn append<E>(&mut self, event: &Metadata<E>) -> io::Result<()>
    where
        E: Event,
    {
        self.buf.clear();
        // TODO: use a better format than butchered JSON.
        write!(
            &mut self.buf,
            "{{\
                \"stream_id\":{},\
                \"description\":\"{}\",\
                \"start\":{},\
                \"end\":{},\
                \"attributes\":",
            self.stream_id,
            E::DESCRIPTION,
            nanos_since_epoch(self.epoch, event.start),
            nanos_since_epoch(self.epoch, event.end),
        )
        .unwrap_or_else(|_| unreachable!());
        event.event.write_attributes(&mut self.buf);
        self.buf.extend_from_slice(b"}\n");
        write_once(&self.file, &self.buf)
    }
}

/// Returns the number of nanoseconds since `epoch`.
///
/// (2 ^ 64) / 1000000000 / (365 * 24 * 60 * 60) ~= 584 years.
/// So restart the application once every 500 years and you're good.
#[track_caller]
fn nanos_since_epoch(epoch: Instant, time: Instant) -> u64 {
    time.duration_since(epoch).as_nanos() as u64
}

/// Start timing for an event (using [`EventTiming`]) if we're tracing, i.e. if
/// `log` is `Some`.
pub(crate) fn start(log: &Option<Log>) -> Option<EventTiming> {
    if log.is_some() {
        Some(EventTiming::start())
    } else {
        None
    }
}

/// Finish a trace, partner function to [`start`].
///
/// If `log` is `Some` `timing` must also be `Some.
#[track_caller]
pub(crate) fn finish<E>(log: &mut Option<Log>, timing: Option<EventTiming>, event: E)
where
    E: Event,
{
    if let Some(log) = log.as_mut() {
        let timing = timing.unwrap();
        let event = timing.finish(event);
        if let Err(err) = log.append(&event) {
            warn!("error writing trace data: {}", err);
        }
    }
}

/// Write the entire `buf`fer into the `output` or return an error.
#[inline(always)]
fn write_once<W>(mut output: W, buf: &[u8]) -> io::Result<()>
where
    W: Write,
{
    output.write(buf).and_then(|written| {
        if written != buf.len() {
            // Not completely correct when going by the name alone, but it's the
            // closest we can get to a descriptive error.
            Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "failed to write entire trace event",
            ))
        } else {
            Ok(())
        }
    })
}

/// Metadata wrapping an [`Event`].
#[derive(Debug)]
pub(crate) struct Metadata<E> {
    start: Instant,
    end: Instant,
    event: E,
}

/// Time an [`Event`].
#[derive(Debug)]
pub(crate) struct EventTiming {
    start: Instant,
}

impl EventTiming {
    /// Start timing.
    pub(crate) fn start() -> EventTiming {
        let start = Instant::now();
        EventTiming { start }
    }

    /// Finish timing `event`.
    pub(crate) fn finish<E>(self, event: E) -> Metadata<E> {
        let end = Instant::now();
        Metadata {
            start: self.start,
            end,
            event,
        }
    }
}

/// Trait that defines an event.
///
/// An event is simple, it has a human-readable description and optional
/// attributes. To create an event type use the [`event!`] macro.
pub(crate) trait Event {
    /// Description of the event in human-readable text, e.g. `run process`.
    const DESCRIPTION: &'static str;

    /// Write attributes related to this event to the `buf`fer.
    ///
    /// For example if we're running a process we would write the process id as
    /// a possible attribute.
    fn write_attributes(&self, buf: &mut Vec<u8>);

    /// Use the [`event!`] macro instead.
    #[doc(hidden)]
    fn _do_not_manually_implement_this_it_wont_work();
}

/// Macro to create a new [`Event`] type.
macro_rules! event {
    ($msg: expr) => {{
        #[derive(Copy, Clone)]
        #[allow(missing_docs)]
        struct Event;

        impl $crate::rt::trace::Event for Event {
            const DESCRIPTION: &'static str = $msg;

            fn write_attributes(&self, buf: &mut Vec<u8>) {
                buf.extend_from_slice(b"{}");
            }

            fn _do_not_manually_implement_this_it_wont_work() {}
        }

        impl std::fmt::Debug for Event {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                use $crate::rt::trace::Event;
                f.debug_struct("Event")
                    .field("description", &Self::DESCRIPTION)
                    .finish()
            }
        }

        Event
    }};
    ($msg: expr, {
        $( $attribute_key: ident : $attribute_type: ty = $attribute_value: expr),+ $(,)*
    }) => {{
        #[derive(Copy, Clone)]
        #[allow(missing_docs)]
        struct Event {
            $( $attribute_key : $attribute_type ),+
        }

        impl $crate::rt::trace::Event for Event {
            const DESCRIPTION: &'static str = $msg;

            fn write_attributes(&self, buf: &mut Vec<u8>) {
                use std::io::Write;
                // TODO: don't write integers in quotes.
                write!(buf,
                    concat!("{{", $( "\"", stringify!($attribute_key), "\":\"{}\"," ),+ ),
                    $( &self.$attribute_key ),+
                ).unwrap_or_else(|_| unreachable!());
                let _ = buf.pop(); // Remove last `,` not allowed in JSON.
                buf.extend_from_slice(b"}");
            }

            fn _do_not_manually_implement_this_it_wont_work() {}
        }

        impl std::fmt::Debug for Event {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                use $crate::rt::trace::Event;
                f.debug_struct("Event")
                    .field("description", &Self::DESCRIPTION)
                    $( .field(stringify!($attribute_key), &self.$attribute_key) )+
                    .finish()
            }
        }

        Event {
            $( $attribute_key : $attribute_value ),+
        }
    }};
}
