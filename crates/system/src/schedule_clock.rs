//! The Mac's wall clock as the schedule runner waits on it: a dispatch timer on the wall clock, which
//! fires also after a sleep, and Foundation's notifications of a new system clock or time zone.
use alondra_engine_ports::schedule_clock::{ClockChange, ScheduleClock, Watch};
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

pub struct MacClock;

impl ScheduleClock for MacClock {
    fn changes(&self) -> Watch<ClockChange> {
        let (receiver, observer) = observe_clock();
        Watch::new(receiver, observer)
    }

    fn timer_at(&self, unix_millis: i64) -> Watch<bool> {
        let (timer, fired) = WallTimer::at(unix_millis);
        Watch::new(fired, timer)
    }
}

fn observe_clock() -> (watch::Receiver<ClockChange>, SystemClockObserver) {
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
        (receiver, SystemClockObserver { center, tokens: vec![clock_token, zone_token] })
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

struct WallTimer(Option<dispatch2::DispatchRetained<DispatchSource>>);

impl WallTimer {
    fn at(unix_millis: i64) -> (Self, watch::Receiver<bool>) {
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
        let mut changes = MacClock.changes();
        let center = NSNotificationCenter::defaultCenter();
        unsafe { center.postNotificationName_object(NSSystemTimeZoneDidChangeNotification, None) };
        assert!(changes.receiver.has_changed().unwrap());
        assert_eq!(*changes.receiver.borrow_and_update(), ClockChange::TimeZone);

        unsafe { center.postNotificationName_object(NSSystemClockDidChangeNotification, None) };
        assert!(changes.receiver.has_changed().unwrap());
        assert_eq!(*changes.receiver.borrow_and_update(), ClockChange::SystemClock);
    }

    #[tokio::test]
    async fn a_wall_timer_fires_once_the_wall_clock_reaches_its_instant() {
        let mut timer = MacClock.timer_at(alondra_core::now_millis() + 50);
        assert!(!*timer.receiver.borrow());
        tokio::time::timeout(std::time::Duration::from_secs(30), timer.receiver.wait_for(|fired| *fired)).await.unwrap().unwrap();
    }
}
