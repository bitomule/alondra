import AlondraShared
import SwiftUI
import AlondraWorkspaceFeature
import ComposableArchitecture

extension EnvironmentValues {
    @Entry var searchActive = false
    /// Tasks with an action request waiting for the user: their dot takes the "your turn" colour.
    @Entry var yourTurnTasks: Set<DomainV2.TaskId> = []
}

private struct WorkspaceSettingsStoreKey: FocusedValueKey {
    typealias Value = StoreOf<ServiceRootFeature>
}

extension FocusedValues {
    var workspaceSettingsStore: StoreOf<ServiceRootFeature>? {
        get { self[WorkspaceSettingsStoreKey.self] }
        set { self[WorkspaceSettingsStoreKey.self] = newValue }
    }
}
