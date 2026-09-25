import AlondraSync
import ComposableArchitecture
import Foundation

public struct IOSRootSync: Sendable {
    public var subscribe: @Sendable () async -> AsyncStream<SyncState>

    public init(subscribe: @escaping @Sendable () async -> AsyncStream<SyncState>) {
        self.subscribe = subscribe
    }

    public static func live(session: SyncSession) -> Self {
        Self(subscribe: { await session.subscribe() })
    }
}

public struct IOSRootProjection: Sendable {
    public var project: @Sendable (SyncState) async -> IOSRootUpdate?

    public init(project: @escaping @Sendable (SyncState) async -> IOSRootUpdate?) {
        self.project = project
    }
}

public struct IOSRootLifecycle: Sendable {
    public var configure: @Sendable (String) async -> Void
    public var activatePush: @Sendable () async -> Void
    public var refreshAuthorization: @Sendable () async -> Void
    public var setForegroundActivity: @Sendable (UUID, Bool) async -> Void

    public init(
        configure: @escaping @Sendable (String) async -> Void,
        activatePush: @escaping @Sendable () async -> Void,
        refreshAuthorization: @escaping @Sendable () async -> Void,
        setForegroundActivity: @escaping @Sendable (UUID, Bool) async -> Void
    ) {
        self.configure = configure
        self.activatePush = activatePush
        self.refreshAuthorization = refreshAuthorization
        self.setForegroundActivity = setForegroundActivity
    }
}

private enum IOSRootSyncKey: DependencyKey {
    static let liveValue = IOSRootSync(subscribe: { AsyncStream { $0.finish() } })
    static let testValue = liveValue
}

private enum IOSRootProjectionKey: DependencyKey {
    static let liveValue = IOSRootProjection(project: { _ in
        preconditionFailure("iOS root projection must be installed at the app composition root")
    })
    static let testValue = liveValue
}

private enum IOSRootLifecycleKey: DependencyKey {
    static let liveValue = IOSRootLifecycle(
        configure: { _ in preconditionFailure("iOS root lifecycle must be installed at the app composition root") },
        activatePush: { preconditionFailure("iOS root lifecycle must be installed at the app composition root") },
        refreshAuthorization: { preconditionFailure("iOS root lifecycle must be installed at the app composition root") },
        setForegroundActivity: { _, _ in preconditionFailure("iOS root lifecycle must be installed at the app composition root") }
    )
    static let testValue = IOSRootLifecycle(
        configure: { _ in }, activatePush: {}, refreshAuthorization: {}, setForegroundActivity: { _, _ in }
    )
}

public extension DependencyValues {
    var iosRootSync: IOSRootSync {
        get { self[IOSRootSyncKey.self] }
        set { self[IOSRootSyncKey.self] = newValue }
    }

    var iosRootProjection: IOSRootProjection {
        get { self[IOSRootProjectionKey.self] }
        set { self[IOSRootProjectionKey.self] = newValue }
    }

    var iosRootLifecycle: IOSRootLifecycle {
        get { self[IOSRootLifecycleKey.self] }
        set { self[IOSRootLifecycleKey.self] = newValue }
    }
}
