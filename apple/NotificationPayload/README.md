# Notification payloads

The notification extension and app share this parser for the encrypted views payload. The
payload names a card and carries up to four rendered actions, including their opaque action
IDs, option IDs, labels and text-input hints. The extension maps those actions to `A0` through
`A3`; the app resolves that index against the authenticated payload and posts the exact action
to `/actions` through `ViewActionSender`.

Question category identifiers are scoped to both the push binding and card ID. Retention is
global to the notification service extension because UserNotifications stores categories
process-wide. The extension keeps the existing bounded retention and asynchronous registration
polling behavior.
