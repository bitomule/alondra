import AlondraContract
import AlondraDesign
import AlondraFeatures
import AlondraiOSUI
import ComposableArchitecture
import SnapshotTesting
import SwiftUI
import UIKit
import XCTest

/// «This Mac's settings» on the iPhone, its folder browser, and the first screen before any Mac shows up.
@MainActor
final class ThisMacSettingsSnapshotTests: XCTestCase {
    private let client = MacSettingsClient(load: { throw CancellationError() }, save: { _ in throw CancellationError() },
                                           folders: { _ in throw CancellationError() })

    private func settings(relay: Bool = true, error: String? = nil) -> StoreOf<MacSettingsFeature> {
        var state = MacSettingsFeature.State(id: UUID(), macName: "MacBook Pro de David", client: client, settings: MacSettings(
            projectsRoot: "/Users/david/Projects",
            candidates: [.init(path: "/Users/david/Projects", repos: 63), .init(path: "/Users/david/Developer", repos: 4),
                         .init(path: "/Users/david/Backups", repos: 1)],
            relay: .init(on: relay)))
        state.error = error
        return Store(initialState: state) { EmptyReducer<MacSettingsFeature.State, MacSettingsFeature.Action>() }
    }

    private func screen(_ store: StoreOf<MacSettingsFeature>) -> some View {
        NavigationStack {
            ThisMacSettingsView(store: store)
                .scrollContentBackground(.hidden)
                .background(DawnBackground())
                .navigationBarTitleDisplayMode(.inline)
        }
    }

    func test_thisMacSettings() {
        let store = settings()
        assertScreen([.light, .dark], inWindow: true) { ScreenFrame(height: 852) { screen(store) } }
    }

    func test_thisMacSettings_spanish() {
        let store = settings()
        assertScreen([.light], language: .spanish, inWindow: true) { ScreenFrame(height: 852) { screen(store) } }
    }

    func test_thisMacSettings_relayOffAndARefusedFolder() {
        let store = settings(relay: false, error: "~/notes is a file, not a folder.")
        assertScreen([.light], inWindow: true) { ScreenFrame(height: 852) { screen(store) } }
    }

    func test_thisMacSettings_relayOffAndARefusedFolder_spanish() {
        let store = settings(relay: false, error: "~/notas es un archivo, no una carpeta.")
        assertScreen([.light], language: .spanish, inWindow: true) { ScreenFrame(height: 852) { screen(store) } }
    }

    private func browser() -> StoreOf<FolderBrowserFeature> {
        let listing = FolderListing(path: "/Users/david/Work", parent: "/Users/david", folders: [
            .init(name: "alondra", path: "/Users/david/Work/alondra", repo: true, repos: 0),
            .init(name: "clients", path: "/Users/david/Work/clients", repo: false, repos: 7),
            .init(name: "notes", path: "/Users/david/Work/notes", repo: false, repos: 0),
        ])
        return Store(initialState: FolderBrowserFeature.State(client: client, listing: listing)) {
            EmptyReducer<FolderBrowserFeature.State, FolderBrowserFeature.Action>()
        }
    }

    func test_folderBrowser() {
        let store = browser()
        assertScreen([.light, .dark], inWindow: true) { ScreenFrame(height: 852) { NavigationStack { FolderBrowserView(store: store) } } }
    }

    func test_folderBrowser_spanish() {
        let store = browser()
        assertScreen([.light], language: .spanish, inWindow: true) {
            ScreenFrame(height: 852) { NavigationStack { FolderBrowserView(store: store) } }
        }
    }

    private func lookingForMacs() -> StoreOf<SignInFeature> {
        var state = SignInFeature.State(additionalMac: false)
        state.phase = .signedOut
        return Store(initialState: state) { EmptyReducer<SignInFeature.State, SignInFeature.Action>() }
    }

    func test_findMac_noMacYet() {
        let store = lookingForMacs()
        assertScreen([.light, .dark], inWindow: true, animatedPixels: true) { ScreenFrame(height: 852) { SignInView(store: store, demo: {}) } }
    }

    func test_findMac_noMacYet_spanish() {
        let store = lookingForMacs()
        assertScreen([.light], language: .spanish, inWindow: true, animatedPixels: true) { ScreenFrame(height: 852) { SignInView(store: store, demo: {}) } }
    }
}
