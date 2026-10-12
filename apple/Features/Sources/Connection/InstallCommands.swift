import SwiftUI
#if canImport(UIKit)
import UIKit
#else
import AppKit
#endif

/// The two commands that install Alondra on a Mac and start it, with a button that copies both.
public struct InstallCommands: View {
    public static let commands = ["brew install bitomule/tap/alondra", "alondra"]
    @State private var copied = false

    public init() {}

    public var body: some View {
        HStack(alignment: .top) {
            VStack(alignment: .leading, spacing: 4) {
                ForEach(Self.commands, id: \.self) { Text(verbatim: $0).lineLimit(1).minimumScaleFactor(0.6) }
            }
            .monospaced()
            .textSelection(.enabled)
            Spacer(minLength: 12)
            Button(copied ? "Copied" : "Copy", systemImage: copied ? "checkmark" : "doc.on.doc") {
                Self.copy(Self.commands.joined(separator: "\n"))
                copied = true
            }
            .labelStyle(.titleAndIcon)
            .accessibilityIdentifier("install.copy")
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.fill.tertiary, in: .rect(cornerRadius: 10))
        .sensoryFeedback(.success, trigger: copied)
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
