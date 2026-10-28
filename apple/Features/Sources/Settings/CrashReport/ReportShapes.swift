import Foundation

/// What a value may look like to enter a crash report as it is, and the pseudonym of everything
/// else: the rule of `alondra report` (`crates/alondra/src/report/redact.rs`), held to the same
/// vectors (`fixtures/report/shape-vectors.json`).
public enum ReportShapes {
    public enum Kind: String, CaseIterable, Sendable {
        case task, request, node, path, repo, account, device, value
    }

    enum Part: Equatable {
        case token(String)
        case gap(String)
    }

    private static let units: Set<String> = [
        "", "ns", "µs", "us", "ms", "s", "m", "h", "d", "w", "B", "KB", "MB", "GB", "TB", "KiB", "MiB", "GiB", "TiB", "k", "%", "x", "×",
    ]

    private static func digit(_ c: Character) -> Bool { ("0"..."9").contains(c) }
    private static func digitByte(_ b: UInt8) -> Bool { (48...57).contains(b) }
    private static func lowerHex(_ c: Character) -> Bool { digit(c) || ("a"..."f").contains(c) }
    private static func anyHex(_ c: Character) -> Bool { lowerHex(c) || ("A"..."F").contains(c) }

    /// `12`, `0.8`, `250ms`, `1.5s`, `41%`, `3GiB`.
    public static func isNumber(_ token: String) -> Bool {
        let digits = token.drop { $0 == "-" }
        let unitAt = digits.firstIndex { !(digit($0) || $0 == ".") } ?? digits.endIndex
        let number = digits[..<unitAt]
        guard let first = number.first, digit(first), number.filter({ $0 == "." }).count <= 1, number.last != "." else { return false }
        return units.contains(String(digits[unitAt...]))
    }

    /// `1m30s`.
    static func isCompoundDuration(_ token: String) -> Bool {
        var rest = Substring(token)
        var parts = 0
        while !rest.isEmpty {
            let end = rest.firstIndex { !(digit($0) || $0 == ".") } ?? rest.endIndex
            guard end > rest.startIndex else { return false }
            let after = rest[end...]
            guard let unit = ["ms", "µs", "us", "ns", "h", "m", "s", "d"].first(where: { after.hasPrefix($0) }) else { return false }
            rest = after.dropFirst(unit.count)
            parts += 1
        }
        return parts > 1
    }

    /// `2026-10-25T05:43:41.854Z`, with any offset.
    public static func isISOTime(_ token: String) -> Bool {
        let b = Array(token.utf8)
        guard b.count >= 19, b[0..<4].allSatisfy(digitByte), b[4] == 45, b[7] == 45, b[10] == 84, b[13] == 58 else { return false }
        return b[11...].allSatisfy { digitByte($0) || [58, 46, 90, 43, 45].contains($0) }
    }

    /// `0.3.5`, `v2.1.30`, `1.2.3-beta.1`: two or three numbers, so an IPv4 address never is one.
    public static func isVersion(_ token: String) -> Bool {
        let bare = token.hasPrefix("v") ? token.dropFirst() : Substring(token)
        let split = bare.firstIndex { $0 == "-" || $0 == "+" }
        let core = split.map { bare[..<$0] } ?? bare
        let suffix = split.map { bare[bare.index(after: $0)...] } ?? ""
        let parts = core.split(separator: ".", omittingEmptySubsequences: false)
        return (2...3).contains(parts.count)
            && parts.allSatisfy { !$0.isEmpty && $0.allSatisfy(digit) }
            && suffix.allSatisfy { ($0.isASCII && ($0.isLetter || $0.isNumber)) || $0 == "." }
            && (parts.count == 3 || !suffix.isEmpty || core.utf8.count <= 6)
    }

    /// Alondra's random ids say nothing about the user: `sdk-<16 hex>`, `run-<hex>`, `o-<hex>`.
    public static func isRandomID(_ token: String) -> Bool {
        func hex(_ rest: Substring, _ least: Int) -> Bool { rest.count >= least && rest.allSatisfy(lowerHex) }
        if token.hasPrefix("sdk-") { return token.dropFirst(4).count == 16 && hex(token.dropFirst(4), 16) }
        if token.hasPrefix("run-") { return hex(token.dropFirst(4), 6) }
        if token.hasPrefix("o-") { return hex(token.dropFirst(2), 6) }
        return false
    }

    private static func slug(_ token: String, after prefix: String) -> Bool {
        guard token.hasPrefix(prefix) else { return false }
        let rest = token.dropFirst(prefix.count)
        return !rest.isEmpty && rest.allSatisfy { ("a"..."z").contains($0) || digit($0) || $0 == "-" }
    }

    private static func isUUID(_ token: String) -> Bool {
        let groups = token.split(separator: "-", omittingEmptySubsequences: false)
        return groups.count == 5 && zip(groups, [8, 4, 4, 4, 12]).allSatisfy { $0.count == $1 && $0.allSatisfy(anyHex) }
    }

    private static func isPath(_ token: String) -> Bool {
        (token.hasPrefix("/") && token.utf8.count > 1) || token.hasPrefix("~/")
    }

    private static func isURL<S: StringProtocol>(_ token: S) -> Bool {
        token.hasPrefix("http://") || token.hasPrefix("https://")
    }

    static func kindOfShape(_ token: String) -> Kind? {
        if slug(token, after: "t-") { return .task }
        if slug(token, after: "req-") { return .request }
        if isUUID(token) { return .node }
        if isPath(token) { return .path }
        if isURL(token) { return .repo }
        return nil
    }

    /// A token that is safe on its own: a number, a time, a version, a random id or a word of the code.
    public static func isSafe(_ token: String, vocabulary: Set<String>) -> Bool {
        isNumber(token) || isCompoundDuration(token) || isISOTime(token) || isVersion(token) || isRandomID(token)
            || vocabulary.contains(token)
    }

    private static func gap(_ c: Character) -> Bool {
        c.isWhitespace || ",()[]{}\"';=|<>`".contains(c)
    }

    /// A value split into tokens and the punctuation between them; a URL is one token.
    static func split(_ value: String) -> [Part] {
        var parts: [Part] = []
        var rest = Substring(value)
        while !rest.isEmpty {
            let tokenEnd = isURL(rest) ? rest.firstIndex(where: \.isWhitespace) ?? rest.endIndex
                                       : rest.firstIndex(where: gap) ?? rest.endIndex
            if tokenEnd > rest.startIndex {
                let token = rest[..<tokenEnd]
                var trimmed = token
                while let last = trimmed.last, ":.,!?".contains(last) { trimmed = trimmed.dropLast() }
                if trimmed.isEmpty { trimmed = token }
                parts.append(.token(String(trimmed)))
                if trimmed.endIndex < token.endIndex { parts.append(.gap(String(token[trimmed.endIndex...]))) }
                rest = rest[tokenEnd...]
                continue
            }
            let gapEnd = rest.firstIndex { !gap($0) } ?? rest.endIndex
            parts.append(.gap(String(rest[..<gapEnd])))
            rest = rest[gapEnd...]
        }
        return parts
    }

    public static func tokens(_ value: String) -> [String] {
        split(value).compactMap { if case .token(let token) = $0 { token } else { nil } }
    }

    static func pullRequest(_ url: String) -> (repo: String, number: String)? {
        guard url.hasPrefix("https://github.com/") else { return nil }
        let rest = url.dropFirst("https://github.com/".count)
        guard let pull = rest.range(of: "/pull/") else { return nil }
        let repo = rest[..<pull.lowerBound]
        var number = rest[pull.upperBound...]
        while number.last == "/" { number = number.dropLast() }
        guard repo.filter({ $0 == "/" }).count == 1, !number.isEmpty, number.allSatisfy(digit) else { return nil }
        return (String(repo), String(number))
    }

    /// `2026-10-25T06:43:41.854+01:00` as `2026-10-25T05:43:41.854Z`.
    static func utc(_ text: String) -> String? {
        let b = Array(text.utf8)
        guard b.count >= 19, b[4] == 45, b[7] == 45, b[10] == 84, b[13] == 58, b[16] == 58 else { return nil }
        func number(_ range: Range<Int>) -> Int? { Int(String(decoding: b[range], as: UTF8.self)) }
        guard let year = number(0..<4), let month = number(5..<7), let day = number(8..<10),
              let hour = number(11..<13), let minute = number(14..<16), let second = number(17..<19) else { return nil }
        var rest = b[19...]
        var millis = 0
        if rest.first == 46 {
            let fraction = rest.dropFirst()
            let digits = fraction.prefix(while: digitByte)
            let kept = String(decoding: digits.prefix(3), as: UTF8.self)
            guard let parsed = Int(kept.padding(toLength: 3, withPad: "0", startingAt: 0)) else { return nil }
            millis = parsed
            rest = fraction.dropFirst(digits.count)
        }
        let offset: Int
        let tail = Array(rest)
        if tail == [90] {
            offset = 0
        } else if tail.count == 6, tail[0] == 43 || tail[0] == 45, tail[3] == 58,
                  let hours = Int(String(decoding: tail[1..<3], as: UTF8.self)),
                  let minutes = Int(String(decoding: tail[4..<6], as: UTF8.self)) {
            offset = (tail[0] == 45 ? -1 : 1) * (hours * 60 + minutes)
        } else {
            return nil
        }
        guard (1...12).contains(month), (1...31).contains(day), hour <= 23, minute <= 59, second <= 60 else { return nil }
        let days = daysFromCivil(year, month, day)
        return utc(millis: ((days * 86_400 + hour * 3_600 + minute * 60 + second) - offset * 60) * 1_000 + millis)
    }

    static func utc(millis: Int) -> String {
        let seconds = floorDiv(millis, 1_000)
        let days = floorDiv(seconds, 86_400)
        let ofDay = seconds - days * 86_400
        let (year, month, day) = civilFromDays(days)
        return String(format: "%04d-%02d-%02dT%02d:%02d:%02d.%03dZ", year, month, day,
                      ofDay / 3_600, ofDay % 3_600 / 60, ofDay % 60, millis - seconds * 1_000)
    }

    private static func floorDiv(_ a: Int, _ b: Int) -> Int { Int((Double(a) / Double(b)).rounded(.down)) }

    private static func daysFromCivil(_ y: Int, _ month: Int, _ day: Int) -> Int {
        let year = month <= 2 ? y - 1 : y
        let era = floorDiv(year, 400)
        let ofEra = year - era * 400
        let ofYear = (153 * (month + (month > 2 ? -3 : 9)) + 2) / 5 + day - 1
        let ofEraDays = ofEra * 365 + ofEra / 4 - ofEra / 100 + ofYear
        return era * 146_097 + ofEraDays - 719_468
    }

    private static func civilFromDays(_ days: Int) -> (Int, Int, Int) {
        let z = days + 719_468
        let era = floorDiv(z, 146_097)
        let ofEra = z - era * 146_097
        let yearOfEra = (ofEra - ofEra / 1_460 + ofEra / 36_524 - ofEra / 146_096) / 365
        let ofYear = ofEra - (365 * yearOfEra + yearOfEra / 4 - yearOfEra / 100)
        let mp = (5 * ofYear + 2) / 153
        let day = ofYear - (153 * mp + 2) / 5 + 1
        let month = mp < 10 ? mp + 3 : mp - 9
        return (yearOfEra + era * 400 + (month <= 2 ? 1 : 0), month, day)
    }
}
