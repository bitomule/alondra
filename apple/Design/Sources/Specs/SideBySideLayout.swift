import SwiftUI

/// Two views side by side when the width allows, one under the other when it does not. The same
/// width decides the size it reports and where it places them, so the height is never measured for
/// one arrangement and drawn in the other.
struct SideBySideLayout: Layout {
    var leadingMinimum: CGFloat = 380
    var trailing: CGFloat = 260
    var spacing: CGFloat = 18

    private func isWide(_ width: CGFloat?) -> Bool {
        guard let width, width.isFinite else { return false }
        return width >= leadingMinimum + spacing + trailing
    }

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        guard subviews.count == 2 else { return .zero }
        if isWide(proposal.width), let width = proposal.width {
            let leading = subviews[0].sizeThatFits(ProposedViewSize(width: width - spacing - trailing, height: nil))
            let side = subviews[1].sizeThatFits(ProposedViewSize(width: trailing, height: nil))
            return CGSize(width: width, height: max(leading.height, side.height))
        }
        let width = proposal.width
        let first = subviews[0].sizeThatFits(ProposedViewSize(width: width, height: nil))
        let second = subviews[1].sizeThatFits(ProposedViewSize(width: width, height: nil))
        return CGSize(width: width ?? max(first.width, second.width), height: first.height + spacing + second.height)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        guard subviews.count == 2 else { return }
        if isWide(bounds.width) {
            let leadingWidth = bounds.width - spacing - trailing
            subviews[0].place(at: bounds.origin, proposal: ProposedViewSize(width: leadingWidth, height: nil))
            subviews[1].place(at: CGPoint(x: bounds.minX + leadingWidth + spacing, y: bounds.minY), proposal: ProposedViewSize(width: trailing, height: nil))
        } else {
            let first = subviews[0].sizeThatFits(ProposedViewSize(width: bounds.width, height: nil))
            subviews[0].place(at: bounds.origin, proposal: ProposedViewSize(width: bounds.width, height: first.height))
            subviews[1].place(at: CGPoint(x: bounds.minX, y: bounds.minY + first.height + spacing), proposal: ProposedViewSize(width: bounds.width, height: nil))
        }
    }
}
