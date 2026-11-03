//! Wall-clock scheduling primitives for the Mac daemon.
use dispatch2::{
    _dispatch_source_type_timer, DispatchObject, DispatchQueue, DispatchQueueAttr, DispatchSource,
    DispatchTime,
};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
use objc2_foundation::{
    NSNotificationCenter, NSSystemClockDidChangeNotification, NSSystemTimeZoneDidChangeNotification,
};
use std::ffi::c_void;
use std::ptr::NonNull;
use tokio::sync::watch;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockChange {
    SystemClock,
    TimeZone,
}

pub struct ClockChanges {
    receiver: watch::Receiver<ClockChange>,
    _observer: SystemClockObserver,
}

impl ClockChanges {
    pub fn new() -> Self {
        let (sender, receiver) = watch::channel(ClockChange::SystemClock);
        let center = NSNotificationCenter::defaultCenter();
        let clock_sender = sender.clone();
        let clock_block = block2::RcBlock::new(
            move |_notification: NonNull<objc2_foundation::NSNotification>| {
                clock_sender.send_replace(ClockChange::SystemClock);
            },
        );
        let clock_token = unsafe {
            center.addObserverForName_object_queue_usingBlock(
                Some(NSSystemClockDidChangeNotification),
                None,
                None,
                &clock_block,
            )
        };
        let zone_sender = sender;
        let zone_block = block2::RcBlock::new(
            move |_notification: NonNull<objc2_foundation::NSNotification>| {
                zone_sender.send_replace(ClockChange::TimeZone);
            },
        );
        let zone_token = unsafe {
            center.addObserverForName_object_queue_usingBlock(
                Some(NSSystemTimeZoneDidChangeNotification),
                None,
                None,
                &zone_block,
            )
        };
        Self {
            receiver,
            _observer: SystemClockObserver {
                center,
                tokens: vec![clock_token, zone_token],
            },
        }
    }

    pub fn receiver(&mut self) -> &mut watch::Receiver<ClockChange> {
        &mut self.receiver
    }
}

struct SystemClockObserver {
    center: Retained<NSNotificationCenter>,
    tokens: Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
}

// Foundation notification-center registration and removal are thread-safe;
// the opaque observer tokens are only passed back to that same center.
unsafe impl Send for SystemClockObserver {}

impl Drop for SystemClockObserver {
    fn drop(&mut self) {
        for token in &self.tokens {
            let object = unsafe {
                &*(token.as_ref() as *const ProtocolObject<dyn NSObjectProtocol>
                    as *const AnyObject)
            };
            unsafe { self.center.removeObserver(object) };
        }
    }
}

pub struct WallTimer(Option<dispatch2::DispatchRetained<DispatchSource>>);

impl WallTimer {
    pub fn at(unix_millis: i64) -> (Self, watch::Receiver<bool>) {
        let (sender, receiver) = watch::channel(false);
        let seconds = unix_millis.div_euclid(1_000);
        let nanos = unix_millis.rem_euclid(1_000) * 1_000_000;
        let instant = libc::timespec {
            tv_sec: seconds as _,
            tv_nsec: nanos as _,
        };
        let queue = DispatchQueue::new(
            "com.bitomule.alondra.schedule-clock",
            DispatchQueueAttr::SERIAL,
        );
        let source_type = unsafe { &_dispatch_source_type_timer as *const _ as *mut _ };
        let source = unsafe { DispatchSource::new(source_type, 0, 0, Some(&queue)) };
        let context = Box::into_raw(Box::new(sender)).cast::<c_void>();
        unsafe {
            source.set_context(context);
            source.set_finalizer_f(free_timer_context);
            source.set_event_handler_f(timer_fired);
            source.set_timer(
                DispatchTime::walltime(&instant, 0),
                DispatchTime::FOREVER.0,
                1_000_000_000,
            );
            source.activate();
        }
        (Self(Some(source)), receiver)
    }
}

impl Drop for WallTimer {
    fn drop(&mut self) {
        if let Some(source) = self.0.take() {
            source.cancel();
        }
    }
}

extern "C" fn timer_fired(context: *mut c_void) {
    let sender = unsafe { &*(context.cast::<watch::Sender<bool>>()) };
    sender.send_replace(true);
}

extern "C" fn free_timer_context(context: *mut c_void) {
    drop(unsafe { Box::from_raw(context.cast::<watch::Sender<bool>>()) });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_clock_and_time_zone_notifications_wake_the_clock_receiver() {
        let mut changes = ClockChanges::new();
        let center = NSNotificationCenter::defaultCenter();
        unsafe { center.postNotificationName_object(NSSystemTimeZoneDidChangeNotification, None) };
        assert!(changes.receiver.has_changed().unwrap());
        assert_eq!(*changes.receiver.borrow_and_update(), ClockChange::TimeZone);

        unsafe { center.postNotificationName_object(NSSystemClockDidChangeNotification, None) };
        assert!(changes.receiver.has_changed().unwrap());
        assert_eq!(
            *changes.receiver.borrow_and_update(),
            ClockChange::SystemClock
        );
    }
}
