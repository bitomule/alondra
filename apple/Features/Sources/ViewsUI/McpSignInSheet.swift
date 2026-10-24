import AlondraDesign
import ComposableArchitecture
import SwiftUI

/// The sheet Connect opens on the card of the tasks that wait for an MCP server. Each phase shows
/// only its own status and buttons, on an opaque background so the card behind does not show
/// through; a sign-in that reaches the Mac closes it, and its host says so.
public struct McpSignInSheet: View {
    @Bindable var store: StoreOf<McpSignInFeature>

    public init(store: StoreOf<McpSignInFeature>) { self.store = store }

    public var body: some View {
        chrome
            .task { await store.send(.appeared).finish() }
    }

    #if os(iOS)
    private var chrome: some View {
        NavigationStack {
            VStack(spacing: 0) {
                ScrollView { content.padding(.horizontal, 20).padding(.vertical, 12) }
                    .scrollBounceBehavior(.basedOnSize)
                actions.padding(.horizontal, 20).padding(.bottom, 12)
            }
            .background(DawnBackground(glow: tint))
            .navigationTitle(title)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .principal) {
                    Text(title).font(.display(.headline)).foregroundStyle(Palette.ink).lineLimit(1)
                }
                ToolbarItem(placement: .cancellationAction) {
                    Button(role: .cancel) { store.send(.closeTapped) } label: { Image(systemName: "xmark") }
                        .accessibilityLabel("Close")
                }
            }
        }
        .presentationDetents([.medium, .large])
        .presentationBackground(Palette.night)
    }
    #else
    private var chrome: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(title).font(.display(.title2)).foregroundStyle(Palette.ink)
            content
            Spacer(minLength: 0)
            HStack {
                Button("Close") { store.send(.closeTapped) }.keyboardShortcut(.cancelAction)
                Spacer()
                actions
            }
        }
        .padding(24)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(DawnBackground(glow: tint))
    }
    #endif

    private var title: LocalizedStringKey { "Sign in to \(store.server)" }

    private var failed: Bool {
        if case .failed = store.phase { true } else { false }
    }

    private var tint: Color { failed ? Palette.ember : Palette.dawn }

    @ViewBuilder private var content: some View {
        switch store.phase {
        case .opening:
            waiting("Opening the sign-in…")
        case .sending:
            waiting("Signing in to \(store.server)…")
        case .reconnecting:
            waiting("The waiting tasks read their sign-in to \(store.server) again, and go on once it is connected.")
        default:
            VStack(alignment: .leading, spacing: 16) {
                status
                if store.asksToPaste, showsPaste { paste }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private func waiting(_ text: LocalizedStringKey) -> some View {
        VStack(spacing: 14) {
            ProgressView().controlSize(.large)
            Text(text).multilineTextAlignment(.center).foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 32)
    }

    private var showsPaste: Bool {
        switch store.phase {
        case .signingIn, .failed(.notAReturn): true
        default: false
        }
    }

    @ViewBuilder private var status: some View {
        switch store.phase {
        case .signingIn(.listening):
            step("safari", "Sign in to \(store.server) in the page that opened. Alondra brings the sign-in back to your Mac by itself, and the waiting tasks go on.")
        case .signingIn(.claudeOnThisMac):
            step("safari", "Sign in to \(store.server) in the browser. Claude on this Mac gets the sign-in by itself, and the waiting tasks go on.")
        case .signingIn(.paste):
            step("doc.on.clipboard", "Sign in to \(store.server) in the browser. Afterwards the browser shows a page that does not load: copy its address and paste it here.")
        case .claudeAi:
            step("link", "\(store.server) is a claude.ai connector. Connect it on claude.ai in the page that opened, then press Reconnect.")
        case .failed(let failure):
            step("exclamationmark.triangle.fill", message(failure))
        case .opening, .sending, .reconnecting:
            EmptyView()
        }
    }

    private func step(_ symbol: String, _ text: LocalizedStringKey) -> some View {
        HStack(alignment: .top, spacing: 14) {
            Image(systemName: symbol)
                .font(.title3)
                .foregroundStyle(tint)
                .frame(width: 44, height: 44)
                .background(tint.opacity(0.14), in: .circle)
                .accessibilityHidden(true)
            Text(text)
                .foregroundStyle(Palette.ink)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
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
            if store.phase != .signingIn(.paste), store.phase != .failed(.notAReturn) {
                Text("Did the page after signing in not load? Paste its address here.").font(.subheadline).foregroundStyle(.secondary)
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
                    .font(.footnote).foregroundStyle(Palette.ember)
            }
        }
    }

    private enum Choice {
        case send, connectAgain, openAgain, openClaudeAiAgain, reconnect
    }

    /// At most one prominent button and one plain one, both for the phase on screen.
    private var choices: (primary: Choice?, secondary: Choice?) {
        switch store.phase {
        case .claudeAi: (.reconnect, .openClaudeAiAgain)
        case .failed(.notAReturn): (.send, .connectAgain)
        case .failed: (.connectAgain, nil)
        case .signingIn: (store.asksToPaste ? .send : nil, .openAgain)
        case .opening, .sending, .reconnecting: (nil, nil)
        }
    }

    @ViewBuilder private var actions: some View {
        let choices = choices
        #if os(iOS)
        VStack(spacing: 6) {
            if let primary = choices.primary {
                button(primary)
                    .buttonStyle(.glassProminent)
                    .controlSize(.large)
                    .tint(tint)
            }
            if let secondary = choices.secondary {
                button(secondary).buttonStyle(.borderless).controlSize(.large).tint(tint)
            }
        }
        #else
        if let secondary = choices.secondary { button(secondary) }
        if let primary = choices.primary {
            button(primary).buttonStyle(.borderedProminent).keyboardShortcut(.defaultAction)
        }
        #endif
    }

    private func button(_ choice: Choice) -> some View {
        Button {
            switch choice {
            case .send: store.send(.sendPastedTapped)
            case .connectAgain, .openAgain, .openClaudeAiAgain: store.send(.connectAgainTapped)
            case .reconnect: store.send(.reconnectTapped)
            }
        } label: {
            Text(label(choice)).frame(maxWidth: .infinity)
        }
        .disabled(choice == .send && store.pasted.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
        #if os(macOS)
        .fixedSize()
        #endif
    }

    private func label(_ choice: Choice) -> LocalizedStringKey {
        switch choice {
        case .send: "Send"
        case .connectAgain: "Connect again"
        case .openAgain: "Open the sign-in again"
        case .openClaudeAiAgain: "Open claude.ai again"
        case .reconnect: "Reconnect"
        }
    }
}
