import AlondraShared
import SwiftUI

struct SettingsView: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(DevicePairing.self) private var pairing
    @AppStorage(ServerSetting.key) private var serverURL = ServerSetting.defaultURL
    @State private var draft = ""
    @State private var check: Check = .idle
    @State private var detent = PresentationDetent.medium

    private enum Check: Equatable {
        case idle, checking, ok, failed(String)
    }

    private var candidate: AlondraClient? {
        guard let url = AlondraClient.serverURL(draft) else { return nil }
        return AlondraClient(base: url, device: "iphone", signer: pairing.signer(for: url))
    }

    var body: some View {
        NavigationStack {
            Form {
                PairingSection(detent: $detent)
                NotificationsSection()
                Section {
                    TextField(ServerSetting.defaultURL, text: $draft)
                        .font(.mono(.body))
                        .keyboardType(.URL)
                        .textContentType(.URL)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .onChange(of: draft) { check = .idle }
                    Button {
                        Task { await test() }
                    } label: {
                        HStack {
                            Text("Test Connection")
                            Spacer()
                            checkBadge
                        }
                    }
                    .disabled(candidate == nil || check == .checking)
                } header: {
                    Text("Server")
                } footer: {
                    Text(candidate == nil && !draft.isEmpty ? String(localized: "Enter a full http:// or https:// address.") : String(localized: "Where alondrad listens. In the simulator, the Mac is 127.0.0.1."))
                }
                Section {
                    Button("Use the Local Address") { draft = ServerSetting.defaultURL }
                        .disabled(draft == ServerSetting.defaultURL)
                }
            }
            .navigationTitle("Settings")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close", systemImage: "xmark") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save", systemImage: "checkmark") {
                        if let candidate { serverURL = candidate.base.absoluteString }
                        dismiss()
                    }
                    .disabled(candidate == nil)
                }
            }
            .onAppear { draft = serverURL }
            .presentationDetents([.medium, .large], selection: $detent)
            .sensoryFeedback(trigger: check) { _, new in
                switch new {
                case .ok: .success
                case .failed: .error
                default: nil
                }
            }
        }
    }

    @ViewBuilder
    private var checkBadge: some View {
        switch check {
        case .idle: EmptyView()
        case .checking: ProgressView()
        case .ok: Label("Connected", systemImage: "checkmark.circle.fill").foregroundStyle(Palette.sage)
        case .failed(let reason): Text(reason).foregroundStyle(Palette.rose).lineLimit(1)
        }
    }

    private func test() async {
        guard let candidate else { return }
        check = .checking
        do {
            try await candidate.health()
            check = .ok
        } catch {
            check = .failed(error.friendlyDescription)
        }
    }
}

enum ServerSetting {
    static let key = "serverURL"
    static let defaultURL = AlondraClient.defaultURL.absoluteString
}

private struct PairingSection: View {
    @Binding var detent: PresentationDetent
    @Environment(DevicePairing.self) private var pairing
    @State private var confirmUnpair = false

    var body: some View {
        Section {
            if let url = pairing.pairedURL, let device = pairing.deviceID {
                LabeledContent("Mac") {
                    Text(url.host() ?? url.absoluteString)
                        .font(.mono(.subheadline))
                        .foregroundStyle(Palette.ink)
                }
                LabeledContent("Device") {
                    Text(device)
                        .font(.mono(.caption))
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .textSelection(.enabled)
                }
                NavigationLink {
                    PairingView()
                        .onAppear { detent = .large }
                } label: {
                    Label("Pair Again", systemImage: "qrcode.viewfinder")
                }
                Button("Unpair", systemImage: "link.badge.minus", role: .destructive) { confirmUnpair = true }
                    .confirmationDialog("Unpair this iPhone?", isPresented: $confirmUnpair, titleVisibility: .visible) {
                        Button("Unpair", role: .destructive) { pairing.unpair() }
                    } message: {
                        Text("This iPhone’s key will be deleted. To connect again from outside this Mac, you’ll have to pair it again.")
                    }
            } else {
                NavigationLink {
                    PairingView()
                        .onAppear { detent = .large }
                } label: {
                    Label {
                        VStack(alignment: .leading, spacing: 2) {
                            Text("Pair with the Mac")
                            Text("Scan the code from alondra pair")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    } icon: {
                        Image(systemName: "qrcode.viewfinder")
                            .foregroundStyle(Palette.dawn)
                    }
                }
            }
        } header: {
            Text("This iPhone")
        } footer: {
            if pairing.isPaired {
                Text("Requests outside 127.0.0.1 are signed with a key that never leaves this iPhone.")
            } else {
                Text("Without pairing, you can only talk to alondrad at 127.0.0.1.")
            }
        }
    }
}

private struct NotificationsSection: View {
    @Environment(PushCenter.self) private var push
    @Environment(DevicePairing.self) private var pairing
    @Environment(\.openURL) private var openURL

    var body: some View {
        @Bindable var push = push
        Section {
            LabeledContent("Alerts") {
                switch push.authorization {
                case .authorized:
                    Text("\(Image(systemName: "bell.badge.fill")) On").foregroundStyle(Palette.sage)
                case .denied:
                    Button("Off · Open Settings") {
                        if let url = URL(string: UIApplication.openNotificationSettingsURLString) { openURL(url) }
                    }
                    .foregroundStyle(Palette.rose)
                case .notDetermined:
                    Button("Turn On") { Task { await push.activate() } }
                case .unknown:
                    ProgressView()
                }
            }
            if push.authorization == .authorized, pairing.isPaired {
                LabeledContent("On the Mac") { registrationStatus }
            }
            Toggle("Quiet Hours", isOn: $push.quietHours.enabled.animation())
            if push.quietHours.enabled {
                DatePicker("From", selection: time($push.quietHours.start), displayedComponents: .hourAndMinute)
                DatePicker("To", selection: time($push.quietHours.end), displayedComponents: .hourAndMinute)
            }
        } header: {
            Text("Notifications")
        } footer: {
            Text(push.quietHours.enabled
                ? String(localized: "Between \(QuietHours.clock(push.quietHours.start)) and \(QuietHours.clock(push.quietHours.end)), alerts arrive without sound and without waking the screen.")
                : String(localized: "You can reply, allow or approve from the Lock Screen."))
        }
        .task { await push.refreshAuthorization() }
    }

    @ViewBuilder
    private var registrationStatus: some View {
        switch push.registration {
        case .idle: Text("Waiting for token").foregroundStyle(.secondary)
        case .sending: ProgressView()
        case .registered: Text("\(Image(systemName: "checkmark.circle.fill")) Registered").foregroundStyle(Palette.sage)
        case .failed(let reason): Text(reason).foregroundStyle(Palette.rose).lineLimit(2)
        }
    }

    private func time(_ minutes: Binding<Int>) -> Binding<Date> {
        Binding {
            Calendar.current.date(bySettingHour: minutes.wrappedValue / 60, minute: minutes.wrappedValue % 60, second: 0, of: .now) ?? .now
        } set: { date in
            let parts = Calendar.current.dateComponents([.hour, .minute], from: date)
            minutes.wrappedValue = (parts.hour ?? 0) * 60 + (parts.minute ?? 0)
        }
    }
}
