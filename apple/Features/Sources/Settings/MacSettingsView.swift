import ComposableArchitecture
import SwiftUI

public struct MacSettingsView<PairingContent: View>: View {
    @Environment(\.scenePhase) private var phase
    @Bindable var store: StoreOf<SettingsFeature>
    private let ink: Color
    private let success: Color
    private let failure: Color
    private let pairingContent: (StoreOf<SignInFeature>) -> PairingContent
    public init(store: StoreOf<SettingsFeature>, ink: Color = .primary, success: Color = .green, failure: Color = .red, @ViewBuilder pairingContent: @escaping (StoreOf<SignInFeature>) -> PairingContent) {
        self.pairingContent = pairingContent
        self.store = store; self.ink = ink; self.success = success; self.failure = failure
    }
    public var body: some View {
        Form {
            if store.usesAccount {
                account
            } else {
                Section {
                    LabeledContent("Connected to") { Text("Alondra on this Mac") }
                } header: {
                    Text("Connection")
                } footer: {
                    Text("This Mac talks to its own Alondra directly. No account is needed.")
                }
            }
            macsSection
            MachineSettingsSection(store: store.scope(state: \.machineSettings, action: \.machineSettings))
            AllowedRulesSection(store: store.scope(state: \.allowedRules, action: \.allowedRules))
            ServerSettingsSection(store: store.scope(state: \.accounts, action: \.accounts))
            McpServersRow(store: store.scope(state: \.mcpServers, action: \.mcpServers))
            if store.devices != nil || store.devicesError != nil { devicesSection }
            if store.supportsNotifications {
                notifications
            }
            Section {
                Button { store.send(.checkConnection) } label: {
                    HStack {
                        Text("Test Connection")
                        Spacer()
                        checkBadge
                    }
                }
                .disabled(store.checking)
            } header: {
                Text("Server")
            }
            ConnectionMeasurementsSection(store: store.scope(state: \.measurements, action: \.measurements))
        }
        .sheet(item: $store.scope(state: \.pairing, action: \.pairing)) { pairing in
            NavigationStack {
                pairingContent(pairing)
                    .toolbar {
                        ToolbarItem(placement: .cancellationAction) {
                            Button("Close") { store.send(.pairing(.dismiss)) }
                        }
                    }
            }
        }
        .sheet(item: $store.scope(state: \.macSettings, action: \.macSettings)) { mac in
            NavigationStack {
                ThisMacSettingsView(store: mac)
                    .toolbar {
                        ToolbarItem(placement: .cancellationAction) {
                            Button("Close") { store.send(.macSettings(.dismiss)) }
                        }
                    }
            }
            .frame(minWidth: 420, minHeight: 360)
        }
        .formStyle(.grouped)
        .navigationTitle("Settings")
        #if os(iOS)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .confirmationAction) {
                Button("Save", systemImage: "checkmark") { store.send(.save) }
                    .disabled(!store.canSave)
            }
        }
        .sensoryFeedback(trigger: store.connected) { _, connected in connected ? .success : nil }
        .sensoryFeedback(trigger: store.connectionError) { _, error in error == nil ? nil : .error }
        #endif
        .confirmationDialog("Sign out of Alondra?", isPresented: Binding(get: { store.confirmLogout }, set: { if !$0 { store.send(.cancelLogout) } }), titleVisibility: .visible) {
            Button("Sign out", role: .destructive) { store.send(.confirmLogout) }
            Button("Cancel", role: .cancel) { store.send(.cancelLogout) }
        } message: {
            Text("Signing out removes this device’s saved connection and notification keys.")
        }
        .modifier(MacSettingsDeviceDialogs(store: store))
        .task { await store.send(.appeared).finish() }
        .task { await store.send(.machineSettings(.task)).finish() }
        .task { await store.send(.allowedRules(.task)).finish() }
        .task { await store.send(.accounts(.task)).finish() }
        .task { await store.send(.mcpServers(.task)).finish() }
        .onDisappear { store.send(.disappeared) }
        .onChange(of: phase) { _, phase in if phase == .active { store.send(.refreshPermission) } }
    }

    private var account: some View {
        Section {
            LabeledContent("Service") {
                Text(store.service)
                    .font(.system(.subheadline, design: .monospaced))
                    .foregroundStyle(ink)
            }
            LabeledContent("Device") {
                Text(store.device)
                    .font(.system(.caption, design: .monospaced))
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .textSelection(.enabled)
            }
            Button("Sign out", systemImage: "link.badge.minus", role: .destructive) { store.send(.logoutTapped) }
        } header: {
            Text("Account")
        } footer: {
            Text("Encrypted between your devices")
        }
    }

    private var macsSection: some View {
        Section {
            ForEach(store.macs) { mac in
                HStack {
                    Button { store.send(.selectMac(mac.id)) } label: {
                        HStack {
                            VStack(alignment: .leading, spacing: 2) {
                                Label(mac.displayName, systemImage: "desktopcomputer")
                                if let status = store.macStatuses[mac.id] { MacStatusLabel(status: status).font(.caption) }
                            }
                            Spacer()
                            if store.selectingMacID == mac.id || store.forgettingMacID == mac.id { ProgressView() }
                            else if store.selectedMacID == mac.id {
                                Image(systemName: "checkmark").foregroundStyle(success).accessibilityHidden(true)
                            }
                        }
                    }
                    .disabled(store.selectingMacID != nil || store.selectedMacID == mac.id)
                    .accessibilityLabel(mac.displayName)
                    .accessibilityValue(store.selectedMacID == mac.id ? "Selected" : "")
                    Button { store.send(.openMacSettings(mac.id)) } label: {
                        Label("This Mac’s Settings…", systemImage: "info.circle").labelStyle(.iconOnly)
                    }
                    .buttonStyle(.borderless)
                    .help(Text("This Mac’s Settings…"))
                    .accessibilityIdentifier("settings.macSettings")
                }
                .contextMenu {
                    Button("This Mac’s Settings…") { store.send(.openMacSettings(mac.id)) }
                    if mac.local { Button("Forget This Mac", role: .destructive) { store.send(.forgetMacTapped(mac.id)) } }
                }
            }
            if let problem = store.macError {
                Label { Text(problem.title) } icon: { Image(systemName: problem.systemImage) }
                    .foregroundStyle(failure)
            }
            if let error = store.forgetError { Text(error).foregroundStyle(failure) }
            Button("Connect to Another Mac…", systemImage: "plus") { store.send(.linkMac) }
                .disabled(store.selectingMacID != nil || store.pairingCleanup)
                .accessibilityIdentifier("settings.addMac")
            if store.showsSignOutWithMacs {
                Button("Sign out", systemImage: "link.badge.minus", role: .destructive) { store.send(.logoutTapped) }
                    .accessibilityIdentifier("settings.signOut")
            }
        } header: {
            Text("Macs")
        }
        .task { await store.send(.loadMacStatuses).finish() }
    }

    private var devicesSection: some View {
        Section {
            ForEach(store.devices ?? []) { device in
                HStack {
                    VStack(alignment: .leading, spacing: 2) {
                        Label(device.name, systemImage: "iphone")
                        if let origin = device.origin { Text(origin.title).font(.caption).foregroundStyle(.secondary) }
                        Group {
                            if let lastUsed = device.lastUsed {
                                Text("Used \(lastUsed, format: .relative(presentation: .named))")
                            } else {
                                Text("Not used yet")
                            }
                        }
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    }
                    Spacer()
                    Button("Revoke", role: .destructive) { store.send(.revokeDeviceTapped(device.id)) }
                        .disabled(store.revokingDeviceID != nil)
                }
                .accessibilityIdentifier("settings.device")
            }
            if store.devices?.isEmpty == true {
                Text("No other device can use this Mac yet.").foregroundStyle(.secondary)
            }
            if let error = store.devicesError { Text(error).foregroundStyle(failure) }
        } header: {
            Text("Devices that can use this Mac")
        } footer: {
            Text("Revoking a device cuts it off on every path at once.")
        }
    }

    private var notifications: some View {
        Section {
            if let notification = store.notifications {
                LabeledContent("Alerts") {
                    switch notification.permission {
                    case .unknown: ProgressView()
                    case .authorized:
                        Text("\(Image(systemName: "bell.badge.fill")) On").foregroundStyle(success)
                    case .notDetermined:
                        Button("Turn On") { store.send(.requestPermission) }
                    case .denied:
                        Button("Off · Open Settings") { store.send(.openSystemSettings) }
                            .foregroundStyle(failure)
                    }
                }
                if notification.permission == .authorized {
                    LabeledContent("On the Mac") { registration(notification.delivery) }
                }
                Toggle("Quiet Hours", isOn: Binding(get: { store.draft.enabled }, set: { store.send(.quietEnabled($0), animation: .default) }))
                    .disabled(store.saving && !store.automaticallySavesQuietHours)
                if store.draft.enabled {
                    DatePicker("From", selection: time(store.draft.start, action: SettingsFeature.Action.quietStart), displayedComponents: .hourAndMinute).disabled(store.saving && !store.automaticallySavesQuietHours)
                    DatePicker("To", selection: time(store.draft.end, action: SettingsFeature.Action.quietEnd), displayedComponents: .hourAndMinute).disabled(store.saving && !store.automaticallySavesQuietHours)
                }
                if store.conflict {
                    Text("Notification settings changed in another window. Your draft has been kept.")
                    Button("Use Current Settings") { store.send(.useCurrent) }.disabled(store.saving)
                }
                if let error = store.error {
                    Label { Text(verbatim: error) } icon: { Image(systemName: "exclamationmark.triangle") }
                        .foregroundStyle(failure)
                }
                #if os(macOS)
                Button("Save Notification Settings") { store.send(.save) }.disabled(!store.canSave)
                #endif
            } else { ProgressView() }
        } header: {
            Text("Notifications")
        } footer: {
            Text(store.draft.enabled
                 ? String(localized: "Between \(clock(store.draft.start)) and \(clock(store.draft.end)), alerts arrive without sound and without waking the screen.")
                 : String(localized: "You can reply, allow or approve from the Lock Screen."))
        }
    }

    @ViewBuilder private var checkBadge: some View {
        if store.checking { ProgressView() }
        else if store.connected { Label("Connected", systemImage: "checkmark.circle.fill").foregroundStyle(success) }
        else if let problem = store.connectionError { Text(problem.title).foregroundStyle(failure).lineLimit(1) }
    }

    @ViewBuilder private func registration(_ delivery: NotificationSettings.Delivery) -> some View {
        switch delivery {
        case .idle: Text("Waiting for token").foregroundStyle(.secondary)
        case .registering: ProgressView()
        case .registered: Text("\(Image(systemName: "checkmark.circle.fill")) Registered").foregroundStyle(success)
        case .failed(let message): Text(message).foregroundStyle(failure).lineLimit(2)
        }
    }

    private func clock(_ minutes: Int) -> String {
        String(format: "%02d:%02d", minutes / 60, minutes % 60)
    }

    private func time(_ minutes: Int, action: @escaping (Int) -> SettingsFeature.Action) -> Binding<Date> {
        Binding(get: {
            Calendar.current.date(bySettingHour: minutes / 60, minute: minutes % 60, second: 0, of: .now) ?? .now
        }, set: {
            let components = Calendar.current.dateComponents([.hour, .minute], from: $0)
            store.send(action((components.hour ?? 0) * 60 + (components.minute ?? 0)))
        })
    }
}
