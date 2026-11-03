import AlondraContract
import SwiftUI

/// How many files a diff changes, its lines in and out, and its files by what happened to them.
public struct DiffSummary: Equatable, Sendable {
    public let files: Int
    public let additions: Int
    public let deletions: Int
    public let added: Int
    public let modified: Int
    public let deleted: Int
    public let renamed: Int

    public init(_ files: [DiffFile]) {
        self.files = files.count
        additions = files.reduce(0) { $0 + $1.additions }
        deletions = files.reduce(0) { $0 + $1.deletions }
        added = files.filter { $0.status == .added }.count
        modified = files.filter { $0.status == .modified }.count
        deleted = files.filter { $0.status == .deleted }.count
        renamed = files.filter { $0.status == .renamed }.count
    }

    public var fileCount: Text { Text("\(files) files", tableName: "AlondraDesign", bundle: #bundle) }

    /// «1 new · 2 modified · 1 deleted», leaving out what did not happen.
    public var breakdown: [Text] {
        [
            added > 0 ? Text("\(added) new", tableName: "AlondraDesign", bundle: #bundle) : nil,
            modified > 0 ? Text("\(modified) modified", tableName: "AlondraDesign", bundle: #bundle) : nil,
            deleted > 0 ? Text("\(deleted) deleted", tableName: "AlondraDesign", bundle: #bundle) : nil,
            renamed > 0 ? Text("\(renamed) renamed", tableName: "AlondraDesign", bundle: #bundle) : nil,
        ].compactMap(\.self)
    }

    /// «9 of 12 viewed», counting the files GitHub has ticked as viewed among these.
    public static func viewedCount(_ files: [DiffFile], marks: [String: FileViewed]?) -> Text? {
        guard let marks else { return nil }
        let viewed = files.filter { marks[$0.id] == .viewed }.count
        return Text("\(viewed) of \(files.count) viewed", tableName: "AlondraDesign", bundle: #bundle)
    }

    /// «4 files · +120 −30».
    public var line: Text { Text("\(fileCount) · +\(additions) −\(deletions)", tableName: "AlondraDesign", bundle: #bundle) }
}
