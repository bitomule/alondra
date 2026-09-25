# Integration in the central Apple BUILD

This branch only adds the payload package. The C integration branch owns `apple/BUILD.bazel` and changes the extension atomically with its new dependency. Add this dependency to `AlondraNotificationServiceLib` there:

```starlark
deps = ["//apple/NotificationPayload:AlondraNotificationPayload"],
```

In `iOSNotificationService/Sources/NotificationService.swift`, import `AlondraNotificationPayload`; replace `OptionQuestion(userInfo:)` with `NotificationPayload.Question(userInfo:)` and remove the old `OptionQuestion` definition. Change `QuestionCategories.register` and `category` to accept `NotificationPayload.Question`. Use `NotificationPayload.Category.questionID(question.id)` for the generated identifier and `NotificationPayload.Category.questionID(from: category.identifier)` when filtering existing categories. Use `NotificationPayload.Action.option(label)`, `.reply`, and `.moreContext` for its action IDs. Leave `Delivery` and `QuestionCategories.register` concurrency and timing unchanged: the single content-handler invocation, 20 category registration polls, 50 ms poll interval, and fallback on expiry are existing behavior.

Once the iOS app replaces its duplicate `NotificationItem` parser and `NotificationCategory` identifiers in `iOS/Sources/PushNotifications.swift`, add the same dependency to `AlondraiOSEntry`. Keep the app's localized `UNNotificationCategory` construction in the app target.

The package test uses APNs JSON shapes emitted by `crates/alondrad/src/push.rs`: a dynamic ask-user question, a fixed permission question, and a report. It checks the routing IDs and action IDs that must agree between daemon, extension, and app.
