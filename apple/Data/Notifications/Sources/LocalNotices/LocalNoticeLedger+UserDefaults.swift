import Foundation

extension LocalNoticeLedger {
    private static let key = "localNoticeLedger"

    public static func load(from defaults: UserDefaults) -> LocalNoticeLedger {
        defaults.data(forKey: key).flatMap { try? JSONDecoder().decode(LocalNoticeLedger.self, from: $0) } ?? LocalNoticeLedger()
    }

    public static func saver(to defaults: UserDefaults) -> @Sendable (LocalNoticeLedger) -> Void {
        nonisolated(unsafe) let defaults = defaults
        return { ledger in
            if let data = try? JSONEncoder().encode(ledger) { defaults.set(data, forKey: key) }
        }
    }
}
