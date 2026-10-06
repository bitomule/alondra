import AlondraFeatures
import ComposableArchitecture
import SwiftUI

/// Connects this Mac to Alondra on another Mac of the same network: pick it, compare the code
/// both screens show, and wait for someone to allow this Mac there.
struct MacFindMacStep: View {
    @Bindable var store: StoreOf<SignInFeature>

    private var find: StoreOf<FindMacFeature> { store.scope(state: \.findMac, action: \.findMac) }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            switch store.findMac.step {
            case .browsing: browsing
            case .connecting(let name):
                ProgressView(String(localized: "Connecting to \(name)…"))
                Button("Cancel") { find.send(.cancel) }
            case .comparing(let pairing):
                Label("Check the code on \(pairing.macName)", systemImage: "checkmark.shield").font(.title2.bold())
                Text("That Mac asks whether to allow this one. Allow it only if it shows this same code.")
                    .foregroundStyle(.secondary)
                Text(verbatim: pairing.code)
                    .font(.system(size: 36, weight: .bold, design: .monospaced))
                    .tracking(4)
                    .accessibilityIdentifier("findMac.code")
                ProgressView("Waiting for the other Mac…")
                Button("Cancel") { find.send(.cancel) }
            case .approved:
                Label("Mac connected", systemImage: "checkmark.shield")
            }
        }
        .task { await find.send(.appeared).finish() }
    }

    @ViewBuilder private var browsing: some View {
        Label("Find a Mac on this network", systemImage: "wifi").font(.title2.bold())
        Text("Macs running Alondra on this Wi-Fi appear here. No account needed.").foregroundStyle(.secondary)
        if store.findMac.macs.isEmpty {
            ProgressView("Looking for Macs on this network…")
        } else {
            ForEach(store.findMac.macs) { mac in
                Button { find.send(.choose(mac.id)) } label: {
                    HStack {
                        Label(mac.name, systemImage: "desktopcomputer")
                        Spacer()
                        if mac.paired { Text("Linked").foregroundStyle(.secondary) }
                    }
                }
                .controlSize(.large)
                .accessibilityIdentifier("findMac.mac")
            }
        }
        if let error = store.findMac.error {
            Text(error).foregroundStyle(.red).accessibilityIdentifier("findMac.error")
        }
        DisclosureGroup("Add by address", isExpanded: Binding(get: { store.findMac.addingByAddress },
                                                               set: { _ in find.send(.addByAddress) })) {
            HStack {
                TextField("192.168.1.20 or studio.tailnet.ts.net", text: Binding(get: { store.findMac.address },
                                                                                 set: { find.send(.binding(.set(\.address, $0))) }))
                    .textFieldStyle(.roundedBorder)
                    .autocorrectionDisabled()
                    .accessibilityIdentifier("findMac.address")
                Button("Connect") { find.send(.submitAddress) }
                    .disabled(!store.findMac.canSubmitAddress)
                    .accessibilityIdentifier("findMac.connect")
            }
            .padding(.top, 10)
        }
        Button("Use an account instead") { store.send(.showAccount(true)) }
            .accessibilityIdentifier("findMac.account")
    }
}
