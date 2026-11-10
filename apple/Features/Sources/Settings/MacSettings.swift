import AlondraContract
import AlondraTransport
import Foundation

/// A Mac's own settings as its daemon serves them on `GET /settings`.
public struct MacSettings: Equatable, Sendable, Decodable {
    public struct Candidate: Equatable, Sendable, Decodable, Identifiable {
        public let path: String
        public let repos: Int?
        public var id: String { path }
        public init(path: String, repos: Int?) { self.path = path; self.repos = repos }
    }

    public struct Relay: Equatable, Sendable, Decodable {
        public let on: Bool
        public init(on: Bool) { self.on = on }
    }

    public let projectsRoot: String
    public let candidates: [Candidate]
    public let relay: Relay
    /// Absent from a daemon older than the diagnosis or one whose iroh endpoint is off.
    public let directConnection: DirectConnection?

    public init(projectsRoot: String, candidates: [Candidate], relay: Relay, directConnection: DirectConnection? = nil) {
        self.projectsRoot = projectsRoot; self.candidates = candidates; self.relay = relay
        self.directConnection = directConnection
    }
}

/// One folder of the daemon's browser, `GET /settings/folders`, with what it holds.
public struct FolderListing: Equatable, Sendable, Decodable {
    public struct Folder: Equatable, Sendable, Decodable, Identifiable {
        public let name: String
        public let path: String
        public let repo: Bool
        public let repos: Int
        public var id: String { path }
        public init(name: String, path: String, repo: Bool, repos: Int) {
            self.name = name; self.path = path; self.repo = repo; self.repos = repos
        }
    }

    public let path: String
    public let parent: String?
    public let folders: [Folder]
    public let truncated: Bool

    public init(path: String, parent: String?, folders: [Folder], truncated: Bool = false) {
        self.path = path; self.parent = parent; self.folders = folders; self.truncated = truncated
    }

    /// Repos directly inside, as the daemon counts them for a candidate.
    public var repos: Int { folders.filter(\.repo).count }
}

/// Reads and changes one paired Mac's settings, whichever path reaches it.
public struct MacSettingsClient: Sendable {
    public var load: @Sendable () async throws -> MacSettings
    public var save: @Sendable (String) async throws -> MacSettings
    public var folders: @Sendable (String?) async throws -> FolderListing
    /// macOS's own folder panel, only when the settings are this Mac's.
    public var pickLocalFolder: (@Sendable () async -> String?)?
    public var problem: @Sendable (any Error) -> ConnectionProblem
    /// How often the open screen reads the settings again, so what the Mac diagnoses by itself
    /// (its direct connection, macOS's Local Network permission) stays current.
    public var refresh: Duration?
    /// The Mac's live connection state, the one every other screen reads: the current one first, then each change.
    public var statuses: @Sendable () -> AsyncStream<MacStatus>

    public init(load: @escaping @Sendable () async throws -> MacSettings,
                save: @escaping @Sendable (String) async throws -> MacSettings,
                folders: @escaping @Sendable (String?) async throws -> FolderListing,
                pickLocalFolder: (@Sendable () async -> String?)? = nil,
                problem: @escaping @Sendable (any Error) -> ConnectionProblem = { _ in .unexpected },
                refresh: Duration? = nil,
                statuses: @escaping @Sendable () -> AsyncStream<MacStatus> = { AsyncStream { $0.finish() } }) {
        self.load = load; self.save = save; self.folders = folders
        self.pickLocalFolder = pickLocalFolder; self.problem = problem; self.refresh = refresh; self.statuses = statuses
    }

    /// Reads and writes through whichever session answers for the Mac at that moment, so a session
    /// replaced while the screen is open is never the one it keeps asking.
    public static func following(_ first: MacSettingsClient,
                                 resolve: @escaping @Sendable () async -> MacSettingsClient?) -> MacSettingsClient {
        @Sendable func live() async throws -> MacSettingsClient {
            guard let client = await resolve() else { throw CancellationError() }
            return client
        }
        return MacSettingsClient(
            load: { try await live().load() },
            save: { try await live().save($0) },
            folders: { try await live().folders($0) },
            pickLocalFolder: first.pickLocalFolder,
            problem: first.problem,
            refresh: first.refresh,
            statuses: {
                AsyncStream { continuation in
                    let following = Task {
                        while !Task.isCancelled, let client = await resolve() {
                            for await status in client.statuses() { continuation.yield(status) }
                            try? await Task.sleep(for: .seconds(1))
                        }
                        continuation.finish()
                    }
                    continuation.onTermination = { _ in following.cancel() }
                }
            })
    }

    /// The daemon's translated reason for refusing a folder, or a generic one.
    public static func refusal(_ error: any Error) -> String {
        if case .server(422, _, let detail?, _) = error as? APIError, !detail.isEmpty { return detail }
        return String(localized: "Your Mac didn’t get the new folder. Try again.")
    }
}

public enum FolderPath {
    /// `/Users/ana/Projects` as `~/Projects`, the way the Mac's owner reads it.
    public static func display(_ path: String) -> String {
        let parts = path.split(separator: "/", omittingEmptySubsequences: false)
        guard parts.count >= 3, parts[0].isEmpty, parts[1] == "Users", !parts[2].isEmpty else { return path }
        return (["~"] + parts.dropFirst(3)).joined(separator: "/")
    }
}
