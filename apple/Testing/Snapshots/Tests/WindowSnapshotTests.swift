import AlondraContract
import AlondraFeatures
import AlondraiOSUI
import ComposableArchitecture
import ObjectiveC
import SnapshotTesting
import SwiftUI
import UIKit
import XCTest

/// A window capture whose navigation bar glass never got the second configuration UIKit gives it while
/// placing the bar, as happens on a loaded machine, still draws the bar the way its baseline shows.
@MainActor
final class WindowSnapshotTests: XCTestCase {
    func test_aBarWhoseGlassMissedItsSecondConfigurationDrawsSettled() {
        let client = MacSettingsClient(load: { throw CancellationError() }, save: { _ in throw CancellationError() },
                                       folders: { _ in throw CancellationError() })
        let listing = FolderListing(path: "/Users/david/Work", parent: "/Users/david", folders: [
            .init(name: "alondra", path: "/Users/david/Work/alondra", repo: true, repos: 0),
            .init(name: "clients", path: "/Users/david/Work/clients", repo: false, repos: 7),
            .init(name: "notes", path: "/Users/david/Work/notes", repo: false, repos: 0),
        ])
        let store = Store(initialState: FolderBrowserFeature.State(client: client, listing: listing)) {
            EmptyReducer<FolderBrowserFeature.State, FolderBrowserFeature.Action>()
        }
        let traits = UITraitCollection(traitsFrom: [UITraitCollection(userInterfaceStyle: .dark), UITraitCollection(displayScale: 1)])

        let blocker = GlassRelayoutBlocker()
        defer { blocker.stop() }
        let image = drawInKeyWindow(
            screen(.dark) { ScreenFrame(height: 852) { NavigationStack { FolderBrowserView(store: store) } } },
            size: nil,
            traits: traits,
            placed: { blocker.stop() }
        )

        XCTAssertGreaterThan(blocker.blocked, 0, "the glass was never laid out a second time, so nothing was reproduced")
        assertAppSnapshot(
            of: image.pngData().flatMap { UIImage(data: $0, scale: image.scale) } ?? image,
            as: .image(precision: 1, perceptualPrecision: 0.98, scale: traits.displayScale),
            named: "dark"
        )
    }
}

/// Lets each of the navigation bar's glass views lay out once and drops every later layout until
/// `stop`, the way a bar that UIKit never laid out again ends up.
@MainActor
private final class GlassRelayoutBlocker {
    private static let selector = NSSelectorFromString("layoutSublayersOfLayer:")
    private let method: Method
    private let original: IMP
    private var laidOut: Set<ObjectIdentifier> = []
    private(set) var blocked = 0
    private var active = true

    init() {
        method = class_getInstanceMethod(UIView.self, Self.selector)!
        original = method_getImplementation(method)
        typealias Layout = @convention(c) (UIView, Selector, CALayer) -> Void
        let layout = unsafeBitCast(original, to: Layout.self)
        let replacement: @convention(block) (UIView, CALayer) -> Void = { [weak self] view, layer in
            let skip = MainActor.assumeIsolated { self?.skips(view) ?? false }
            if !skip { layout(view, Self.selector, layer) }
        }
        method_setImplementation(method, imp_implementationWithBlock(replacement))
    }

    func stop() {
        guard active else { return }
        active = false
        method_setImplementation(method, original)
    }

    private func skips(_ view: UIView) -> Bool {
        guard active, Self.isGlass(view) else { return false }
        if laidOut.insert(ObjectIdentifier(view)).inserted { return false }
        blocked += 1
        return true
    }

    private static func isGlass(_ view: UIView) -> Bool {
        let name = NSStringFromClass(type(of: view))
        return ["SDFBackdropView", "MaterialDefinitionView", "GlassGroup"].contains { name.contains($0) }
    }
}
