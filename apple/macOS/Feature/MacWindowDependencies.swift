import AlondraSync
import ComposableArchitecture

public struct MacWindowSync: Sendable {
    public var subscribe: @Sendable () async -> AsyncStream<SyncState>

    public init(subscribe: @escaping @Sendable () async -> AsyncStream<SyncState>) {
        self.subscribe = subscribe
    }

    public static func live(session: SyncSession) -> Self {
        Self(subscribe: { await session.subscribe() })
    }
}

public struct MacWindowProjection: Sendable {
    public var project: @Sendable (SyncState) async -> MacWindowUpdate?

    public init(project: @escaping @Sendable (SyncState) async -> MacWindowUpdate?) {
        self.project = project
    }
}

private enum MacWindowSyncKey: DependencyKey {
    static let liveValue = MacWindowSync(subscribe: {
        preconditionFailure("Mac window sync must be installed at the app composition root")
    })
    static let testValue = liveValue
}

private enum MacWindowProjectionKey: DependencyKey {
    static let liveValue = MacWindowProjection(project: { state in
        preconditionFailure("Mac window projection must be installed at the app composition root")
    })
    static let testValue = liveValue
}

public extension DependencyValues {
    var macWindowSync: MacWindowSync {
        get { self[MacWindowSyncKey.self] }
        set { self[MacWindowSyncKey.self] = newValue }
    }

    var macWindowProjection: MacWindowProjection {
        get { self[MacWindowProjectionKey.self] }
        set { self[MacWindowProjectionKey.self] = newValue }
    }
}
