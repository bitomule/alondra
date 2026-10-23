import AppKit

/// Copies a command and brings Terminal up for the person to paste it. The App Store build is
/// sandboxed: it may not tell Terminal what to run (an Apple-event exception for Terminal is
/// refused in review) and the scripts it writes are quarantined, so pasting is the step that works.
@MainActor enum ClaudeTerminal {
    static func open(_ command: String) async -> Bool {
        guard let terminal = NSWorkspace.shared.urlForApplication(withBundleIdentifier: "com.apple.Terminal") else { return false }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(command, forType: .string)
        return (try? await NSWorkspace.shared.openApplication(at: terminal, configuration: NSWorkspace.OpenConfiguration())) != nil
    }
}
