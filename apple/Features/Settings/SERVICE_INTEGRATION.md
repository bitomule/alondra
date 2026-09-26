# Approved-service settings

Use ServiceSettingsFeature / ServiceSettingsView for the approved account tab.
The former daemon URL editor, local reset and DevicePairing adapter are obsolete
for this path. Settings expose the fixed authenticated service, device identifier,
connection test, and a confirmed sign-out using the account lifecycle cleanup.

Scene hookups:

- iOS: ServiceSettingsScene(services: services, owner: owner, notifications: notifications).
- macOS: MacServiceSettingsScene(services: services, owner: account).
- Both entry targets need AlondraSettingsFeature. The module already includes the
  new shared view and reducer via its Sources glob. New app source files and the
  AppComposition settings adapter likewise use existing globs.

The iOS client observes the process notification runtime, not another scene's
Store. Drafts, conflict state and pending operations belong to each settings
feature. Save compares the persisted current quiet hours with the baseline before
writing through PushCenter. A conflicting draft is retained until explicitly
reloaded. Local settings save triggers the existing versioned encrypted push
registration; registration status distinguishes waiting, sending, registered and
failure. Permission changes refresh when the scene becomes active again.

Quiet hours are in the executor Mac's timezone. The view only converts minute
values for display; defaults, network and Keychain operations stay in services.
Mac had no APNs settings runtime in the prior app, so it shows account/service
settings without advertising an unimplemented Mac push channel.

Source EN and translated ES entries are supplied in both app catalogs, including
current iOS/macOS onboarding. Rendering, live permissions, multi-window conflict
behavior and actual registration require the orchestrator's app/MAV verification.
