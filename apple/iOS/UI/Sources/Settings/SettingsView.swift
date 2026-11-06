import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// The sheet behind the gear: this iPhone and its Macs, the alerts it may send, and the version.
public struct SettingsView: View {
    @Environment(\.locale) private var locale
    @Environment(\.scenePhase) private var phase
    @Environment(\.calendar) private var calendar
    @Bindable var store: StoreOf<SettingsFeature>

    public init(store: StoreOf<SettingsFeature>) { self.store = store }

    public var body: some View {
        Form {
            account
            MachineSettingsSection(store: store.scope(state: \.machineSettings, action: \.machineSettings))
            AllowedRulesSection(store: store.scope(state: \.allowedRules, action: \.allowedRules))
            ServerSettingsSection(store: store.scope(state: \.accounts, action: \.accounts))
            McpServersRow(store: store.scope(state: \.mcpServers, action: \.mcpServers))
            MetricsRow(store: store.scope(state: \.metrics, action: \.metrics))
            if store.supportsNotifications { notifications } else if store.alertsNeedAccount { alertsNeedAccount }
            ConnectionMeasurementsSection(store: store.scope(state: \.measurements, action: \.measurements))
            CrashReportRow(store: store.scope(state: \.crashReport, action: \.crashReport))
            LicensesRow()
            about
        }
        .scrollContentBackground(.hidden)
        .background(DawnBackground())
        .navigationTitle("Settings")
        .navigationBarTitleDisplayMode(.inline)
        .sheet(item: $store.scope(state: \.pairing, action: \.pairing)) { pairing in
            NavigationStack {
                SignInView(store: pairing)
                    .toolbar {
                        ToolbarItem(placement: .cancellationAction) {
                            Button("Close", systemImage: "xmark") { store.send(.pairing(.dismiss)) }
                        }
                    }
            }
        }
        .navigationDestination(item: $store.scope(state: \.macSettings, action: \.macSettings)) { mac in
            ThisMacSettingsView(store: mac)
                .scrollContentBackground(.hidden)
                .background(DawnBackground())
                .navigationBarTitleDisplayMode(.inline)
        }
        .confirmationDialog(
            "Sign out of Alondra?",
            isPresented: Binding(get: { store.confirmLogout }, set: { if !$0 { store.send(.cancelLogout) } }),
            titleVisibility: .visible
        ) {
            Button("Sign out", role: .destructive) { store.send(.confirmLogout) }
        } message: {
            Text("Signing out removes this device’s saved connection and notification keys.")
        }
        .confirmationDialog(
            "Forget this Mac?",
            isPresented: Binding(get: { store.confirmForgetMacID != nil }, set: { if !$0 { store.send(.cancelForgetMac) } }),
            titleVisibility: .visible
        ) {
            Button("Forget Mac", role: .destructive) { store.send(.confirmForgetMac) }
        } message: {
            Text("This iPhone deletes its keys for this Mac. To use it again, find it and pair it again.")
        }
        .task { await store.send(.appeared).finish() }
        .task { await store.send(.machineSettings(.task)).finish() }
        .task { await store.send(.allowedRules(.task)).finish() }
        .task { await store.send(.accounts(.task)).finish() }
        .task { await store.send(.mcpServers(.task)).finish() }
        .task { await store.send(.metrics(.task)).finish() }
        .onDisappear { store.send(.disappeared) }
        .onChange(of: phase) { _, phase in
            if phase == .active {
                store.send(.refreshPermission)
                store.send(.loadMacStatuses)
                store.send(.loadAccountName)
            }
        }
    }

    private var account: some View {
        Section {
            if store.usesAccount {
                LabeledContent("Account") {
                    (store.accountName.map { Text(verbatim: $0) } ?? Text("Account connected"))
                        .font(.mono(.subheadline))
                        .foregroundStyle(Palette.ink)
                }
            }
            LabeledContent("Device") {
                Text(verbatim: store.device)
                    .font(.mono(.caption))
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .textSelection(.enabled)
            }
            ForEach(store.macs) { mac in
                VStack(alignment: .leading, spacing: 8) {
                    HStack {
                        Button { store.send(.selectMac(mac.id)) } label: {
                            HStack {
                                VStack(alignment: .leading, spacing: 2) {
                                    Label(mac.displayName, systemImage: "desktopcomputer")
                                        .foregroundStyle(Palette.ink)
                                    if let status = store.macStatuses[mac.id] {
                                        MacStatusLabel(status: status).font(.caption).padding(.leading, 34)
                                    }
                                }
                                Spacer()
                                if store.selectingMacID == mac.id || store.forgettingMacID == mac.id {
                                    ProgressView()
                                } else if store.selectedMacID == mac.id {
                                    Image(systemName: "checkmark").foregroundStyle(Palette.sage).accessibilityHidden(true)
                                }
                            }
                        }
                        .buttonStyle(.borderless)
                        .disabled(store.selectingMacID != nil || (store.selectedMacID == mac.id && !mac.hasAlternatePairings))
                        .accessibilityLabel(Text(verbatim: mac.displayName))
                        .accessibilityValue(Text(verbatim: [
                            store.selectedMacID == mac.id ? String(localized: "Selected", locale: locale) : nil,
                            store.macStatuses[mac.id]?.accessibilityDescription(locale: locale),
                        ].compactMap { $0 }.joined(separator: ", ")))
                        Button { store.send(.openMacSettings(mac.id)) } label: {
                            Label("This Mac’s settings", systemImage: "info.circle").labelStyle(.iconOnly)
                        }
                        .buttonStyle(.borderless)
                        .accessibilityIdentifier("settings.macSettings")
                    }
                    if store.macStatuses[mac.id]?.canPairAgain == true {
                        Button("Link Again", systemImage: "link.badge.plus") { store.send(.pairAgain(mac.id)) }
                            .accessibilityIdentifier("settings.pairAgain")
                    }
                }
                .swipeActions {
                    if mac.forgettable {
                        Button("Forget", systemImage: "trash", role: .destructive) { store.send(.forgetMacTapped(mac.id)) }
                    }
                }
            }
            .task { await store.send(.loadMacStatuses).finish() }
            .task { await store.send(.loadAccountName).finish() }
            if let problem = store.macError {
                Label { Text(problem.title) } icon: { Image(systemName: problem.systemImage) }
                    .foregroundStyle(Palette.rose)
                    .accessibilityIdentifier("settings.macError")
            }
            if let error = store.forgetError { Text(verbatim: error).foregroundStyle(Palette.rose) }
            Button("Add a Mac", systemImage: "plus") { store.send(.linkMac) }
                .disabled(store.selectingMacID != nil || store.pairingCleanup)
                .accessibilityIdentifier("settings.addMac")
            if store.usesAccount {
                Button("Sign out", systemImage: "link.badge.minus", role: .destructive) { store.send(.logoutTapped) }
            }
        } header: {
            Text("This iPhone")
        } footer: {
            Text("Encrypted between your devices")
        }
    }

    private var notifications: some View {
        Section {
            if let notification = store.notifications {
                LabeledContent("Alerts") {
                    switch notification.permission {
                    case .unknown: ProgressView()
                    case .authorized:
                        Text("\(Image(systemName: "bell.badge.fill")) On").foregroundStyle(Palette.sage)
                    case .notDetermined:
                        Button("Turn On") { store.send(.requestPermission) }
                    case .denied:
                        Button("Off · Open Settings") { store.send(.openSystemSettings) }
                            .foregroundStyle(Palette.rose)
                    }
                }
                if notification.permission == .authorized {
                    LabeledContent("On the Mac") { registration(notification.delivery) }
                }
                Toggle("Quiet Hours", isOn: Binding(get: { store.draft.enabled }, set: { store.send(.quietEnabled($0), animation: .default) }))
                    .disabled(busy)
                if store.draft.enabled {
                    DatePicker("From", selection: time(store.draft.start, SettingsFeature.Action.quietStart), displayedComponents: .hourAndMinute)
                        .disabled(busy)
                    DatePicker("To", selection: time(store.draft.end, SettingsFeature.Action.quietEnd), displayedComponents: .hourAndMinute)
                        .disabled(busy)
                }
                if store.conflict {
                    Text("Notification settings changed in another window. Your draft has been kept.")
                    Button("Use Current Settings") { store.send(.useCurrent) }.disabled(store.saving)
                }
                if let error = store.error {
                    Label { Text(verbatim: error) } icon: { Image(systemName: "exclamationmark.triangle") }
                        .foregroundStyle(Palette.rose)
                        .accessibilityIdentifier("settings.notificationsError")
                }
            } else {
                ProgressView()
            }
        } header: {
            Text("Notifications")
        } footer: {
            if store.draft.enabled {
                Text("Between \(clock(store.draft.start)) and \(clock(store.draft.end)), alerts arrive without sound and without waking the screen.")
            } else {
                Text("You can reply, allow or approve from the Lock Screen.")
            }
        }
    }

    private var alertsNeedAccount: some View {
        Section("Notifications") {
            Label("Alerts reach this iPhone through your Alondra account. Your Macs are linked without one, so they can’t send alerts.",
                  systemImage: "bell.slash")
                .foregroundStyle(.secondary)
                .accessibilityIdentifier("settings.alertsNeedAccount")
            Button("Use my account", systemImage: "person.crop.circle") { store.send(.useAccount) }
                .disabled(store.selectingMacID != nil || store.pairingCleanup)
                .accessibilityIdentifier("settings.useAccount")
        }
    }

    private var about: some View {
        Section("About") {
            LabeledContent("Version") {
                Text(verbatim: Self.version).font(.mono(.subheadline))
            }
        }
    }

    private var busy: Bool { store.saving && !store.automaticallySavesQuietHours }

    private static var version: String {
        let info = Bundle.main.infoDictionary
        let version = info?["CFBundleShortVersionString"] as? String ?? "0"
        let build = info?["CFBundleVersion"] as? String ?? "0"
        return "\(version) (\(build))"
    }

    @ViewBuilder private func registration(_ delivery: NotificationSettings.Delivery) -> some View {
        switch delivery {
        case .idle: Text("Waiting for token").foregroundStyle(.secondary)
        case .registering: ProgressView()
        case .registered: Text("\(Image(systemName: "checkmark.circle.fill")) Registered").foregroundStyle(Palette.sage)
        case .failed(let message): Text(verbatim: message).foregroundStyle(Palette.rose)
        }
    }

    private func clock(_ minutes: Int) -> String {
        String(format: "%02d:%02d", minutes / 60, minutes % 60)
    }

    private func time(_ minutes: Int, _ action: @escaping (Int) -> SettingsFeature.Action) -> Binding<Date> {
        Binding(get: {
            calendar.date(bySettingHour: minutes / 60, minute: minutes % 60, second: 0, of: .now) ?? .now
        }, set: {
            let components = calendar.dateComponents([.hour, .minute], from: $0)
            store.send(action((components.hour ?? 0) * 60 + (components.minute ?? 0)))
        })
    }
}
