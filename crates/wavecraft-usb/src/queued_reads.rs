//! Bookkeeping for the asynchronous bulk reads the DSLogic and DSCope drivers
//! keep queued while a capture runs.
//!
//! A capture queues a trigger-header read and one or more payload reads, then
//! services libusb events until they complete. Whatever is still queued when a
//! capture ends or fails has to be cancelled and waited for: libusb keeps
//! writing into a transfer's buffer until it reports completion, so a pending
//! read can never simply be dropped.

use crate::ds_driver_util::{capture_error, combine_results};
use crate::transport::{QueuedBulkRead, QueuedBulkReadOutcome};
use crate::{UsbDeviceSession, UsbError};
use std::collections::VecDeque;
use std::time::Duration;
use wavecraft_core::CaptureError;

/// How long one wait for libusb events may block. It bounds how quickly a
/// capture notices cancellation, since the wait is the only thing that drives
/// queued transfers forward.
const USB_EVENT_POLL: Duration = Duration::from_millis(5);

/// Drives libusb so queued transfers can make progress and complete.
pub(crate) fn service_usb_events(session: &UsbDeviceSession) -> Result<(), CaptureError> {
    session
        .handle_usb_events(USB_EVENT_POLL)
        .map_err(capture_error)
}

/// The operations the capture drivers need from one queued read. The real
/// implementation is [`QueuedBulkRead`]; the trait lets the bookkeeping be
/// tested without a device.
pub(crate) trait QueuedRead {
    fn is_complete(&self) -> bool;
    fn cancel(&mut self) -> Result<(), UsbError>;
    fn finish(self) -> Result<QueuedBulkReadOutcome, UsbError>;
}

impl QueuedRead for QueuedBulkRead {
    fn is_complete(&self) -> bool {
        QueuedBulkRead::is_complete(self)
    }

    fn cancel(&mut self) -> Result<(), UsbError> {
        QueuedBulkRead::cancel(self)
    }

    fn finish(self) -> Result<QueuedBulkReadOutcome, UsbError> {
        QueuedBulkRead::finish(self)
    }
}

/// Turns a finished read into its data, naming the transfer when it was
/// cancelled although nothing asked for that.
pub(crate) fn require_queued_data(
    outcome: QueuedBulkReadOutcome,
    transfer: &str,
) -> Result<Vec<u8>, CaptureError> {
    match outcome {
        QueuedBulkReadOutcome::Data(data) => Ok(data),
        QueuedBulkReadOutcome::Cancelled => Err(CaptureError::new(format!(
            "{transfer} transfer was unexpectedly cancelled"
        ))),
    }
}

/// One trigger-header read and a first-in, first-out queue of payload reads.
/// Each payload read carries driver-specific `meta` data (the number of valid
/// bytes it is expected to hold, for example).
pub(crate) struct QueuedReads<M, R = QueuedBulkRead> {
    header: Option<R>,
    payloads: VecDeque<(R, M)>,
}

impl<M, R> Default for QueuedReads<M, R> {
    fn default() -> Self {
        Self {
            header: None,
            payloads: VecDeque::new(),
        }
    }
}

impl<M, R: QueuedRead> QueuedReads<M, R> {
    pub(crate) fn set_header(&mut self, read: R) {
        self.header = Some(read);
    }

    pub(crate) fn push_payload(&mut self, read: R, meta: M) {
        self.payloads.push_back((read, meta));
    }

    pub(crate) fn payload_count(&self) -> usize {
        self.payloads.len()
    }

    /// Returns the header data once its read has completed, and `None` while
    /// it is still pending or was already taken. `transfer` names the read in
    /// the error reported when it was cancelled.
    pub(crate) fn take_completed_header(
        &mut self,
        transfer: &str,
    ) -> Result<Option<Vec<u8>>, CaptureError> {
        match self.header.take() {
            Some(read) if read.is_complete() => {
                require_queued_data(read.finish().map_err(capture_error)?, transfer).map(Some)
            }
            pending => {
                self.header = pending;
                Ok(None)
            }
        }
    }

    /// Returns the oldest payload read's data and `meta` once it has
    /// completed. Payloads are always taken in queue order: a completed read
    /// behind a pending one waits for it.
    pub(crate) fn take_completed_payload(
        &mut self,
        transfer: &str,
    ) -> Result<Option<(Vec<u8>, M)>, CaptureError> {
        if !self
            .payloads
            .front()
            .is_some_and(|(read, _)| read.is_complete())
        {
            return Ok(None);
        }
        let Some((read, meta)) = self.payloads.pop_front() else {
            return Ok(None);
        };
        let data = require_queued_data(read.finish().map_err(capture_error)?, transfer)?;
        Ok(Some((data, meta)))
    }

    fn has_incomplete(&self) -> bool {
        self.header.as_ref().is_some_and(|read| !read.is_complete())
            || self.payloads.iter().any(|(read, _)| !read.is_complete())
    }

    /// Cancels every queued read, waits for libusb to hand each one back, and
    /// discards the results. `force_ready` runs first when anything is queued
    /// so the device stops producing data; `service_events` drives libusb.
    ///
    /// A failed cancel returns at once: the transfer might never complete, so
    /// waiting for it could hang, and a dropped pending read is leaked
    /// safely instead of freed.
    fn cancel_and_drain_with(
        &mut self,
        force_ready: impl FnOnce(),
        service_events: impl FnMut() -> Result<(), CaptureError>,
    ) -> Result<(), CaptureError> {
        if self.header.is_none() && self.payloads.is_empty() {
            return Ok(());
        }

        force_ready();
        if let Some(header) = self.header.as_mut() {
            header.cancel().map_err(capture_error)?;
        }
        for (read, _) in &mut self.payloads {
            read.cancel().map_err(capture_error)?;
        }

        let mut service_events = service_events;
        while self.has_incomplete() {
            service_events()?;
        }

        if let Some(header) = self.header.take() {
            let _ = header.finish();
        }
        while let Some((read, _)) = self.payloads.pop_front() {
            let _ = read.finish();
        }
        Ok(())
    }
}

impl<M> QueuedReads<M> {
    /// Cancels and waits for everything still queued; see
    /// [`cancel_and_drain_with`](Self::cancel_and_drain_with).
    pub(crate) fn cancel_and_drain(
        &mut self,
        session: &UsbDeviceSession,
    ) -> Result<(), CaptureError> {
        self.cancel_and_drain_with(
            || {
                let _ = session.force_capture_ready();
            },
            || service_usb_events(session),
        )
    }
}

/// Runs `capture` with an empty set of queued reads, then cancels whatever it
/// left queued whether it succeeded or not. When both fail, both errors are
/// reported, with `cleanup_action` naming the cleanup step.
pub(crate) fn with_queued_reads<M>(
    session: &UsbDeviceSession,
    cleanup_action: &str,
    capture: impl FnOnce(&mut QueuedReads<M>) -> Result<(), CaptureError>,
) -> Result<(), CaptureError> {
    let mut reads = QueuedReads::default();
    let capture_result = capture(&mut reads);
    let cleanup_result = reads.cancel_and_drain(session);
    combine_results(capture_result, cleanup_result, cleanup_action)
}

/// Cancels the given reads, waits for them to complete, and discards them.
pub(crate) fn abandon_reads(
    session: &UsbDeviceSession,
    reads: Vec<QueuedBulkRead>,
) -> Result<(), CaptureError> {
    abandon_reads_with(reads, || service_usb_events(session))
}

/// Abandons the reads after `error` ended a capture and returns the error to
/// report: `error` itself, plus the cleanup failure when abandoning failed too.
pub(crate) fn abandon_reads_after_error(
    session: &UsbDeviceSession,
    error: CaptureError,
    reads: Vec<QueuedBulkRead>,
) -> CaptureError {
    abandon_reads_after_error_with(error, reads, || service_usb_events(session))
}

fn abandon_reads_after_error_with<R: QueuedRead>(
    error: CaptureError,
    reads: Vec<R>,
    service_events: impl FnMut() -> Result<(), CaptureError>,
) -> CaptureError {
    let cleanup = abandon_reads_with(reads, service_events);
    match combine_results(Err(error.clone()), cleanup, "cancel queued reads") {
        Ok(()) => error,
        Err(combined) => combined,
    }
}

fn abandon_reads_with<R: QueuedRead>(
    mut reads: Vec<R>,
    mut service_events: impl FnMut() -> Result<(), CaptureError>,
) -> Result<(), CaptureError> {
    for read in &mut reads {
        read.cancel().map_err(capture_error)?;
    }
    while reads.iter().any(|read| !read.is_complete()) {
        service_events()?;
    }
    for read in reads {
        let _ = read.finish();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    type Log = Rc<RefCell<Vec<String>>>;

    /// A queued read that completes when a test says so and records what the
    /// bookkeeping does to it.
    struct FakeRead {
        name: &'static str,
        log: Log,
        complete: Rc<Cell<bool>>,
        cancel_error: Option<&'static str>,
        outcome: Result<QueuedBulkReadOutcome, &'static str>,
    }

    impl QueuedRead for FakeRead {
        fn is_complete(&self) -> bool {
            self.complete.get()
        }

        fn cancel(&mut self) -> Result<(), UsbError> {
            self.log.borrow_mut().push(format!("cancel {}", self.name));
            match self.cancel_error {
                Some(message) => Err(UsbError::new(message)),
                None => Ok(()),
            }
        }

        fn finish(self) -> Result<QueuedBulkReadOutcome, UsbError> {
            self.log.borrow_mut().push(format!("finish {}", self.name));
            self.outcome.map_err(UsbError::new)
        }
    }

    struct Fixture {
        log: Log,
        flags: Vec<Rc<Cell<bool>>>,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                log: Rc::default(),
                flags: Vec::new(),
            }
        }

        fn read(&mut self, name: &'static str, complete: bool, data: &[u8]) -> FakeRead {
            self.read_with(
                name,
                complete,
                Ok(QueuedBulkReadOutcome::Data(data.to_vec())),
            )
        }

        fn read_with(
            &mut self,
            name: &'static str,
            complete: bool,
            outcome: Result<QueuedBulkReadOutcome, &'static str>,
        ) -> FakeRead {
            let flag = Rc::new(Cell::new(complete));
            self.flags.push(Rc::clone(&flag));
            FakeRead {
                name,
                log: Rc::clone(&self.log),
                complete: flag,
                cancel_error: None,
                outcome,
            }
        }

        /// Models libusb returning every cancelled transfer during one event
        /// pass.
        fn service_events(&self) -> impl FnMut() -> Result<(), CaptureError> + '_ {
            move || {
                self.log.borrow_mut().push("service".to_owned());
                for flag in &self.flags {
                    flag.set(true);
                }
                Ok(())
            }
        }

        fn log(&self) -> Vec<String> {
            self.log.borrow().clone()
        }
    }

    #[test]
    fn a_cancelled_transfer_is_reported_by_name() {
        assert_eq!(
            require_queued_data(QueuedBulkReadOutcome::Data(vec![1, 2]), "DSCope payload"),
            Ok(vec![1, 2])
        );
        assert_eq!(
            require_queued_data(QueuedBulkReadOutcome::Cancelled, "DSCope trigger header")
                .unwrap_err()
                .to_string(),
            "DSCope trigger header transfer was unexpectedly cancelled"
        );
    }

    #[test]
    fn the_header_is_taken_once_after_it_completes() {
        let mut fixture = Fixture::new();
        let mut reads = QueuedReads::<(), FakeRead>::default();
        assert_eq!(reads.take_completed_header("header"), Ok(None));

        reads.set_header(fixture.read("header", false, &[7, 8]));
        assert_eq!(reads.take_completed_header("header"), Ok(None));
        assert!(
            fixture.log().is_empty(),
            "a pending header must not be finished"
        );

        fixture.flags[0].set(true);
        assert_eq!(reads.take_completed_header("header"), Ok(Some(vec![7, 8])));
        assert_eq!(reads.take_completed_header("header"), Ok(None));
        assert_eq!(fixture.log(), ["finish header"]);
    }

    #[test]
    fn a_cancelled_header_is_an_error() {
        let mut fixture = Fixture::new();
        let mut reads = QueuedReads::<(), FakeRead>::default();
        let header = fixture.read_with("header", true, Ok(QueuedBulkReadOutcome::Cancelled));
        reads.set_header(header);

        assert_eq!(
            reads
                .take_completed_header("stream trigger-header")
                .unwrap_err()
                .to_string(),
            "stream trigger-header transfer was unexpectedly cancelled"
        );
    }

    #[test]
    fn payloads_are_taken_in_queue_order_with_their_meta_data() {
        let mut fixture = Fixture::new();
        let mut reads = QueuedReads::<usize, FakeRead>::default();
        assert_eq!(reads.take_completed_payload("payload"), Ok(None));

        let first = fixture.read("first", false, &[1]);
        let second = fixture.read("second", true, &[2, 2]);
        reads.push_payload(first, 10);
        reads.push_payload(second, 20);
        assert_eq!(reads.payload_count(), 2);

        // The second read is complete, but it waits behind the pending first.
        assert_eq!(reads.take_completed_payload("payload"), Ok(None));
        assert_eq!(reads.payload_count(), 2);

        fixture.flags[0].set(true);
        assert_eq!(
            reads.take_completed_payload("payload"),
            Ok(Some((vec![1], 10)))
        );
        assert_eq!(
            reads.take_completed_payload("payload"),
            Ok(Some((vec![2, 2], 20)))
        );
        assert_eq!(reads.take_completed_payload("payload"), Ok(None));
        assert_eq!(reads.payload_count(), 0);
    }

    #[test]
    fn a_failed_or_cancelled_payload_is_an_error() {
        let mut fixture = Fixture::new();
        let mut reads = QueuedReads::<(), FakeRead>::default();
        reads.push_payload(fixture.read_with("failed", true, Err("bus reset")), ());
        assert_eq!(
            reads
                .take_completed_payload("payload")
                .unwrap_err()
                .to_string(),
            "bus reset"
        );

        reads.push_payload(
            fixture.read_with("cancelled", true, Ok(QueuedBulkReadOutcome::Cancelled)),
            (),
        );
        assert_eq!(
            reads
                .take_completed_payload("loop-stream payload")
                .unwrap_err()
                .to_string(),
            "loop-stream payload transfer was unexpectedly cancelled"
        );
    }

    #[test]
    fn nothing_is_cancelled_or_forced_ready_while_no_read_is_queued() {
        let fixture = Fixture::new();
        let mut reads = QueuedReads::<(), FakeRead>::default();

        let result = reads.cancel_and_drain_with(
            || fixture.log.borrow_mut().push("force ready".to_owned()),
            fixture.service_events(),
        );

        assert_eq!(result, Ok(()));
        assert!(fixture.log().is_empty());
    }

    #[test]
    fn cancelling_waits_for_every_read_and_empties_the_queue() {
        let mut fixture = Fixture::new();
        let mut reads = QueuedReads::<(), FakeRead>::default();
        let header = fixture.read("header", false, &[]);
        let first = fixture.read("first", false, &[]);
        let second = fixture.read("second", true, &[]);
        reads.set_header(header);
        reads.push_payload(first, ());
        reads.push_payload(second, ());

        let result = reads.cancel_and_drain_with(
            || fixture.log.borrow_mut().push("force ready".to_owned()),
            fixture.service_events(),
        );

        assert_eq!(result, Ok(()));
        assert_eq!(
            fixture.log(),
            [
                "force ready",
                "cancel header",
                "cancel first",
                "cancel second",
                "service",
                "finish header",
                "finish first",
                "finish second",
            ]
        );
        assert_eq!(reads.payload_count(), 0);
        assert_eq!(reads.take_completed_header("header"), Ok(None));
    }

    #[test]
    fn events_are_serviced_until_every_read_has_completed() {
        let mut fixture = Fixture::new();
        let mut reads = QueuedReads::<(), FakeRead>::default();
        reads.set_header(fixture.read("header", false, &[]));
        reads.push_payload(fixture.read("payload", false, &[]), ());
        let mut passes = 0;

        let result = reads.cancel_and_drain_with(
            || {},
            || {
                // libusb hands back one cancelled transfer per event pass.
                fixture.flags[passes].set(true);
                passes += 1;
                Ok(())
            },
        );

        assert_eq!(result, Ok(()));
        assert_eq!(passes, 2);
    }

    #[test]
    fn a_failed_cancel_returns_without_waiting_for_the_transfer() {
        let mut fixture = Fixture::new();
        let mut reads = QueuedReads::<(), FakeRead>::default();
        reads.set_header(fixture.read("header", false, &[]));
        let mut stuck = fixture.read("stuck", false, &[]);
        stuck.cancel_error = Some("libusb error -99");
        reads.push_payload(stuck, ());
        reads.push_payload(fixture.read("later", false, &[]), ());

        let error = reads
            .cancel_and_drain_with(|| {}, fixture.service_events())
            .unwrap_err();

        assert_eq!(error.to_string(), "libusb error -99");
        assert_eq!(fixture.log(), ["cancel header", "cancel stuck"]);
    }

    #[test]
    fn a_failure_while_servicing_events_is_reported_and_nothing_is_finished() {
        let mut fixture = Fixture::new();
        let mut reads = QueuedReads::<(), FakeRead>::default();
        reads.set_header(fixture.read("header", false, &[]));

        let error = reads
            .cancel_and_drain_with(|| {}, || Err(CaptureError::new("device disconnected")))
            .unwrap_err();

        assert_eq!(error.to_string(), "device disconnected");
        assert_eq!(fixture.log(), ["cancel header"]);
    }

    #[test]
    fn abandoned_reads_are_cancelled_waited_for_and_finished() {
        let mut fixture = Fixture::new();
        let reads = vec![
            fixture.read("header", false, &[]),
            fixture.read("payload", false, &[]),
        ];

        let result = abandon_reads_with(reads, fixture.service_events());

        assert_eq!(result, Ok(()));
        assert_eq!(
            fixture.log(),
            [
                "cancel header",
                "cancel payload",
                "service",
                "finish header",
                "finish payload",
            ]
        );
    }

    #[test]
    fn abandoning_waits_for_every_read_before_finishing_any() {
        let mut fixture = Fixture::new();
        let reads = vec![
            fixture.read("header", false, &[]),
            fixture.read("payload", false, &[]),
        ];
        let mut passes = 0;

        let result = abandon_reads_with(reads, || {
            fixture.log.borrow_mut().push("service".to_owned());
            fixture.flags[passes].set(true);
            passes += 1;
            Ok(())
        });

        assert_eq!(result, Ok(()));
        assert_eq!(
            fixture.log(),
            [
                "cancel header",
                "cancel payload",
                "service",
                "service",
                "finish header",
                "finish payload",
            ]
        );
    }

    #[test]
    fn abandoning_reads_after_an_error_keeps_that_error() {
        let mut fixture = Fixture::new();
        let reads = vec![fixture.read("header", false, &[])];

        let error = abandon_reads_after_error_with(
            CaptureError::new("failed to start the capture"),
            reads,
            fixture.service_events(),
        );

        assert_eq!(error.to_string(), "failed to start the capture");
    }

    #[test]
    fn a_failed_cleanup_is_added_to_the_original_error() {
        let mut fixture = Fixture::new();
        let mut stuck = fixture.read("header", false, &[]);
        stuck.cancel_error = Some("libusb error -99");

        let error = abandon_reads_after_error_with(
            CaptureError::new("failed to start the capture"),
            vec![stuck],
            fixture.service_events(),
        );

        assert_eq!(
            error.to_string(),
            "failed to start the capture; additionally failed to cancel queued reads: libusb error -99"
        );
    }
}
