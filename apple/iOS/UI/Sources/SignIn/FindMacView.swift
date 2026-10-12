import AlondraDesign
import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// The first screen without an account: the Macs running Alondra on this Wi-Fi, the code to
/// compare with the Mac, and the wait while someone allows this iPhone there.
public struct FindMacView: View {
    @Bindable var store: StoreOf<SignInFeature>
    let demo: (() -> Void)?

    public init(store: StoreOf<SignInFeature>, demo: (() -> Void)? = nil) {
        self.store = store
        self.demo = demo
    }

    private var find: StoreOf<FindMacFeature> { store.scope(state: \.findMac, action: \.findMac) }

    public var body: some View {
        PairingLayout(store: store, glow: store.findMac.step == .approved ? Palette.sage : Palette.dawn, step: step,
                      steps: ["Find", "Compare", "Done"]) {
            title
        } subtitle: {
            subtitle
        } content: {
            content
        } footer: {
            if store.findMac.step == .browsing {
                Button("Access from anywhere", systemImage: "globe") { store.send(.showAccount(true)) }
                    .buttonStyle(.glass)
                    .controlSize(.large)
                    .frame(maxWidth: .infinity)
                    .accessibilityIdentifier("findMac.account")
                if !store.additionalMac, let demo {
                    Button("Explore the demo", systemImage: "sparkles", action: demo)
                        .frame(maxWidth: .infinity)
                        .accessibilityIdentifier("demo.enter")
                }
            }
        }
        .task { await find.send(.appeared).finish() }
        .sensoryFeedback(trigger: store.findMac.step) { _, step in
            switch step {
            case .approved: .success
            case .comparing: .impact(weight: .medium)
            default: nil
            }
        }
    }

    private var step: Int {
        switch store.findMac.step {
        case .browsing, .connecting: 0
        case .comparing: 1
        case .approved: 3
        }
    }

    private var title: Text {
        switch store.findMac.step {
        case .comparing: Text("Same code on both?")
        case .approved: Text("You're connected.")
        default: Text("Find your Mac.")
        }
    }

    private var subtitle: Text {
        switch store.findMac.step {
        case .comparing: Text("Your Mac asks whether to allow this iPhone. Allow it only if it shows this same code.")
        case .approved: Text("Your inbox, tasks and conversations are ready.")
        default: Text("Macs running Alondra on this Wi-Fi appear here. No account needed.")
        }
    }

    @ViewBuilder private var content: some View {
        switch store.findMac.step {
        case .browsing where store.findMac.explainsPermission: explanation
        case .browsing where store.findMac.permissionDenied: denied
        case .browsing: browsing
        case .connecting(let name):
            ProgressView(String(localized: "Connecting to \(name)…")).frame(maxWidth: .infinity).padding(.vertical, 24)
            Button("Cancel") { find.send(.cancel) }.frame(maxWidth: .infinity)
        case .comparing(let pairing):
            Label("Check the code on \(pairing.macName)", systemImage: "checkmark.shield").font(.headline)
            Text(verbatim: pairing.code)
                .font(.system(.largeTitle, design: .monospaced, weight: .semibold))
                .tracking(4)
                .frame(maxWidth: .infinity)
                .padding(.vertical, 18)
                .background(Palette.night, in: .rect(cornerRadius: 16))
                .accessibilityIdentifier("findMac.code")
            Label("Waiting for your Mac…", systemImage: "hourglass").font(.callout).foregroundStyle(.secondary)
            Button("Cancel") { find.send(.cancel) }.font(.callout)
        case .approved:
            Label("Mac connected", systemImage: "checkmark.circle.fill").font(.headline).foregroundStyle(Palette.sage)
        }
    }

    @ViewBuilder private var explanation: some View {
        Label("Alondra needs your local network", systemImage: "wifi").font(.headline)
        Text("To find the Macs on this Wi-Fi, iOS will ask whether Alondra may use your local network. Alondra only talks to your Macs, always encrypted.")
            .foregroundStyle(.secondary)
        Button { find.send(.allowSearch) } label: {
            Label("Continue", systemImage: "arrow.right").frame(maxWidth: .infinity)
        }
        .buttonStyle(.glassProminent)
        .buttonBorderShape(.capsule)
        .controlSize(.large)
        .tint(Palette.dawn)
        .accessibilityIdentifier("findMac.allow")
    }

    @ViewBuilder private var denied: some View {
        Label("Local network access is off", systemImage: "wifi.slash").font(.headline)
        Text("Alondra can’t look for your Macs until you turn on Local Network for it in Settings.")
            .foregroundStyle(.secondary)
        Button { find.send(.openSettings) } label: {
            Label("Open Settings", systemImage: "gear").frame(maxWidth: .infinity)
        }
        .buttonStyle(.glassProminent)
        .buttonBorderShape(.capsule)
        .controlSize(.large)
        .tint(Palette.dawn)
        .accessibilityIdentifier("findMac.openSettings")
        addByAddress
    }

    @ViewBuilder private var browsing: some View {
        if store.findMac.macs.isEmpty {
            ProgressView("Looking for Macs on this network…").frame(maxWidth: .infinity).padding(.vertical, 12)
            Text("Is Alondra running on your Mac? Run `alondra` in Terminal, and keep both devices on the same Wi-Fi.")
                .font(.footnote)
                .foregroundStyle(.secondary)
        } else {
            ForEach(store.findMac.macs) { mac in
                Button { find.send(.choose(mac.id)) } label: {
                    HStack {
                        Label(mac.name, systemImage: "desktopcomputer")
                        Spacer()
                        if mac.paired { Text("Linked").foregroundStyle(.secondary) }
                        else { Image(systemName: "chevron.right").foregroundStyle(.tertiary) }
                    }
                    .contentShape(.rect)
                }
                .buttonStyle(.plain)
                .font(.headline)
                .padding(.vertical, 6)
                .accessibilityIdentifier("findMac.mac")
            }
        }
        notThere
        addByAddress
    }

    private var notThere: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Not there? On your Mac run:").font(.footnote).foregroundStyle(.secondary)
            InstallCommands().font(.footnote)
        }
        .accessibilityIdentifier("findMac.notThere")
    }

    /// The error of the last attempt, and pairing by a typed address, also when Bonjour is off.
    @ViewBuilder private var addByAddress: some View {
        if let error = store.findMac.error {
            Label(error, systemImage: "exclamationmark.circle")
                .font(.callout)
                .foregroundStyle(Palette.rose)
                .accessibilityIdentifier("findMac.error")
        }
        DisclosureGroup("Add by address", isExpanded: Binding(get: { store.findMac.addingByAddress },
                                                               set: { _ in find.send(.addByAddress) })) {
            VStack(alignment: .leading, spacing: 12) {
                Text("For a Mac on Tailscale or another network, type its address.").font(.footnote).foregroundStyle(.secondary)
                TextField("192.168.1.20 or studio.tailnet.ts.net", text: Binding(get: { store.findMac.address },
                                                                                 set: { find.send(.binding(.set(\.address, $0))) }))
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .keyboardType(.URL)
                    .padding(12)
                    .background(Palette.night, in: .rect(cornerRadius: 12))
                    .accessibilityIdentifier("findMac.address")
                Button("Connect") { find.send(.submitAddress) }
                    .disabled(!store.findMac.canSubmitAddress)
                    .accessibilityIdentifier("findMac.connect")
            }
            .padding(.top, 12)
        }
        .font(.callout)
    }
}
