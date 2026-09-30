import AlondraViewsWorkspaceFeature
import ComposableArchitecture
import SwiftUI

extension EnvironmentValues {
    @Entry var searchActive = false
}

private struct WorkspaceSettingsStoreKey: FocusedValueKey {
    typealias Value = StoreOf<ViewsServiceRootFeature>
}

extension FocusedValues {
    var workspaceSettingsStore: StoreOf<ViewsServiceRootFeature>? {
        get { self[WorkspaceSettingsStoreKey.self] }
        set { self[WorkspaceSettingsStoreKey.self] = newValue }
    }
}
