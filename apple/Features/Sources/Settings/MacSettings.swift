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

    public init(projectsRoot: String, candidates: [Candidate], relay: Relay) {
        self.projectsRoot = projectsRoot; self.candidates = candidates; self.relay = relay
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

    public init(load: @escaping @Sendable () async throws -> MacSettings,
                save: @escaping @Sendable (String) async throws -> MacSettings,
                folders: @escaping @Sendable (String?) async throws -> FolderListing,
                pickLocalFolder: (@Sendable () async -> String?)? = nil,
                problem: @escaping @Sendable (any Error) -> ConnectionProblem = { _ in .unexpected }) {
        self.load = load; self.save = save; self.folders = folders
        self.pickLocalFolder = pickLocalFolder; self.problem = problem
    }

    /// The daemon's translated reason for refusing a folder, or a generic one.
    public static func refusal(_ error: any Error) -> String {
        if case .server(422, _, let detail?, _) = error as? APIError, !detail.isEmpty { return detail }
        return String(localized: "Your Mac didn’t get the new folder. Try again.")
    }
}

public enum FolderPath {
    /// `/Users/david/Projects` as `~/Projects`, the way the Mac's owner reads it.
    public static func display(_ path: String) -> String {
        let parts = path.split(separator: "/", omittingEmptySubsequences: false)
        guard parts.count >= 3, parts[0].isEmpty, parts[1] == "Users", !parts[2].isEmpty else { return path }
        return (["~"] + parts.dropFirst(3)).joined(separator: "/")
    }
}
