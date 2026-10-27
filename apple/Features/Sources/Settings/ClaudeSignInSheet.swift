import SwiftUI
#if canImport(UIKit)
import UIKit
#else
import AppKit
#endif

/// The selected provider's native login, completed in Terminal on the daemon's Mac.
public struct ClaudeSignInSheet: View {
    public let signIn: ServerSettingsFeature.ClaudeSignIn
    public let onDone: () -> Void
    @State private var copied = false

    public init(signIn: ServerSettingsFeature.ClaudeSignIn, onDone: @escaping () -> Void) {
        self.signIn = signIn
        self.onDone = onDone
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text(title).font(.title2.bold())
            if signIn.terminalOpen {
                Text("Terminal is open and the command is copied. Paste it with ⌘V and press Return to sign in to \(signIn.provider.displayName):")
            } else {
                if let mac = signIn.macName {
                    Text("Run this in Terminal on \(mac) to sign in to \(signIn.provider.displayName):")
                } else {
                    Text("Run this in Terminal on the Mac to sign in to \(signIn.provider.displayName):")
                }
            }
            HStack(alignment: .top) {
                Text(verbatim: signIn.command)
                    .monospaced()
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
                Spacer(minLength: 12)
                Button(copied ? LocalizedStringKey("Copied") : LocalizedStringKey("Copy"), systemImage: copied ? "checkmark" : "doc.on.doc") {
                    Self.copy(signIn.command)
                    copied = true
                }
                .labelStyle(.titleAndIcon)
                .accessibilityIdentifier("accountSignIn.copy")
            }
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(.fill.tertiary, in: .rect(cornerRadius: 10))
            Spacer(minLength: 0)
            HStack {
                Spacer()
                Button("Done", action: onDone)
                    .buttonStyle(.borderedProminent)
                    .keyboardShortcut(.defaultAction)
                    .accessibilityIdentifier("accountSignIn.done")
            }
        }
        .padding(24)
        .frame(maxWidth: .infinity, alignment: .leading)
        .sensoryFeedback(.success, trigger: copied)
        #if os(iOS)
        .presentationDetents([.medium])
        #else
        .frame(minWidth: 460, minHeight: 240)
        #endif
    }

    private var title: LocalizedStringKey {
        signIn.terminalOpen ? "Finish in Terminal" : "Finish on the Mac"
    }

    private static func copy(_ text: String) {
        #if canImport(UIKit)
        UIPasteboard.general.string = text
        #else
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
        #endif
    }
}
