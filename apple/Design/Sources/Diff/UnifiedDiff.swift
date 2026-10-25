import Foundation

/// Reads `gh pr diff` output: files with their status, hunks, and each line with its old and new numbers.
public enum UnifiedDiff {
    public static func parse(_ text: String) -> [DiffFile] {
        var files: [DiffFile] = []
        var path: String?
        var oldPath: String?
        var status = DiffFile.Status.modified
        var binary = false
        var lines: [DiffLine] = []
        var (old, new) = (0, 0)
        var inHunk = false

        func close() {
            if let path {
                files.append(DiffFile(path: path, oldPath: oldPath, status: status, isBinary: binary,
                                      lines: InlineChanges.marking(lines)))
            }
            (path, oldPath, status, binary, lines, inHunk) = (nil, nil, .modified, false, [], false)
        }

        for raw in text.split(separator: "\n", omittingEmptySubsequences: false).map(String.init) {
            if raw.hasPrefix("diff --git ") {
                close()
                path = raw.components(separatedBy: " b/").last
                continue
            }
            guard let current = path else { continue }
            if !inHunk {
                if raw.hasPrefix("new file mode") { status = .added; continue }
                if raw.hasPrefix("deleted file mode") { status = .deleted; continue }
                if raw.hasPrefix("rename from ") {
                    (oldPath, status) = (String(raw.dropFirst(12)), .renamed)
                    continue
                }
                if raw.hasPrefix("rename to ") { path = String(raw.dropFirst(10)); continue }
                if raw.hasPrefix("Binary files ") || raw == "GIT binary patch" { binary = true; continue }
                if raw.hasPrefix("--- ") { continue }
                if raw.hasPrefix("+++ ") {
                    let target = String(raw.dropFirst(4))
                    if target != "/dev/null" { path = target.hasPrefix("b/") ? String(target.dropFirst(2)) : target }
                    continue
                }
            }
            if raw.hasPrefix("@@") {
                let (o, n) = hunkStarts(raw)
                (old, new, inHunk) = (o, n, true)
                lines.append(DiffLine(id: "\(current)#\(lines.count)", kind: .hunk, text: raw, old: nil, new: nil, anchor: nil))
                continue
            }
            guard inHunk, let first = raw.first else { continue }
            let body = String(raw.dropFirst())
            let id = "\(current)#\(lines.count)"
            switch first {
            case "+":
                lines.append(DiffLine(id: id, kind: .added, text: body, old: nil, new: new, anchor: "\(current):\(new)"))
                new += 1
            case "-":
                lines.append(DiffLine(id: id, kind: .removed, text: body, old: old, new: nil, anchor: "\(current):\(old)"))
                old += 1
            case " ":
                lines.append(DiffLine(id: id, kind: .context, text: body, old: old, new: new, anchor: "\(current):\(new)"))
                (old, new) = (old + 1, new + 1)
            case "\\":
                lines.append(DiffLine(id: id, kind: .note, text: body.trimmingCharacters(in: .whitespaces), old: nil, new: nil, anchor: nil))
            default:
                continue
            }
        }
        close()
        return files
    }

    /// `@@ -12,7 +12,8 @@` → (12, 12)
    static func hunkStarts(_ header: String) -> (Int, Int) {
        let parts = header.split(separator: " ")
        func start(_ prefix: Character) -> Int {
            guard let part = parts.first(where: { $0.first == prefix }) else { return 1 }
            return Int(part.dropFirst().split(separator: ",").first ?? "1") ?? 1
        }
        return (start("-"), start("+"))
    }
}
