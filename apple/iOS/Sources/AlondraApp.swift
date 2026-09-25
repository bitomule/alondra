import AlondraShared
import AlondraCommands
import AlondraiOSFeature
import ComposableArchitecture
import SwiftUI
import UIKit
import UserNotifications

@main
struct AlondraApp: App {
    @UIApplicationDelegateAdaptor private var delegate: AppDelegate
    @AppStorage(ServerSetting.key) private var serverURL = ServerSetting.defaultURL

    var body: some Scene {
        WindowGroup {
            RootView(serverURL: serverURL, services: delegate.sync)
                .environment(delegate.fleet)
                .environment(delegate.pairing)
                .environment(delegate.push)
                .tint(Palette.dawn)
        }
    }
}

final class AppDelegate: NSObject, UIApplicationDelegate {
    let pairing = DevicePairing()
    lazy var fleet: FleetStore = {
        let address = UserDefaults.standard.string(forKey: ServerSetting.key) ?? ServerSetting.defaultURL
        let base = AlondraClient.serverURL(address) ?? AlondraClient.defaultURL
        let signer = pairing.signer(for: base)
        let client = AlondraClient(base: base, device: "iphone", signer: signer)
        let scope = IOSAppSync.commandScope(for: IOSAppSync.identity(for: client), generation: 1)
        let gate = CommandGate(
            scope: scope,
            journal: FileCommandJournal(directory: CommandJournalLocation.applicationSupport(
                bundleID: Bundle.main.bundleIdentifier ?? "com.bitomule.alondra"))
        )
        return FleetStore(client: client, animated: true, retryDelay: .seconds(3), commandGate: gate, commandScope: scope)
    }()
    lazy var push = PushCenter(fleet: fleet, pairing: pairing)
    lazy var sync = IOSAppSync(fleet: fleet, pairing: pairing, push: push)
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
    let services: IOSAppSync

    @State private var context: IOSWindowContext
    @State private var sceneID = UUID()

    init(serverURL: String, services: IOSAppSync) {
        self.serverURL = serverURL
        self.services = services
        _context = State(initialValue: IOSWindowContext(services: services))
    }

    @Environment(FleetStore.self) private var fleet
    @Environment(DevicePairing.self) private var pairing
    @Environment(PushCenter.self) private var push
    @Environment(\.scenePhase) private var scenePhase
    private struct SyncKey: Hashable {
        let url: String
        let revision: UUID
    }

    var body: some View {
        @Bindable var feature = context.feature
        return TabView(selection: $feature.tab) {
            Tab("Waiting for You", systemImage: "tray.full", value: IOSRootFeature.Tab.triage) {
                TriageView()
            }
            .badge(feature.connection == .unpaired ? 0 : feature.inboxCount)
            Tab("Tasks", systemImage: "checklist", value: IOSRootFeature.Tab.tasks) {
                TasksTab()
            }
            Tab("Fleet", systemImage: "circle.hexagongrid", value: IOSRootFeature.Tab.fleet) {
                FleetTab()
            }
            Tab("New", systemImage: "plus.bubble", value: IOSRootFeature.Tab.new) {
                NewRequestView()
            }
        }
        .id(feature.presentationGeneration)
        .tabBarMinimizeBehavior(.onScrollDown)
        .onChange(of: fleet.focusRequest) { context.feature.send(.focusTriage) }
        .onChange(of: fleet.queue.count) { _, count in context.feature.send(.inboxCountChanged(count)) }
        .onChange(of: push.route) { if push.route != nil { context.feature.send(.focusTriage) } }
        .task(id: pairing.revision) {
            context.feature.send(.pairingChanged)
        }
        .task(id: scenePhase) {
            context.feature.send(.sceneActive(scenePhase != .background))
            context.feature.send(.activityActive(sceneID, scenePhase == .active))
        }
        .onDisappear {
            context.feature.send(.sceneActive(false))
            context.feature.send(.activityActive(sceneID, false))
        }
        .overlay(alignment: .bottom) { NoticeToast() }
        .closeRefusalAlert(fleet)
        .task(id: SyncKey(url: serverURL, revision: pairing.revision)) {
            context.feature.send(.configurationChanged(serverURL))
        }
    }
}

private struct NoticeToast: View {
    @Environment(FleetStore.self) private var fleet

    var body: some View {
        VStack(spacing: 8) {
            if let notice = fleet.notice {
                Label(notice.text, systemImage: notice.failure ? "exclamationmark.triangle.fill" : "checkmark.circle.fill")
                    .font(.subheadline.weight(.semibold))
                    .lineLimit(2)
                    .truncationMode(.tail)
                    .foregroundStyle(notice.failure ? Palette.rose : Palette.ink)
                    .padding(.horizontal, 18)
                    .padding(.vertical, 12)
                    .glassEffect(.regular, in: .rect(cornerRadius: 22))
                    .padding(.horizontal, 24)
                    .transition(.move(edge: .bottom).combined(with: .opacity).combined(with: .scale(scale: 0.9)))
                    .onTapGesture { dismiss() }
                    .accessibilityAction(named: "Dismiss") { dismiss() }
                    .task(id: notice.id) {
                        try? await Task.sleep(for: .seconds(notice.failure ? 4 : 2.2))
                        dismiss()
                    }
                    .id(notice.id)
            }
            if !fleet.uncertainSends.isEmpty {
                Button {
                    Task { await fleet.dismissUncertainSends() }
                } label: {
                    HStack(spacing: 10) {
                        Label("Check Alondra: \(fleet.uncertainActionDescription) may have completed", systemImage: "exclamationmark.triangle.fill")
                            .lineLimit(2)
                        Image(systemName: "xmark")
                            .font(.caption.weight(.bold))
                            .accessibilityHidden(true)
                    }
                    .font(.footnote.weight(.semibold))
                    .foregroundStyle(Palette.rose)
                    .padding(.horizontal, 16)
                    .padding(.vertical, 10)
                }
                .buttonStyle(.plain)
                .glassEffect(.regular.interactive(), in: .rect(cornerRadius: 20))
                .padding(.horizontal, 24)
                .accessibilityHint(Text("Dismiss"))
                .transition(.opacity)
            }
        }
        .padding(.bottom, 92)
        .animation(.spring(duration: 0.45, bounce: 0.3), value: fleet.notice)
        .animation(.snappy, value: fleet.uncertainSends)
        .sensoryFeedback(trigger: fleet.notice) { _, new in
            new?.failure == true ? .error : nil
        }
    }

    private func dismiss() {
        fleet.notice = nil
    }
}
