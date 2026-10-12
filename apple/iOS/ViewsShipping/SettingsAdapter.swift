import AlondraDesign
import AlondraAccount
import AlondraFeatures
import ComposableArchitecture
import Foundation
import Observation
import UIKit

@MainActor
extension NotificationRuntime {
    func settingsClient(services: Connection, owner: AccountSession) -> SettingsClient {
        var client = SettingsClient(observe: { [weak self] in
            await MainActor.run { [weak self] in
                guard let self else { return AsyncStream { $0.finish() } }
                return SettingsObservation.stream(runtime: self)
            }
        }, save: { [weak self] next, expected in
            try await MainActor.run { [weak self] in
                guard let self, self.owns(services) else { throw CancellationError() }
                guard next.valid else { throw SettingsError.invalidTime }
                let current = QuietHours(enabled: self.push.quietHours.enabled, start: self.push.quietHours.start, end: self.push.quietHours.end)
                guard current == expected else { throw SettingsError.conflict }
                self.push.quietHours = StoredQuietHours(enabled: next.enabled, start: next.start, end: next.end)
            }
        }, refreshPermission: { [weak self] in await self?.push.refreshAuthorization() }, requestPermission: { [weak self] in await self?.push.activate() },
        openSystemSettings: {
            await MainActor.run {
                guard let url = URL(string: UIApplication.openNotificationSettingsURLString) else { return }
                UIApplication.shared.open(url)
            }
        }, checkConnection: {
            try await services.checkSettingsConnection()
        }, logout: { [weak self] in
            await MainActor.run { [weak self] in
                guard let self, self.owns(services) else { return }
                _ = owner.store.send(.logout)
            }
        })
        client.selectMac = { id in
            try await owner.selectMac(choice: id, expectedSession: services)
        }
        client.macStatuses = { await owner.macStatuses() }
        client.forgetMac = { id in try await owner.forgetMac(choice: id) }
        client.macSettings = { id in await owner.session(choice: id)?.macSettingsClient() }
        client.problem = { services.connectionProblem(for: $0) }
        return client
    }
}

@MainActor private final class SettingsObservation {
    let runtime: NotificationRuntime
    let continuation: AsyncStream<NotificationSettings>.Continuation
    private var active = true
    private var push: PushCenter { runtime.push }
    init(runtime: NotificationRuntime, continuation: AsyncStream<NotificationSettings>.Continuation) {
        self.runtime = runtime; self.continuation = continuation
    }
    static func stream(runtime: NotificationRuntime) -> AsyncStream<NotificationSettings> {
        let push = runtime.push
        let (stream, continuation) = AsyncStream<NotificationSettings>.makeStream(bufferingPolicy: .bufferingNewest(1))
        let observation = SettingsObservation(runtime: runtime, continuation: continuation)
        continuation.onTermination = { _ in Task { @MainActor in observation.active = false; observation.continuation.onTermination = nil } }
        observation.track()
        Task { await push.refreshAuthorization() }
        return stream
    }
    private func track() {
        guard active else { return }
        let value = withObservationTracking {
            let permission: NotificationSettings.Permission = switch push.authorization {
            case .unknown: .unknown
            case .notDetermined: .notDetermined
            case .denied: .denied
            case .authorized: .authorized
            }
            var delivery: NotificationSettings.Delivery = switch push.registration {
            case .idle: .idle
            case .sending: .registering
            case .registered: .registered
            case .failed(let message): .failed(message)
            }
            if let error = runtime.error { delivery = .failed(error) }
            return NotificationSettings(permission: permission, delivery: delivery,
                quietHours: QuietHours(enabled: self.push.quietHours.enabled, start: self.push.quietHours.start, end: self.push.quietHours.end))
        } onChange: { [weak self] in
            Task { @MainActor [weak self] in self?.track() }
        }
        continuation.yield(value)
    }
}

private enum SettingsError: LocalizedError {
    case conflict, invalidTime
    var errorDescription: String? {
        switch self {
        case .conflict: String(localized: "Notification settings changed in another window. Your draft has been kept.")
        case .invalidTime: String(localized: "Choose a valid start and end time.")
        }
    }
}
