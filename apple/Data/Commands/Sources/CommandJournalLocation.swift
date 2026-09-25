import Foundation

public enum CommandJournalLocation {
    /// Each app owns its journal. An extension must opt in to a shared container explicitly.
    public static func applicationSupport(bundleID: String) -> URL {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent(bundleID, isDirectory: true)
            .appendingPathComponent("CommandJournal", isDirectory: true)
    }
}
