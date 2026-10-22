import AlondraContract
import AlondraFeatures
import SwiftUI

/// The buttons in a column by role, and what does not fit behind "More…".
struct MacActionColumn: View {
    let buttons: MacCardButtons
    @Binding var moreOpen: Bool
    var onAction: (AlondraContract.Action) -> Void

    var body: some View {
        VStack(spacing: 8) {
            ForEach(Array(buttons.visible.enumerated()), id: \.offset) { _, action in
                VStack(spacing: 4) {
                    Button { onAction(action) } label: { MacCardButtonLabel(action: action) }
                        .buttonStyle(MacCardButtonStyle(role: action.paintedRole))
                        .disabled(!action.enabled)
                    DisabledReason(action).font(.caption).foregroundStyle(.secondary).multilineTextAlignment(.center)
                }
            }
            if !buttons.more.isEmpty { moreButton }
        }
    }

    private var moreButton: some View {
        Button { moreOpen.toggle() } label: {
            HStack(spacing: 6) {
                Text("More…").font(.subheadline.weight(.medium))
                Text("\(buttons.more.count)").font(.caption.monospacedDigit()).foregroundStyle(.tertiary)
                Spacer(minLength: 0)
                MacKeyHint(key: "m")
            }
            .padding(.horizontal, 12).frame(maxWidth: .infinity, minHeight: 32, alignment: .leading)
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .foregroundStyle(.secondary)
        .popover(isPresented: $moreOpen, arrowEdge: .bottom) {
            VStack(alignment: .leading, spacing: 2) {
                ForEach(Array(buttons.more.enumerated()), id: \.offset) { _, action in
                    Button {
                        moreOpen = false
                        onAction(action)
                    } label: {
                        HStack(spacing: 16) {
                            Text(action.label)
                            Spacer(minLength: 12)
                            if let key = action.key { MacKeyHint(key: key) }
                        }
                        .padding(.horizontal, 10).padding(.vertical, 6).contentShape(.rect)
                    }
                    .buttonStyle(.plain)
                    .disabled(!action.enabled)
                }
            }
            .padding(6).frame(minWidth: 220)
        }
    }
}
