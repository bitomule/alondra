# The iPhone Live Activity

Three counts, summed over every Mac of the account, on the Lock Screen and in the Dynamic Island
while something waits or works: Inbox, Waiting and Working. Neither Apple nor the relay reads them.

## Who does what

- **Each Mac** (`crates/alondrad/src/activity_push.rs`) seals its counts for each phone in an
  envelope of kind `activity`, and posts it to `/v1/mac/activity` with three flags in the clear:
  `active`, `may_start` (the phone is outside its quiet hours) and `urgent` (the Inbox grew).
  Changes settle for 5 s. While active, the state repeats every 15 min even if nothing changed.
- **The relay** (`crates/relay/src/activity.rs`) keeps the latest envelope of each Mac for each
  phone, unopened, and starts, updates, rotates and ends the Live Activity through APNs.
- **The phone** registers its ActivityKit tokens at the relay once per pairing, under one
  `phone_id`, so the relay knows which pairings feed the same Live Activity. Tokens go to the relay
  only, never to a Mac.
- **The widget extension** opens the envelopes when it draws and adds them up. It reads the key
  from a file in the App Group it shares with the app, because the process that draws a Live
  Activity has no Keychain (see below).
- **The app** writes the Macs it is connected to in the clear, as `reports`. It keeps the sealed
  reports of every other Mac, so a foreground update never loses their counts.

An activity stays known to the relay until APNs takes its end, and a sweep once a minute picks up
what no report will: a push lost to a restart, or a failed end. One started for an app closed by
force has no update token, so it cannot be ended; nothing starts on top of it until it rotates. A
Mac told a phone is gone asks again after an hour: Live Activities turned back on register the same
phone with the same binding.

## The key

Each pairing has a key used for nothing else, made by the phone and kept in
`live-activity-keys.json` in the App Group. A stolen file reveals the counts and the Mac's name at
most, never the content of a card. It is readable after the first unlock, stays out of backups and
leaves with the account. The phone gives the public half to the Mac through `/device/activity-key`,
and a Mac without it sends that phone no reports.

Measured on an iPhone, in the process that draws the Live Activity (`AlondraLiveActivity`), with the
`keychain-access-groups` entitlement in the binary: reading the shared item, reading with no group
and adding to its own group all return `errSecNotAvailable` (-25291), locked and unlocked. The app
reads the same item with status 0, saved as `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`.
That process reads an App Group file on a locked phone.

## The push

`aps.content-state` is `{"macs": [envelope, …], "omitted": n}`. The extension keeps the newest
report of each Mac by `seq`, ignores an envelope no pairing opens, and does not apply the
envelope's expiry: the relay resends a Mac's envelope when another Mac changes, and
`measured_at_ms` says how old it is. When nothing opens, the Live Activity asks to open the app.

A push-to-start carries `attributes: {"started_at_ms": …}` and a fixed alert, translated on the
phone from its `loc-key`, that never sounds.

## Limits

Apple publishes no number for the budget of priority 10, and sets none for priority 5, which iOS
delivers when it suits it. Limits are per phone, so a second Mac does not double them.

| What | Limit |
|---|---|
| Priority 5, every update that is not urgent | at most one a minute |
| Priority 10 | only when the Inbox grew, frequent updates are on and it is outside quiet hours; at most one every 5 min |
| Start | at most one every 10 min, never in quiet hours; rotation at 7 h 45 min |
| Start, end, priority 10 | wait 2 s, so reports that arrive together go as one push |
| Stale date | 30 min after the last push |
| Macs in one push | 4, the rest counted in `omitted` |

### What the limits are based on

A replay of 14 days of real task states through these rules, on one Mac that always had something
active. It approximates the counts from task states, and takes quiet hours as 23:00 to 8:00.

| Per hour | Before | With these limits |
|---|---|---|
| Pushes, mean | 10.9 | 9.9 |
| Pushes, 95th percentile | 27 | 22 |
| Pushes, worst hour | 44 | 33 |
| Priority 10, mean | 3.0 | 1.4 |
| Priority 10, worst hour | 13 | 7 |

An update with one Mac takes 792 bytes with fixture ids, and a real envelope about 800 bytes, so
four fit in Apple's 4,096. The silent push cannot carry this: Apple suggests no more than two or
three an hour.

## Open on a real device

The simulator cannot answer these; each needs a signed build on an iPhone.

- The delay of priority 5 updates, and whether a late repeat shows "No news" with no reason.
- The battery cost.
- The real Dynamic Island: the simulator widens it for the activity, but `simctl io screenshot`
  leaves what it shows out of the image.
