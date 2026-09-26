import AlondraAppComposition
import AlondraCloudPairingFeature
import AlondraSettingsFeature
import ComposableArchitecture
import Foundation
import Observation
import UIKit

@MainActor
extension ServiceNotificationRuntime {
    func settingsClient(services: CloudAppServices, owner: CloudPairingOwner) -> ServiceSettingsClient {
        var client = ServiceSettingsClient(observe: { [weak self] in
            await MainActor.run { [weak self] in
                guard let self else { return AsyncStream { $0.finish() } }
                return SettingsObservation.stream(push: self.push)
            }
        }, save: { [weak self] next, expected in
            try await MainActor.run { [weak self] in
                guard let self, self.owns(services) else { throw CancellationError() }
                guard next.valid else { throw SettingsError.invalidTime }
                let current = ServiceQuietHours(enabled: self.push.quietHours.enabled, start: self.push.quietHours.start, end: self.push.quietHours.end)
                guard current == expected else { throw SettingsError.conflict }
                self.push.quietHours = QuietHours(enabled: next.enabled, start: next.start, end: next.end)
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
            guard await owner.services === services else { throw CancellationError() }
            try await owner.selectMac(choice: id)
        }
        return client
    }
}

@MainActor private final class SettingsObservation {
    let push: PushCenter
    let continuation: AsyncStream<ServiceNotificationSettings>.Continuation
    private var active = true
    init(push: PushCenter, continuation: AsyncStream<ServiceNotificationSettings>.Continuation) {
        self.push = push; self.continuation = continuation
    }
    static func stream(push: PushCenter) -> AsyncStream<ServiceNotificationSettings> {
        let (stream, continuation) = AsyncStream<ServiceNotificationSettings>.makeStream(bufferingPolicy: .bufferingNewest(1))
        let observation = SettingsObservation(push: push, continuation: continuation)
        continuation.onTermination = { _ in Task { @MainActor in observation.active = false; observation.continuation.onTermination = nil } }
        observation.track()
        Task { await push.refreshAuthorization() }
        return stream
    }
    private func track() {
        guard active else { return }
        let value = withObservationTracking {
            let permission: ServiceNotificationSettings.Permission = switch push.authorization {
            case .unknown: .unknown
            case .notDetermined: .notDetermined
            case .denied: .denied
            case .authorized: .authorized
            }
            let delivery: ServiceNotificationSettings.Delivery = switch push.registration {
            case .idle: .idle
            case .sending: .registering
            case .registered: .registered
            case .failed(let message): .failed(message)
            }
            return ServiceNotificationSettings(permission: permission, delivery: delivery,
                quietHours: ServiceQuietHours(enabled: self.push.quietHours.enabled, start: self.push.quietHours.start, end: self.push.quietHours.end))
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
