import AlondraContract
import SwiftUI

/// What the banner says while every Claude account is at its real limit, kept apart from the view so it can be tested.
public struct UsageExhaustedWording: Equatable, Sendable {
    public enum Subject: Equatable, Sendable {
        case unspecified
        case thisMac
        case someMacs(exhausted: Int, total: Int)
        case allMacs
        case pausedByUser
    }

    public enum When: Equatable, Sendable {
        case until(Date)
        case minutesLeft(Int)
        case untilText(String)
    }

    public let subject: Subject
    public let when: When

    public init(_ usage: UsageExhausted, now: Date) {
        subject = Self.subject(of: usage)
        guard let until = Self.parse(usage.until) else {
            when = .untilText(usage.until)
            return
        }
        let minutes = Int((until.timeIntervalSince(now) / 60).rounded(.up))
        when = (1..<60).contains(minutes) && !usage.pausedByUser ? .minutesLeft(minutes) : .until(until)
    }

    private static func subject(of usage: UsageExhausted) -> Subject {
        if usage.pausedByUser { return .pausedByUser }
        if usage.macsTotal > 1 {
            return usage.macsExhausted >= usage.macsTotal
                ? .allMacs : .someMacs(exhausted: Int(usage.macsExhausted), total: Int(usage.macsTotal))
        }
        return usage.scope == "this_mac" ? .thisMac : .unspecified
    }

    static func parse(_ iso: String) -> Date? {
        let withFraction = ISO8601DateFormatter()
        withFraction.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        let plain = ISO8601DateFormatter()
        return withFraction.date(from: iso) ?? plain.date(from: iso)
    }
}

/// One line across the top of a screen while every Claude account is at its limit and new work waits.
public struct UsageExhaustedBanner: View {
    private let usage: UsageExhausted
    @Environment(\.locale) private var locale
    @Environment(\.calendar) private var calendar
    @Environment(\.timeZone) private var timeZone

    public init(_ usage: UsageExhausted) { self.usage = usage }

    @State private var now = RelativeClock.now()

    public var body: some View {
        Group {
            let wording = UsageExhaustedWording(usage, now: now)
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Image(systemName: "pause.circle.fill").foregroundStyle(Palette.dawn)
                message(wording, now: now)
                    .font(.footnote.weight(.semibold))
                    .foregroundStyle(Palette.ink)
                    .multilineTextAlignment(.leading)
                    .fixedSize(horizontal: false, vertical: true)
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 14).padding(.vertical, 10)
            .glassEffect(.regular.tint(Palette.dawn.opacity(0.18)), in: .rect(cornerRadius: 18))
            .accessibilityElement(children: .combine)
        }
        .onReceive(Timer.publish(every: 30, on: .main, in: .common).autoconnect()) { _ in now = RelativeClock.now() }
    }

    private func clock(_ date: Date, now: Date) -> String {
        var calendar = calendar
        calendar.timeZone = timeZone
        let style = calendar.isDate(date, inSameDayAs: now)
            ? Date.FormatStyle(date: .omitted, time: .shortened, locale: locale, calendar: calendar, timeZone: timeZone)
            : Date.FormatStyle(date: .abbreviated, time: .shortened, locale: locale, calendar: calendar, timeZone: timeZone)
        return date.formatted(style)
    }

    private func timeText(_ when: UsageExhaustedWording.When, now: Date) -> String {
        switch when {
        case .until(let date):
            var calendar = calendar
            calendar.timeZone = timeZone
            return date.formatted(Date.FormatStyle(date: .omitted, time: .shortened, locale: locale, calendar: calendar, timeZone: timeZone))
        case .untilText(let text): return text
        case .minutesLeft: return ""
        }
    }

    private func whenText(_ when: UsageExhaustedWording.When, now: Date) -> Text {
        switch when {
        case .until(let date): Text("until \(clock(date, now: now))", tableName: "AlondraDesign", bundle: #bundle)
        case .untilText(let text): Text("until \(text)", tableName: "AlondraDesign", bundle: #bundle)
        case .minutesLeft(let minutes): Text("for \(minutes) more min", tableName: "AlondraDesign", bundle: #bundle)
        }
    }

    private func message(_ wording: UsageExhaustedWording, now: Date) -> Text {
        let when = whenText(wording.when, now: now)
        switch wording.subject {
        case .unspecified:
            return Text("No margin \(when) · every Claude account is at its limit", tableName: "AlondraDesign", bundle: #bundle)
        case .thisMac:
            return Text("No margin on this Mac \(when) · every Claude account is at its limit", tableName: "AlondraDesign", bundle: #bundle)
        case .someMacs(let exhausted, let total):
            return Text("\(exhausted) of \(total) Macs with no margin \(when) · their Claude accounts are at their limit", tableName: "AlondraDesign", bundle: #bundle)
        case .pausedByUser:
            return Text("Paused by you until tomorrow \(timeText(wording.when, now: now))", tableName: "AlondraDesign", bundle: #bundle)
        case .allMacs:
            return Text("No margin on any of your Macs \(when) · every Claude account is at its limit", tableName: "AlondraDesign", bundle: #bundle)
        }
    }
}

/// One line across the top of a screen while no Claude account has a session and new work waits for one.
public struct SessionWaitBanner: View {
    private let notice: Notice

    public init(_ notice: Notice) { self.notice = notice }

    public var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: "person.crop.circle.badge.exclamationmark").foregroundStyle(Palette.dawn)
            Text(verbatim: notice.text)
                .font(.footnote.weight(.semibold))
                .foregroundStyle(Palette.ink)
                .multilineTextAlignment(.leading)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 14).padding(.vertical, 10)
        .glassEffect(.regular.tint(Palette.dawn.opacity(0.18)), in: .rect(cornerRadius: 18))
        .accessibilityElement(children: .combine)
    }
}
