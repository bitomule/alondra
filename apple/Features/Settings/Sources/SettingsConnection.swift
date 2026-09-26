import ComposableArchitecture
import Foundation

/// The app supplies the transport and persistence at composition. Nothing in Settings owns a signer.
public struct SettingsConnection: Sendable {
    public var check: @Sendable (URL) async throws -> Void
    public var save: @Sendable (URL, String) -> SettingsSaveResult

    public init(
        check: @escaping @Sendable (URL) async throws -> Void,
        save: @escaping @Sendable (URL, String) -> SettingsSaveResult
    ) {
        self.check = check
        self.save = save
    }
}

private enum SettingsConnectionKey: DependencyKey {
    static let liveValue = SettingsConnection(
        check: { _ in preconditionFailure("Install SettingsConnection at the iOS composition boundary") },
        save: { _, _ in preconditionFailure("Install SettingsConnection at the iOS composition boundary") }
    )
    static let testValue = liveValue
}

public enum SettingsSaveResult: Equatable, Sendable {
    case saved
    case conflict(currentAddress: String)
}

public extension DependencyValues {
    var settingsConnection: SettingsConnection {
        get { self[SettingsConnectionKey.self] }
        set { self[SettingsConnectionKey.self] = newValue }
    }
}

/// Match the API client's accepted server addresses before starting any I/O.
public enum SettingsAddress {
    public static func parse(_ address: String) -> URL? {
        let trimmed = address.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let url = URL(string: trimmed),
              let scheme = url.scheme,
              scheme == "http" || scheme == "https",
              url.host() != nil else { return nil }
        return url
    }
}
