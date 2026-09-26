import SwiftUI
import AlondraWorkspaceFeature
import ComposableArchitecture

extension EnvironmentValues {
    @Entry var searchActive = false
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
