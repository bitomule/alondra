import SnapshotTesting
import SwiftUI
import UIKit

extension Snapshotting where Value == AnyView, Format == UIImage {
    /// `.image(drawHierarchyInKeyWindow: true)` with the navigation bar's Liquid Glass in its final
    /// configuration. `size` is the frame for `.fixed`, and nil sizes the view to fit, as `.sizeThatFits` does.
    @MainActor
    static func windowImage(
        precision: Float = 1,
        perceptualPrecision: Float = 1,
        size: CGSize? = nil,
        traits: UITraitCollection
    ) -> Self {
        Snapshotting<UIImage, UIImage>.image(precision: precision, perceptualPrecision: perceptualPrecision, scale: traits.displayScale)
            .pullback { view in MainActor.assumeIsolated { drawInKeyWindow(view, size: size, traits: traits) } }
    }
}

/// Draws the view in the test host's key window the way swift-snapshot-testing does: placed and laid
/// out on screen, then moved off it so no safe area applies, then drawn once. `placed` runs once the
/// view is laid out in the window.
@MainActor
func drawInKeyWindow(_ view: AnyView, size fixedSize: CGSize?, traits: UITraitCollection, placed: () -> Void = {}) -> UIImage {
    let host = UIHostingController(rootView: view)
    let size: CGSize
    if let fixedSize {
        size = fixedSize
    } else {
        host.safeAreaRegions = []
        size = host.sizeThatFits(in: .zero)
    }
    let initialFrame = host.view.frame
    host.view.frame.size = size
    guard let window = UIApplication.shared.connectedScenes
        .compactMap({ $0 as? UIWindowScene })
        .flatMap(\.windows)
        .first(where: \.isKeyWindow)
    else {
        fatalError("a window capture needs the test host's key window")
    }
    window.frame.size = size

    let root = UIViewController()
    root.view.backgroundColor = .clear
    root.view.frame = window.frame
    root.preferredContentSize = root.view.frame.size
    host.view.frame = root.view.frame
    root.view.addSubview(host.view)
    host.view.autoresizingMask = [.flexibleWidth, .flexibleHeight]
    root.addChild(host)
    root.setOverrideTraitCollection(traits, forChild: host)
    host.didMove(toParent: root)
    window.rootViewController = root
    root.beginAppearanceTransition(true, animated: false)
    root.endAppearanceTransition()
    root.view.setNeedsLayout()
    root.view.layoutIfNeeded()
    host.view.setNeedsLayout()
    host.view.layoutIfNeeded()
    placed()
    settleGlass(host)
    host.view.frame.origin = CGPoint(x: 10_000, y: 10_000)

    defer {
        host.view.frame = initialFrame
        root.beginAppearanceTransition(false, animated: false)
        host.willMove(toParent: nil)
        host.view.removeFromSuperview()
        host.removeFromParent()
        host.didMove(toParent: nil)
        root.endAppearanceTransition()
        window.rootViewController = nil
    }

    return UIGraphicsImageRenderer(bounds: host.view.bounds, format: UIGraphicsImageRendererFormat(for: traits)).image { _ in
        host.view.drawHierarchy(in: host.view.bounds, afterScreenUpdates: true)
    }
}

/// UIKit configures a navigation bar's glass twice while placing it: once provisionally as the bar is
/// positioned, and again when the traits it was placed with reach the glass. On a loaded machine that
/// second pass sometimes never comes, and the bar's buttons draw in another tone, with nothing left
/// that would lay the glass out again. A round trip of each bar through the other appearance makes
/// UIKit configure its glass once more with the final traits. Only the bars take the trip: text fields
/// elsewhere on the screen do not draw the same after one. It runs while the view is still on screen:
/// laid out off it, the bars lose their safe area.
@MainActor
private func settleGlass(_ host: UIViewController) {
    for bar in navigationBars(in: host.view) {
        bar.traitOverrides.userInterfaceStyle = bar.traitCollection.userInterfaceStyle == .dark ? .light : .dark
        bar.layoutIfNeeded()
        bar.traitOverrides.remove(UITraitUserInterfaceStyle.self)
        bar.layoutIfNeeded()
    }
}

private func navigationBars(in view: UIView) -> [UINavigationBar] {
    (view as? UINavigationBar).map { [$0] } ?? view.subviews.flatMap(navigationBars)
}
