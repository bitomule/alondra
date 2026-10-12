import AppKit

/// macOS's folder panel, for this Mac's own projects folder.
@MainActor enum LocalFolderPanel {
    static func choose() async -> String? {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.canCreateDirectories = true
        panel.prompt = String(localized: "Use This Folder")
        panel.directoryURL = FileManager.default.homeDirectoryForCurrentUser
        guard await panel.begin() == .OK else { return nil }
        return panel.url?.path(percentEncoded: false)
    }
}
