import SnapshotTesting
import SwiftUI
import UIKit
import XCTest

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
            .pullback { view in MainActor.assumeIsolated {
                drawInKeyWindow(view, size: size, traits: traits)
            } }
    }
}

/// Draws the view in the test host's key window the way swift-snapshot-testing does: placed and laid
/// out on screen, then moved off it so no safe area applies, then captured. `placed` runs once the
/// view is laid out in the window, and `drawn` once it has been moved off it, right before the capture.
@MainActor
func drawInKeyWindow(
    _ view: AnyView, size fixedSize: CGSize?, traits: UITraitCollection,
    placed: () -> Void = {},
    drawn: (UIView) -> Void = { _ in }
) -> UIImage {
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
    stopAnimationClock(host.view.layer)
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
    let placedContent = contentSafeAreas(in: host.view)
    host.view.frame.origin = CGPoint(x: 10_000, y: 10_000)
    keepContent(placedContent, in: host)
    advanceAnimationClock(host.view.layer)
    drawn(host.view)

    defer {
        host.view.layer.speed = 1
        host.view.layer.timeOffset = 0
        host.view.frame = initialFrame
        root.beginAppearanceTransition(false, animated: false)
        host.willMove(toParent: nil)
        host.view.removeFromSuperview()
        host.removeFromParent()
        host.didMove(toParent: nil)
        root.endAppearanceTransition()
        window.rootViewController = nil
    }

    let renderer = UIGraphicsImageRenderer(bounds: host.view.bounds, format: UIGraphicsImageRendererFormat(for: traits))
    // The first compositions after boot quantize shadows and native button labels a few levels off
    // their baseline. Whichever screen is drawn first would carry that, so every capture completes
    // that pass on its own host before it is drawn, without advancing time or recreating it.
    _ = renderer.image { _ in
        host.view.drawHierarchy(in: host.view.bounds, afterScreenUpdates: true)
    }
    return renderer.image { _ in
        host.view.drawHierarchy(in: host.view.bounds, afterScreenUpdates: true)
    }
}

/// What a navigation controller gave its content as top safe area while the screen was on the screen.
private typealias PlacedContent = (controller: UINavigationController, inset: CGFloat)

@MainActor
private func contentSafeAreas(in view: UIView) -> [PlacedContent] {
    navigationControllers(in: view).compactMap { controller in
        controller.topViewController.map { (controller, $0.view.safeAreaInsets.top) }
    }
}

/// Moving the host off the screen takes its safe area away, and the navigation bar moves up with it
/// as soon as the bars are laid out. The content under the bar is told so one turn of the run loop
/// later: the controller then hands it the new bottom edge of the bar, and a screen laid out with the
/// inset it had on the screen is laid out again with one ~50 pt smaller. The baselines were drawn
/// before that turn, and which side of the capture it falls on depends on the machine. Adding the
/// difference to the content's own safe area makes the turn hand it the inset it already has, so
/// the screen is drawn as it was placed whenever the turn comes, and the wait below makes it come
/// before the capture rather than during it.
@MainActor
private func keepContent(_ placed: [PlacedContent], in host: UIViewController) {
    host.view.layoutIfNeeded()
    var held: [(content: UIViewController, inset: CGFloat)] = []
    for (controller, inset) in placed {
        guard let content = controller.topViewController else { continue }
        let difference = inset - controller.navigationBar.frame.maxY
        guard abs(difference) > 0.5 else { continue }
        content.additionalSafeAreaInsets.top += difference
        held.append((content, inset))
    }
    host.view.layoutIfNeeded()
    for _ in 0..<400 where held.contains(where: { abs($0.content.view.safeAreaInsets.top - $0.inset) > 0.5 }) {
        RunLoop.main.run(mode: .default, before: Date(timeIntervalSinceNow: 0.005))
    }
    if let stale = held.first(where: { abs($0.content.view.safeAreaInsets.top - $0.inset) > 0.5 }) {
        XCTFail("the navigation controller never handed its content the top safe area of \(stale.inset) it was placed with")
    }
}

private func navigationControllers(in view: UIView) -> [UINavigationController] {
    var found: [UINavigationController] = []
    for bar in navigationBars(in: view) {
        var responder: UIResponder? = bar
        while let current = responder, !(current is UINavigationController) { responder = current.next }
        if let controller = responder as? UINavigationController, !found.contains(controller) { found.append(controller) }
    }
    return found
}

/// Core Animation's clock stands still under `layer`: an animation added from here on starts at the
/// same instant of it whenever it is added, so how long the machine took to get here changes nothing.
@MainActor
func stopAnimationClock(_ layer: CALayer) {
    layer.speed = 0
    layer.timeOffset = 0
}

/// Commits what the stopped clock holds, then moves it on far enough that every animation that ends
/// has ended and every repeating one, a spinner or a pulsing symbol, stands at the same point of its
/// cycle on every run.
@MainActor
func advanceAnimationClock(_ layer: CALayer) {
    CATransaction.flush()
    layer.timeOffset = 60
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
