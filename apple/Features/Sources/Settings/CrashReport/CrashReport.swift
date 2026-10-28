import Foundation

/// A crash report the Mac built and has not sent: what the preview shows of it, read from the
/// daemon's answer to `POST /reports`.
public struct CrashReport: Equatable, Sendable {
    public struct File: Equatable, Sendable, Identifiable {
        public var name: String
        /// Its size, or nil when it holds nothing.
        public var bytes: Int?
        /// Why it holds nothing.
        public var empty: String?
        public var id: String { name }
        /// The files of the package; an empty section is listed but has nothing to open.
        public var openable: Bool { bytes != nil || name == "manifest.json" }

        public init(name: String, bytes: Int?, empty: String? = nil) {
            self.name = name; self.bytes = bytes; self.empty = empty
        }
    }

    public struct Hit: Equatable, Sendable {
        public var section: String
        public var line: Int
        public var what: String

        public init(section: String, line: Int, what: String) {
            self.section = section; self.line = line; self.what = what
        }
    }

    public enum LeftOut: Equatable, Sendable {
        case omitted(String, count: Int)
        case trimmed(section: String, lines: Int)
    }

    public var id: String
    public var compressedBytes: Int
    public var hours: Int
    /// Where it goes once the user says yes; nil when this Mac knows no report repository.
    public var repo: String?
    public var passed: Bool
    public var hits: [Hit]
    public var files: [File]
    public var leftOut: [LeftOut]

    public init(id: String, compressedBytes: Int, hours: Int = 72, repo: String?, passed: Bool, hits: [Hit] = [],
                files: [File], leftOut: [LeftOut] = []) {
        self.id = id; self.compressedBytes = compressedBytes; self.hours = hours; self.repo = repo
        self.passed = passed; self.hits = hits; self.files = files; self.leftOut = leftOut
    }

    /// Whether the one question can be asked: the final check passed and there is somewhere to send it.
    public var sendable: Bool { passed && repo != nil }

    public static func isID(_ id: String) -> Bool {
        let parts = id.split(separator: "-", omittingEmptySubsequences: false)
        return parts.count == 3 && parts[0] == "r"
            && parts[1].count == 8 && parts[1].allSatisfy { ("0"..."9").contains($0) }
            && parts[2].count == 8 && parts[2].allSatisfy(\.isHexDigit)
    }

    public init?(daemonReply data: Data) {
        guard let answer = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
              let id = answer["id"] as? String, Self.isID(id) else { return nil }
        let manifest = answer["manifest"] as? [String: Any] ?? [:]
        var files = [File(name: "manifest.json", bytes: nil)]
        if let bytes = (manifest["description"] as? [String: Any])?["bytes"] as? Int {
            files.append(File(name: "description.txt", bytes: bytes))
        }
        for section in manifest["sections"] as? [[String: Any]] ?? [] {
            guard let name = section["name"] as? String else { return nil }
            if let bytes = section["bytes"] as? Int, section["sha256"] is String {
                files.append(File(name: name, bytes: bytes))
            } else {
                files.append(File(name: name, bytes: nil, empty: section["empty"] as? String ?? ""))
            }
        }
        var leftOut: [LeftOut] = []
        for (key, count) in (manifest["omitted"] as? [String: Any] ?? [:]).sorted(by: { $0.key < $1.key }) {
            if let count = count as? Int, count > 0 { leftOut.append(.omitted(key, count: count)) }
        }
        for trim in manifest["trimmed"] as? [[String: Any]] ?? [] {
            leftOut.append(.trimmed(section: trim["section"] as? String ?? "", lines: trim["lines"] as? Int ?? 0))
        }
        let check = answer["check"] as? [String: Any] ?? [:]
        let hits = (check["hits"] as? [[String: Any]] ?? []).map {
            Hit(section: $0["section"] as? String ?? "", line: $0["line"] as? Int ?? 0, what: $0["what"] as? String ?? "")
        }
        let repo = (answer["repo"] as? String).flatMap { repo -> String? in
            let parts = repo.split(separator: "/", omittingEmptySubsequences: false)
            return parts.count == 2 && parts.allSatisfy { !$0.isEmpty } ? repo : nil
        }
        self.init(id: id, compressedBytes: answer["compressed_bytes"] as? Int ?? 0,
                  hours: (manifest["window"] as? [String: Any])?["hours"] as? Int ?? 72, repo: repo,
                  passed: check["passed"] as? Bool == true, hits: hits, files: files, leftOut: leftOut)
    }
}
