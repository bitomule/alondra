import AlondraContract
import SwiftUI

/// Why a button cannot be pressed: the server's own words, or that this app is older than the
/// daemon that sent it.
public struct DisabledReason: View {
    let action: AlondraContract.Action

    public init(_ action: AlondraContract.Action) { self.action = action }

    public var body: some View {
        if action.needsNewerApp {
            Text("Update Alondra to use this")
        } else if !action.enabled, let reason = action.disabledReason {
            Text(reason)
        }
    }
}

/// An item of a kind a newer daemon sent and this app cannot draw.
public struct UnsupportedItemView: View {
    public init() {}

    public var body: some View {
        Label("Update Alondra to see this", systemImage: "arrow.down.circle")
            .font(.footnote)
            .foregroundStyle(.secondary)
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(ViewPalette.slate.opacity(0.10), in: .rect(cornerRadius: 12))
    }
}
