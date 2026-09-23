import AlondraShared
import SwiftUI
import UIKit
import UserNotifications

@main
struct AlondraApp: App {
    @UIApplicationDelegateAdaptor private var delegate: AppDelegate
    @AppStorage(ServerSetting.key) private var serverURL = ServerSetting.defaultURL

    var body: some Scene {
        WindowGroup {
            RootView(serverURL: serverURL)
                .environment(delegate.fleet)
                .environment(delegate.pairing)
                .environment(delegate.push)
                .tint(Palette.dawn)
        }
    }
}

final class AppDelegate: NSObject, UIApplicationDelegate {
    let fleet = FleetStore(client: AlondraClient(base: AlondraClient.defaultURL, device: "iphone"), animated: true, retryDelay: .seconds(3))
    let pairing = DevicePairing()
    lazy var push = PushCenter(fleet: fleet, pairing: pairing)
    private lazy var responder = NotificationResponder(push: push)

    func application(_ application: UIApplication, didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil) -> Bool {
        let center = UNUserNotificationCenter.current()
        center.delegate = responder
        Task {
            let questions = await center.notificationCategories().filter { $0.identifier.hasPrefix(NotificationCategory.questionPrefix) }
            center.setNotificationCategories(NotificationCategory.all.union(questions))
        }
        return true
    }

    func application(_ application: UIApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        push.didRegister(deviceToken: deviceToken)
    }

    func application(_ application: UIApplication, didFailToRegisterForRemoteNotificationsWithError error: any Error) {
        push.didFailToRegister(error)
    }
}

private struct RootView: View {
    let serverURL: String

    @Environment(FleetStore.self) private var fleet
    @Environment(DevicePairing.self) private var pairing
    @Environment(PushCenter.self) private var push
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
            Tab("Waiting for You", systemImage: "tray.full", value: .triage) {
                TriageView()
            }
            .badge(fleet.connection == .unpaired ? 0 : fleet.queue.count)
            Tab("Fleet", systemImage: "circle.hexagongrid", value: .fleet) {
                FleetTab()
            }
            Tab("New", systemImage: "plus.bubble", value: .new) {
                NewRequestView()
            }
        }
        .tabBarMinimizeBehavior(.onScrollDown)
        .onChange(of: fleet.focusRequest) { tab = .triage }
        .onChange(of: push.route) { if push.route != nil { tab = .triage } }
        .onChange(of: fleet.queue) { push.inboxChanged(pending: fleet.queue) }
        .task(id: pairing.deviceID) {
            guard pairing.isPaired else { return }
            await push.activate()
            push.scheduleSync()
        }
        .task(id: scenePhase) {
            guard scenePhase == .active else { return }
            await push.refreshAuthorization()
        }
        .overlay(alignment: .top) { NoticeToast() }
        .task(id: SyncKey(url: serverURL, device: pairing.deviceID, active: scenePhase != .background)) {
            guard scenePhase != .background else { return }
            if let client = AlondraClient(string: serverURL, device: "iphone", signer: pairing.signer) {
                fleet.use(client)
                push.scheduleSync()
            }
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
