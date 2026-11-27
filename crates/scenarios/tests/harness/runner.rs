use alondra_engine_ports::machine::MachineProbe;
use alondra_scenarios::runner::CalmMac;
use std::future::Future;
use std::task::{Context, Poll, Waker};

#[test]
fn a_started_sample_is_not_a_published_sample() {
    let mac = CalmMac::default();
    mac.sample();
    mac.1.publish();
    mac.sample();

    let mut ready = Box::pin(mac.wait_for_second_publication());
    let mut context = Context::from_waker(Waker::noop());
    assert!(matches!(ready.as_mut().poll(&mut context), Poll::Pending));
    mac.1.publish();
    assert!(matches!(ready.as_mut().poll(&mut context), Poll::Ready(())));
}

#[test]
fn a_checkpoint_waits_for_every_sample_that_started() {
    let mac = CalmMac::default();
    let mut context = Context::from_waker(Waker::noop());
    let mut idle = Box::pin(mac.wait_for_published_samples());
    assert!(matches!(idle.as_mut().poll(&mut context), Poll::Ready(())), "no sample started, none to wait for");
    mac.sample();
    let mut waiting = Box::pin(mac.wait_for_published_samples());
    assert!(matches!(waiting.as_mut().poll(&mut context), Poll::Pending));
    mac.1.publish();
    assert!(matches!(waiting.as_mut().poll(&mut context), Poll::Ready(())));
}
