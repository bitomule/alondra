# Integration handoff for C

`AlondraContracts` and `AlondraDomain` compile independently. The existing `AlondraShared` target still owns identical public names until the central `apple/BUILD.bazel` integration. Do not import both modules into a file without removing or qualifying the old declaration.

## Exact replacement in Shared

1. Add `//apple/Contracts:AlondraContracts` and `//apple/Domain:AlondraDomain` to `AlondraShared` deps. Keep `Shared/Sources/**/*.swift` in its glob; no exclusions are needed for this small extraction because the old files also contain presentation and observable models.
2. In `Shared/Sources/WorkOptions.swift`, delete the declarations of `OptionChoice`, `OptionSpec`, `OptionsCatalog`, `ProjectList`, `NewRequestBody`, and `WorkOptions`. Add public typealiases to their counterparts in Contracts and Domain. Keep `WorkOptionsModel` and add Shared extensions for `OptionSpec.title`, `title(of:)`, `description(of:)`, `choices`, `accepts`, `ProjectList.filter`, and `WorkOptions.summary` where those presentation and compatibility APIs are still called. The selection rule now lives in Domain.
   In `WorkOptionsModel.choose(issue:)`, call `options.select(issue:inferredProject:)` with the result of `IssueSummary.project(of:among:)`, then load a new catalog only if the project changed. The old order assigns the issue before changing project; the Domain reset would erase it.
3. In `Shared/Sources/AlondraClient.swift`, delete only `PairingInvite`, `PairedDevice`, and `PushRegistration`; alias them to the Contracts types. Keep `PairingInvite.payload(fallbackURL:)` as a Shared extension because it creates a QR presentation payload. `AlondraError`, `CloseRefused`, and translated error messages remain in Shared until the error adapter is migrated.
4. Ensure `AlondraAPIClient` uses Contracts types. The old `AlondraClient` is the compatibility adapter and retains its public method signatures; aliases make its call sites source-compatible. Preserve `POST /v1/activity` as the app heartbeat that suppresses APNs, even though it has no response DTO.
5. Build `//apple:AlondraiOS`, `//apple:AlondraMac`, `//apple:AlondraNotificationService` and run `//apple:AlondraSharedTests` after aliases and adapters are integrated. If Swift's alias visibility from `import AlondraShared` needs explicit forwarding at a use site, add the relevant Contracts or Domain import there rather than duplicating the type.

## Deferred wire types

`Overview`, `Node`, `Question`, `Report`, `WorkRequest`, `WorkTask`, `PullRequest`, `IssueSummary`, `ScheduledTask` and their nested values remain in Shared during this slice. They mix server DTOs with localization, view logic, and cross-model rules. The API client can expose generic typed decode/send operations for these while Shared owns the decoding adapter. Move each family when its feature migrates and separate localized extensions at that point. Avoid a second copy of the full snapshot graph in Contracts.
