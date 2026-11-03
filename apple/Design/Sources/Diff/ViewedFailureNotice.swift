import SwiftUI

/// Said when GitHub would not keep a «Viewed» box after half a minute of trying: the box went back.
public struct ViewedFailureNotice: View {
    let path: String
    let dismiss: () -> Void

    public init(path: String, dismiss: @escaping () -> Void) {
        self.path = path
        self.dismiss = dismiss
    }

    public var body: some View {
        Button(action: dismiss) {
            Label {
                Text("Couldn’t mark \(name) as viewed on GitHub. Try again.", tableName: "AlondraDesign", bundle: #bundle)
            } icon: {
                Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(Palette.dawn)
            }
            .font(.footnote.weight(.medium))
            .foregroundStyle(Palette.ink)
            .padding(.horizontal, 14).padding(.vertical, 8)
            #if os(iOS)
            .glassEffect(.regular, in: .capsule)
            #else
            .background(Palette.card, in: .capsule)
            .overlay { Capsule().strokeBorder(Palette.hairline) }
            .shadow(color: .black.opacity(0.15), radius: 8, y: 2)
            #endif
        }
        .buttonStyle(.plain)
        .accessibilityHint(Text("Dismisses this message", tableName: "AlondraDesign", bundle: #bundle))
    }

    private var name: String { path.split(separator: "/").last.map(String.init) ?? path }
}
