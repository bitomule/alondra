import AlondraDomain
import ComposableArchitecture

private enum DomainWorkspaceClientKey: DependencyKey {
    /// The scene boundary installs the process-owned client. An unconfigured feature neither
    /// observes nor sends a command.
    static let liveValue = DomainWorkspaceClient.inert
    static let testValue = liveValue
}

extension DependencyValues {
    public var domainWorkspace: DomainWorkspaceClient {
        get { self[DomainWorkspaceClientKey.self] }
        set { self[DomainWorkspaceClientKey.self] = newValue }
    }
}
