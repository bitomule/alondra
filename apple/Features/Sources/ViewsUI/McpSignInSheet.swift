import AlondraDesign
import ComposableArchitecture
import SwiftUI

/// The sheet Connect opens on the card of the tasks that wait for an MCP server.
public struct McpSignInSheet: View {
    @Bindable var store: StoreOf<McpSignInFeature>

    public init(store: StoreOf<McpSignInFeature>) { self.store = store }

    public var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Sign in to \(store.server)").font(.title2.bold())
            status
            if store.asksToPaste, showsPaste { paste }
            Spacer(minLength: 0)
            buttons
        }
        .padding(24)
        .frame(maxWidth: .infinity, alignment: .leading)
        .task { await store.send(.appeared).finish() }
    }

    private var showsPaste: Bool {
        switch store.phase {
        case .signingIn, .failed(.notAReturn): true
        default: false
        }
    }

    @ViewBuilder private var status: some View {
        switch store.phase {
        case .opening:
            ProgressView("Opening the sign-in…")
        case .signingIn(.listening):
            Text("Sign in to \(store.server) in the page that opened. Alondra brings the sign-in back to your Mac by itself, and the waiting tasks go on.")
        case .signingIn(.claudeOnThisMac):
            Text("Sign in to \(store.server) in the browser. Claude on this Mac gets the sign-in by itself, and the waiting tasks go on.")
        case .signingIn(.paste):
            Text("Sign in to \(store.server) in the browser. Afterwards the browser shows a page that does not load: copy its address and paste it here.")
        case .claudeAi:
            Text("\(store.server) is a claude.ai connector. Connect it on claude.ai in the page that opened, then press Reconnect.")
        case .sending:
            ProgressView("Signing in to \(store.server)…")
        case .reconnecting:
            Text("The waiting tasks read their sign-in to \(store.server) again, and go on once it is connected.")
        case .connected:
            Label("Signed in to \(store.server): the waiting tasks go on.", systemImage: "checkmark.circle.fill")
                .foregroundStyle(.green)
        case .failed(let failure):
            Label(message(failure), systemImage: "exclamationmark.triangle.fill").foregroundStyle(.orange)
        }
    }

    private func message(_ failure: McpSignInFeature.Failure) -> LocalizedStringKey {
        switch failure {
        case .notAReturn: "That is not the address the browser came back to. Copy the whole address of the page that did not load: it starts with http://localhost: and has /callback."
        case .cut: "The sign-in to \(store.server) was cut. Press Connect again."
        case .noNode: "No task that waits for \(store.server) can open its sign-in now. Try again in a moment, or go on without it from the card."
        case .unreachable: "Alondra could not reach your Mac. Try again."
        }
    }

    @ViewBuilder private var paste: some View {
        VStack(alignment: .leading, spacing: 8) {
            if store.phase != .signingIn(.paste) {
                Text("Did the page after signing in not load? Paste its address here.").font(.subheadline)
            }
            HStack {
                TextField("http://localhost:…/callback?…", text: $store.pasted)
                    .textFieldStyle(.roundedBorder)
                    .autocorrectionDisabled()
                    #if os(iOS)
                    .textInputAutocapitalization(.never)
                    .keyboardType(.URL)
                    #endif
                    .onSubmit { store.send(.sendPastedTapped) }
                PasteButton(payloadType: String.self) { strings in
                    guard let text = strings.first else { return }
                    Task { @MainActor in store.send(.pasted(text)) }
                }
                .labelStyle(.iconOnly)
            }
            if store.pasteRefused {
                Text("That is not the address the browser came back to: it starts with http://localhost: and has /callback.")
                    .font(.footnote).foregroundStyle(.orange)
            }
        }
    }

    @ViewBuilder private var buttons: some View {
        HStack {
            Button("Close") { store.send(.closeTapped) }
            Spacer()
            switch store.phase {
            case .claudeAi:
                Button("Open claude.ai again") { store.send(.connectAgainTapped) }
                Button("Reconnect") { store.send(.reconnectTapped) }.buttonStyle(.borderedProminent)
            case .failed(.notAReturn), .signingIn(.paste):
                Button("Send") { store.send(.sendPastedTapped) }
                    .buttonStyle(.borderedProminent)
                    .disabled(store.pasted.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            case .failed:
                Button("Connect again") { store.send(.connectAgainTapped) }.buttonStyle(.borderedProminent)
            case .signingIn:
                Button("Open the sign-in again") { store.send(.connectAgainTapped) }
                if store.asksToPaste {
                    Button("Send") { store.send(.sendPastedTapped) }
                        .buttonStyle(.borderedProminent)
                        .disabled(store.pasted.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
            case .connected:
                Button("Done") { store.send(.closeTapped) }.buttonStyle(.borderedProminent)
            case .opening, .sending, .reconnecting:
                EmptyView()
            }
        }
    }
}
