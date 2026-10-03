import SwiftUI

/// The buttons floating at the foot of a review screen, as one Liquid Glass group: every button
/// one line and one height, the main action prominent and tinted, the rest plain glass.
struct GlassActionBar<Content: View>: View {
    @ViewBuilder let content: Content

    var body: some View {
        GlassEffectContainer(spacing: 10) {
            VStack(spacing: 10) { content }
                .controlSize(.large)
                .buttonBorderShape(.capsule)
                .font(.headline)
                .lineLimit(1)
                .minimumScaleFactor(0.7)
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 8)
    }
}

/// A row of bar buttons side by side, stacked instead at accessibility text sizes so no label
/// has to shrink or cut.
struct GlassBarRow<Content: View>: View {
    @Environment(\.dynamicTypeSize) private var dynamicTypeSize
    @ViewBuilder let content: Content

    var body: some View {
        let layout = dynamicTypeSize.isAccessibilitySize
            ? AnyLayout(VStackLayout(spacing: 10)) : AnyLayout(HStackLayout(spacing: 10))
        layout { content }
    }
}

/// A bar button's label: it drops its symbol before its words when the row runs short, and
/// stretches so the buttons sharing a row split what the hugging ones leave.
struct GlassBarLabel: View {
    let title: String
    let systemImage: String?
    let fills: Bool
    @Environment(\.dynamicTypeSize) private var dynamicTypeSize

    init(_ title: String, systemImage: String? = nil, fills: Bool = true) {
        self.title = title
        self.systemImage = systemImage
        self.fills = fills
    }

    var body: some View {
        ViewThatFits(in: .horizontal) {
            if let systemImage { Label(title, systemImage: systemImage) }
            Text(title)
        }
        .frame(maxWidth: fills || dynamicTypeSize.isAccessibilitySize ? .infinity : nil)
    }
}
