import AlondraCloudPairingFeature
import AlondraSettingsFeature
import AlondraShared
import AlondraWorkspaceFeature
import ComposableArchitecture
import SwiftUI
import Foundation

@main
struct AlondraVisualHarness: App {
    @State private var fixture: VisualFixture
    init() { _fixture = State(initialValue: try! VisualFixture(environment: ProcessInfo.processInfo.environment)) }
    var body: some Scene {
        WindowGroup("Alondra visual fixture") {
            VisualScene(fixture: fixture)
                #if os(macOS)
                .frame(minWidth: 860, minHeight: 540)
                #endif
        }
        #if os(macOS)
        .defaultSize(width: 1240, height: 800)
        #endif
    }
}

private struct VisualScene: View {
    let fixture: VisualFixture
    @State private var restoring: StoreOf<ServiceRootFeature> = Store(initialState: ServiceRootFeature.State()) { EmptyReducer() }
    var body: some View {
        CloudWorkspaceGate(store: fixture.pairing, hasServices: fixture.ready) {
            #if os(iOS)
            ServiceRootView(store: fixture.workspace, accountContent: settings)
                .id(ObjectIdentifier(fixture.workspace)).tint(Palette.dawn)
            #else
            MacServiceWorkspace(store: fixture.workspace, accountContent: settings)
                .id(ObjectIdentifier(fixture.workspace))
            #endif
        } restoring: {
            #if os(iOS)
            ServiceRestoringWorkspace(store: restoring, pairing: fixture.pairing).tint(Palette.dawn)
            #else
            MacRestoringWorkspace(store: restoring, pairing: fixture.pairing)
            #endif
        } onboarding: {
            #if os(iOS)
            NavigationStack { CloudPairingScreen(store: fixture.pairing) }
            #else
            MacServicePairingScreen(store: fixture.pairing)
            #endif
        }
        .task { await fixture.pairing.send(.appeared).finish() }
        .task { await fixture.monitor() }
    }
    private var settings: some View {
        #if os(iOS)
        ServiceSettingsView(store: fixture.settings, ink: Palette.ink, success: Palette.sage, failure: Palette.rose) {
            CloudPairingScreen(store: $0)
        }
        #else
        ServiceSettingsView(store: fixture.settings) { MacServicePairingScreen(store: $0) }
        #endif
    }
}
