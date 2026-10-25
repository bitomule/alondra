/// Marks what changed inside a line, as GitHub does: each run of removed lines is paired, line by
/// line, with the run of added lines right after it, and the words between their common start and
/// common end are the change. Lines that share nothing, or are too long to compare, stay unmarked.
public enum InlineChanges {
    public static let longestComparedLine = 400

    public static func marking(_ lines: [DiffLine]) -> [DiffLine] {
        var out = lines
        var index = 0
        while index < out.count {
            guard out[index].kind == .removed else { index += 1; continue }
            let removedStart = index
            while index < out.count, out[index].kind == .removed { index += 1 }
            let addedStart = index
            while index < out.count, out[index].kind == .added { index += 1 }
            for offset in 0..<min(addedStart - removedStart, index - addedStart) {
                let (removed, added) = (removedStart + offset, addedStart + offset)
                guard let (before, after) = changed(out[removed].text, out[added].text) else { continue }
                out[removed].changed = before
                out[added].changed = after
            }
        }
        return out
    }

    /// The changed characters of each line, or nil when the lines share no word at either end or
    /// one of them is too long.
    public static func changed(_ before: String, _ after: String) -> (Range<Int>, Range<Int>)? {
        guard before != after, before.count <= longestComparedLine, after.count <= longestComparedLine else { return nil }
        let (a, b) = (words(before), words(after))
        var prefix = 0
        while prefix < a.count, prefix < b.count, a[prefix] == b[prefix] { prefix += 1 }
        var suffix = 0
        while suffix < a.count - prefix, suffix < b.count - prefix, a[a.count - 1 - suffix] == b[b.count - 1 - suffix] { suffix += 1 }
        let shared = a[..<prefix].joined().trimmingCharacters(in: .whitespaces).count
            + a[(a.count - suffix)...].joined().trimmingCharacters(in: .whitespaces).count
        guard shared > 0 else { return nil }
        func range(_ tokens: [String]) -> Range<Int> {
            let start = tokens[..<prefix].reduce(0) { $0 + $1.count }
            let end = tokens[..<(tokens.count - suffix)].reduce(0) { $0 + $1.count }
            return start..<end
        }
        return (range(a), range(b))
    }

    /// Runs of letters, digits and `_` are one word; every other character is a word of its own.
    static func words(_ text: String) -> [String] {
        var out: [String] = []
        var word = ""
        for character in text {
            if character.isLetter || character.isNumber || character == "_" {
                word.append(character)
            } else {
                if !word.isEmpty { out.append(word); word = "" }
                out.append(String(character))
            }
        }
        if !word.isEmpty { out.append(word) }
        return out
    }
}
