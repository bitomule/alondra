import AlondraFeatures
import ComposableArchitecture
import SwiftUI

extension EnvironmentValues {
    @Entry var searchActive = false
}

private struct WorkspaceSettingsStoreKey: FocusedValueKey {
    typealias Value = StoreOf<MacAppFeature>
}

extension FocusedValues {
    var workspaceSettingsStore: StoreOf<MacAppFeature>? {
        get { self[WorkspaceSettingsStoreKey.self] }
        set { self[WorkspaceSettingsStoreKey.self] = newValue }
    }
}
