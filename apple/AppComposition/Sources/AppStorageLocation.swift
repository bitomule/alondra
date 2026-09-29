import Foundation

public enum AppStorageLocation {
    /// Pairings and push preferences stay in the folder earlier builds wrote them to, so an
    /// update keeps the phone paired.
    public static func applicationSupport(bundleID: String) -> URL {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent(bundleID, isDirectory: true)
            .appendingPathComponent("CommandJournal", isDirectory: true)
    }
}
