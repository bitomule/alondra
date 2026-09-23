import AlondraShared
import SwiftUI

@main
struct AlondraApp: App {
    @AppStorage(ServerSetting.key) private var serverURL = ServerSetting.defaultURL
    @State private var fleet = FleetStore(client: AlondraClient(base: AlondraClient.defaultURL, device: "iphone"), animated: true, retryDelay: .seconds(3))
    @State private var pairing = DevicePairing()

    var body: some Scene {
        WindowGroup {
            RootView(serverURL: serverURL)
                .environment(fleet)
                .environment(pairing)
                .tint(Palette.dawn)
        }
    }
}

private struct RootView: View {
    let serverURL: String

    @Environment(FleetStore.self) private var fleet
    @Environment(DevicePairing.self) private var pairing
    @Environment(\.scenePhase) private var scenePhase
    @State private var tab = AppTab.triage

    enum AppTab: Hashable {
        case triage, fleet, new
    }

    private struct SyncKey: Hashable {
        let url: String
        let device: String?
        let active: Bool
    }

    var body: some View {
        TabView(selection: $tab) {
            Tab("Te espera", systemImage: "tray.full", value: .triage) {
                TriageView()
            }
            .badge(fleet.connection == .unpaired ? 0 : fleet.queue.count)
            Tab("Flota", systemImage: "circle.hexagongrid", value: .fleet) {
                FleetTab()
            }
            Tab("Nuevo", systemImage: "plus.bubble", value: .new) {
                NewRequestView()
            }
        }
        .tabBarMinimizeBehavior(.onScrollDown)
        .onChange(of: fleet.focusRequest) { tab = .triage }
        .overlay(alignment: .top) { NoticeToast() }
        .task(id: SyncKey(url: serverURL, device: pairing.deviceID, active: scenePhase != .background)) {
            guard scenePhase != .background else { return }
            if let client = AlondraClient(string: serverURL, device: "iphone", signer: pairing.signer) { fleet.use(client) }
            await fleet.run()
        }
    }
}

private struct NoticeToast: View {
    @Environment(FleetStore.self) private var fleet

    var body: some View {
        ZStack {
            if let notice = fleet.notice {
                Label(notice.text, systemImage: notice.failure ? "exclamationmark.triangle.fill" : "checkmark.circle.fill")
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(notice.failure ? Palette.rose : Palette.ink)
                    .padding(.horizontal, 18)
                    .padding(.vertical, 12)
                    .glassEffect(.regular, in: .capsule)
                    .padding(.horizontal, 24)
                    .transition(.move(edge: .top).combined(with: .opacity).combined(with: .scale(scale: 0.9)))
                    .onTapGesture { dismiss() }
                    .task(id: notice.id) {
                        try? await Task.sleep(for: .seconds(notice.failure ? 4 : 2.2))
                        dismiss()
                    }
                    .id(notice.id)
            }
        }
        .animation(.spring(duration: 0.45, bounce: 0.3), value: fleet.notice)
        .sensoryFeedback(trigger: fleet.notice) { _, new in
            new?.failure == true ? .error : nil
        }
    }

    private func dismiss() {
        fleet.notice = nil
    }
}
