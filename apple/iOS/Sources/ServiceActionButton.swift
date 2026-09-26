import SwiftUI

struct ServiceActionButton: View {
    let title: String
    let symbol: String
    let tint: Color
    let enabled: Bool
    var isWorking = false
    var role: ButtonRole? = nil
    let action: () -> Void

    var body: some View {
        Button(role: role, action: action) {
            HStack(spacing: 8) {
                if isWorking { ProgressView().controlSize(.small).accessibilityHidden(true) }
                Label {
                    Text(title)
                } icon: {
                    Image(systemName: symbol)
                        .foregroundStyle(enabled ? AnyShapeStyle(tint) : AnyShapeStyle(.tertiary))
                }
            }
            .font(.body.weight(.semibold))
            .padding(.horizontal, 14)
            .padding(.vertical, 12)
            .frame(maxWidth: .infinity, minHeight: 50)
            .foregroundStyle(enabled ? AnyShapeStyle(Palette.ink) : AnyShapeStyle(.tertiary))
            .background(enabled ? AnyShapeStyle(tint.opacity(0.12)) : AnyShapeStyle(Palette.hairline), in: .rect(cornerRadius: 18, style: .continuous))
            .contentShape(.rect)
        }
        .buttonStyle(PressStyle())
        .disabled(!enabled)
        .accessibilityValue(isWorking ? String(localized: "In progress") : "")
    }
}

