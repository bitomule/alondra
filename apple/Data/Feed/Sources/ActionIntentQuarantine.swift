import AlondraContract
import AlondraTransport
import Foundation
import OSLog
import Synchronization

/// What a store set aside held: the answers that may never have reached the Mac. They are never
/// sent again, so the person is told and answers again whatever still waits.
public struct SetAsideAnswers: Codable, Equatable, Sendable {
    /// The cards those answers took out of the queue; one still waiting is back in it.
    public let cards: [String]
    /// Answers that resolved no card, such as a request for new work.
    public let otherActions: Int
    /// False when part or all of the store could not be read, so there may be more than these.
    public let complete: Bool

    public init(cards: [String], otherActions: Int, complete: Bool) {
        self.cards = cards; self.otherActions = otherActions; self.complete = complete
    }

    init(requests: [ActionRequest], complete: Bool) {
        let cards = requests.compactMap(\.answeredCard)
        self.init(cards: Array(Set(cards)).sorted(), otherActions: requests.count - cards.count, complete: complete)
    }

    public var lostNothing: Bool { complete && cards.isEmpty && otherActions == 0 }
}

/// The set-aside answers this device has not shown yet. Dismissing it is remembered on disk, so the
/// notice survives a relaunch until the person has seen it, and only then.
public final class SetAsideNotice: Sendable {
    private let state: Mutex<(answers: SetAsideAnswers?, folder: URL?)>

    public init() { state = Mutex((nil, nil)) }
    init(answers: SetAsideAnswers, folder: URL) { state = Mutex((answers, folder)) }

    public var current: SetAsideAnswers? { state.withLock(\.answers) }

    public func dismiss() {
        let folder = state.withLock { state -> URL? in
            defer { state = (nil, nil) }
            return state.folder
        }
        guard let folder else { return }
        FileManager.default.createFile(atPath: folder.appending(path: ActionIntentQuarantine.seen).path, contents: Data())
    }
}

/// Unreadable stores are moved here instead of deleted, so one can still be examined, and only the
/// newest few are kept.
enum ActionIntentQuarantine {
    static let folderName = "set-aside"
    static let kept = 3
    static let report = "answers.json"
    static let seen = "seen"

    static func root(_ directory: URL) -> URL { directory.appending(path: folderName) }

    static func newFolder(in directory: URL, name: String, now: Date) -> URL {
        let stamp = String(format: "%016llx", UInt64(max(0, now.timeIntervalSince1970 * 1000)))
        return root(directory).appending(path: stamp + "-" + name.prefix(16) + "-" + UUID().uuidString.prefix(8))
    }

    static func record(_ answers: SetAsideAnswers, in folder: URL) throws {
        try JSONEncoder().encode(answers).write(to: folder.appending(path: report),
                                                options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
    }

    /// Drops the oldest folders past `kept`. Folder names start with a hex timestamp, so they sort in time.
    static func prune(_ directory: URL) {
        let root = root(directory)
        guard let folders = try? FileManager.default.contentsOfDirectory(atPath: root.path).sorted() else { return }
        for folder in folders.dropLast(kept) { try? FileManager.default.removeItem(at: root.appending(path: folder)) }
    }

    /// The newest set-aside answers of this store's identity the person has not dismissed yet.
    static func unseen(in directory: URL, name: String) -> SetAsideNotice? {
        let root = root(directory)
        guard let folders = try? FileManager.default.contentsOfDirectory(atPath: root.path) else { return nil }
        for folder in folders.sorted().reversed() where folder.dropFirst(17).hasPrefix(name.prefix(16)) {
            let url = root.appending(path: folder)
            guard !FileManager.default.fileExists(atPath: url.appending(path: seen).path),
                  let data = try? Data(contentsOf: url.appending(path: report)),
                  let answers = try? JSONDecoder().decode(SetAsideAnswers.self, from: data) else { continue }
            return SetAsideNotice(answers: answers, folder: url)
        }
        return nil
    }
}

/// A store that opened, or none, and what was set aside on the way.
public struct OpenedActionIntents: Sendable {
    public let store: ViewActionIntentStore?
    public let setAside: SetAsideNotice
}

extension ViewActionIntentStore {
    private static let log = Logger(subsystem: "com.bitomule.alondra", category: "action-intents")

    /// Opening the store never stops the connection. A store that cannot be read is set aside with
    /// what it still shows of its answers, and a new one starts above every sequence the old one
    /// may have used. When even that fails, answers go without a durable record.
    public static func openSettingAsideUnreadable(directory: URL, keyMaterial: Data, scope: ActionIntentScope,
                                                  now: @Sendable () -> Date = Date.init) async -> OpenedActionIntents {
        do {
            let store = try await ViewActionIntentStore(directory: directory, keyMaterial: keyMaterial, scope: scope)
            return OpenedActionIntents(store: store, setAside: unseenNotice(directory: directory, keyMaterial: keyMaterial, scope: scope))
        } catch ViewActionIntentError.unreadableStore, ViewActionIntentError.invalidRecord {
            log.error("Action intent store unreadable; setting it aside")
        } catch {
            log.error("Action intent store unavailable: \(String(describing: error), privacy: .public)")
            return OpenedActionIntents(store: nil, setAside: SetAsideNotice())
        }
        let started = now()
        let floor = sequenceFloor(at: started)
        var notice = SetAsideNotice()
        do {
            let unreadable = try ActionIntentDatabase(directory: directory, keyMaterial: keyMaterial, scope: scope, legacyLimit: maxCompleted)
            let folder = ActionIntentQuarantine.newFolder(in: directory, name: unreadable.name, now: started)
            let answers = try await unreadable.setAside(into: folder)
            if !answers.lostNothing {
                try ActionIntentQuarantine.record(answers, in: folder)
                notice = SetAsideNotice(answers: answers, folder: folder)
            }
            ActionIntentQuarantine.prune(directory)
        } catch {
            log.error("Could not set the unreadable action intent store aside: \(String(describing: error), privacy: .public)")
            return OpenedActionIntents(store: nil, setAside: notice)
        }
        do {
            let store = try await ViewActionIntentStore(directory: directory, keyMaterial: keyMaterial, scope: scope,
                completedLimit: maxCompleted, sequenceFloor: floor)
            return OpenedActionIntents(store: store, setAside: notice)
        } catch {
            log.error("A new action intent store failed to open: \(String(describing: error), privacy: .public)")
            return OpenedActionIntents(store: nil, setAside: notice)
        }
    }

    /// Milliseconds since 1970: the old store issued at most one sequence per answer, never one per
    /// millisecond since its own floor, so the new one starts past all of them.
    static func sequenceFloor(at date: Date) -> UInt64 {
        min(ActionOperationID.maximumCounter / 2, max(1, UInt64(max(0, date.timeIntervalSince1970 * 1000))))
    }

    private static func unseenNotice(directory: URL, keyMaterial: Data, scope: ActionIntentScope) -> SetAsideNotice {
        guard let name = try? ActionIntentDatabase(directory: directory, keyMaterial: keyMaterial, scope: scope, legacyLimit: maxCompleted).name
        else { return SetAsideNotice() }
        return ActionIntentQuarantine.unseen(in: directory, name: name) ?? SetAsideNotice()
    }
}
