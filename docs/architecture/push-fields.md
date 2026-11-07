# What Apple reads in a push

Apple delivers every push, so the relay and Apple read each field of the body below. Each field the
relay sends to APNs has a line here; a test in `crates/relay/src/push.rs` fails on one that does not.

## Alert, background and badge pushes

- `aps.alert`: a fixed text, shown only by a device that cannot open the envelope.
- `aps.mutable-content`: asks iOS to run the notification service extension, which opens the envelope.
- `aps.content-available`: wakes the app without showing anything.
- `aps.badge`: the number of cards waiting in the Inbox of a Mac, in the clear, when that number
  drops. iOS sets the icon badge from it without running any of our code, so it cannot be sealed.
  The same count is sealed everywhere else.
- `alondra.envelope`: the sealed message and its routing header.
- `alondra.push_binding`: lets the phone stop its own notifications.

## Live Activity pushes

- `aps.timestamp`, `aps.event`, `aps.stale-date`, `aps.dismissal-date`: when the activity starts,
  updates, goes stale and ends.
- `aps.content-state`: the sealed envelope of each Mac, and how many Macs did not fit.
- `aps.attributes-type`, `aps.attributes`, `aps.input-push-token`: what iOS needs to start the
  activity, and when it started.
