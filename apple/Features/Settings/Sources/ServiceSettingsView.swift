import ComposableArchitecture
import SwiftUI

public struct ServiceSettingsView: View {
    @Environment(\.scenePhase) private var phase
    @Bindable var store: StoreOf<ServiceSettingsFeature>
    public init(store: StoreOf<ServiceSettingsFeature>) { self.store = store }
    public var body: some View {
        Form {
            Section("Account") {
                LabeledContent("Service", value: store.service)
                LabeledContent("Device", value: store.device).textSelection(.enabled)
                Label("Encrypted between your devices", systemImage: "lock.shield")
                Button("Test Connection") { store.send(.checkConnection) }.disabled(store.checking)
                if store.checking { ProgressView("Checking connection…") }
                if store.connected { Label("Connected", systemImage: "checkmark.circle") }
            }
            if store.supportsNotifications {
                Section {
                    if let notification = store.notifications {
                        switch notification.permission {
                        case .unknown: ProgressView()
                        case .authorized: Label("Notifications enabled", systemImage: "bell.badge")
                        case .notDetermined: Button("Turn On") { store.send(.requestPermission) }
                        case .denied: Button("Open Notification Settings") { store.send(.openSystemSettings) }
                        }
                        switch notification.delivery {
                        case .idle: Text("Waiting for token")
                        case .registering: ProgressView("Registering notifications…")
                        case .registered: Label("Registered", systemImage: "checkmark.circle")
                        case .failed(let message): Text(message).foregroundStyle(.red)
                        }
                        Toggle("Quiet Hours", isOn: Binding(get: { store.draft.enabled }, set: { store.send(.quietEnabled($0)) }))
                            .disabled(store.saving)
                        if store.draft.enabled {
                            DatePicker("From", selection: time(store.draft.start, action: ServiceSettingsFeature.Action.quietStart), displayedComponents: .hourAndMinute).disabled(store.saving)
                            DatePicker("To", selection: time(store.draft.end, action: ServiceSettingsFeature.Action.quietEnd), displayedComponents: .hourAndMinute).disabled(store.saving)
                        }
                        if store.conflict {
                            Text("Notification settings changed in another window. Your draft has been kept.")
                            Button("Use Current Settings") { store.send(.useCurrent) }.disabled(store.saving)
                        }
                        Button("Save Notification Settings") { store.send(.save) }.disabled(!store.canSave)
                    } else { ProgressView("Loading settings…") }
                } header: {
                    Text("Notifications")
                } footer: {
                    Text("Quiet hours use the time zone of the Mac running your agents. Changes sync through the encrypted service.")
                }
            }
            if let error = store.error { Section { Text(error).foregroundStyle(.red).textSelection(.enabled) } }
            Section {
                Button("Sign out", role: .destructive) { store.send(.logoutTapped) }
            } footer: {
                Text("Signing out removes this device’s saved connection and notification keys.")
            }
        }
        .formStyle(.grouped)
        .navigationTitle("Settings")
        .confirmationDialog("Sign out of Alondra?", isPresented: Binding(get: { store.confirmLogout }, set: { if !$0 { store.send(.cancelLogout) } })) {
            Button("Sign out", role: .destructive) { store.send(.confirmLogout) }
            Button("Cancel", role: .cancel) { store.send(.cancelLogout) }
        }
        .task { await store.send(.appeared).finish() }
        .onDisappear { store.send(.disappeared) }
        .onChange(of: phase) { _, phase in if phase == .active { store.send(.refreshPermission) } }
    }
    private func time(_ minutes: Int, action: @escaping (Int) -> ServiceSettingsFeature.Action) -> Binding<Date> {
        Binding(get: {
            Calendar.current.date(bySettingHour: minutes / 60, minute: minutes % 60, second: 0, of: .now) ?? .now
        }, set: {
            let components = Calendar.current.dateComponents([.hour, .minute], from: $0)
            store.send(action((components.hour ?? 0) * 60 + (components.minute ?? 0)))
        })
    }
}
