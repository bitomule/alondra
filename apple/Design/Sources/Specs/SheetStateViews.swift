import SwiftUI

/// What a screen of specs shows while its sheets load: two sheets in outline, so the page does not
/// jump when they arrive.
public struct SheetLoadingView: View {
    public init() {}

    public var body: some View {
        VStack(spacing: 12) {
            ForEach(0..<2, id: \.self) { _ in
                SheetCard(raised: true) {
                    VStack(alignment: .leading, spacing: 10) {
                        Text(verbatim: "Partial refunds").font(.display(.title3, weight: .bold))
                        Text(verbatim: "A customer gets back any amount up to what is left of the order.").font(.subheadline)
                        RoundedRectangle(cornerRadius: 12).fill(Palette.hairline).frame(height: 64)
                    }
                }
            }
        }
        .redacted(reason: .placeholder)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text("Loading specs", tableName: "AlondraDesign", bundle: #bundle))
        .accessibilityAddTraits(.updatesFrequently)
    }
}

/// A screen of specs that could not be read from the Mac, with the way to try again.
public struct SheetFailureView: View {
    let message: String
    let retry: () -> Void

    public init(message: String, retry: @escaping () -> Void) { self.message = message; self.retry = retry }

    public var body: some View {
        VStack(spacing: 14) {
            Image(systemName: "exclamationmark.triangle.fill").font(.largeTitle).foregroundStyle(Palette.gold).accessibilityHidden(true)
            Text(message).font(.subheadline).foregroundStyle(Palette.ink).multilineTextAlignment(.center)
            Button { retry() } label: { Label { Text("Try Again", tableName: "AlondraDesign", bundle: #bundle) } icon: { Image(systemName: "arrow.clockwise") } }
                .buttonStyle(.borderedProminent).tint(Palette.dawn)
        }
        .padding(24)
        .frame(maxWidth: .infinity)
        .accessibilityElement(children: .contain)
    }
}
