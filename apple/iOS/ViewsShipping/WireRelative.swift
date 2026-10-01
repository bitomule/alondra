import Foundation

func relativeSince(_ raw: String) -> String? {
    let fractional = ISO8601DateFormatter()
    fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
    guard let date = fractional.date(from: raw) ?? ISO8601DateFormatter().date(from: raw) else { return nil }
    return relative(Int64(date.timeIntervalSince1970 * 1_000))
}
