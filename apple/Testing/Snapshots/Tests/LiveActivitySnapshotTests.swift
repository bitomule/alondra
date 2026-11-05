import AlondraLiveActivity
import AlondraLiveActivityUI
import AlondraTransport
import SwiftUI
import XCTest

/// Every presentation of the Live Activity, drawn outside ActivityKit, which offers no way to
/// capture one: the lock screen, the compact, minimal and expanded island, the small family of
/// CarPlay and the Apple Watch, and the island with its width limited.
@MainActor
final class LiveActivitySnapshotTests: XCTestCase {
    private let now = snapshotReferenceDate.addingTimeInterval(10 * 3600 + 42 * 60)

    private func summary(_ reports: [(String, ActivityCounts, minutesAgo: Double)], stale: Bool = false) -> ActivitySummary {
        ActivitySummary(ActivityContentState(reports: reports.map {
            ActivityReport(macID: $0.0, macName: $0.0, counts: $0.1, measuredAt: now.addingTimeInterval(-$0.minutesAgo * 60))
        }), now: now, stale: stale)
    }

    private var busy: ActivitySummary {
        summary([("Mac mini", ActivityCounts(inbox: 2, waiting: 1, working: 1), minutesAgo: 0),
                 ("MacBook Pro", ActivityCounts(inbox: 1, waiting: 0, working: 1), minutesAgo: 3)])
    }

    func test_busy() { assertBothLanguages { ActivityBoard(summary: busy) } }

    func test_allCaughtUp() {
        assertBothLanguages { ActivityBoard(summary: summary([("Mac mini", .zero, minutesAgo: 2)])) }
    }

    func test_noNewsFromAnyMac() {
        assertBothLanguages {
            ActivityBoard(summary: summary([("Mac mini", ActivityCounts(inbox: 3, waiting: 1, working: 2), minutesAgo: 40)], stale: true))
        }
    }

    func test_oneMacWithoutNews() {
        assertBothLanguages {
            ActivityBoard(summary: summary([("Mac mini", ActivityCounts(inbox: 1, waiting: 0, working: 1), minutesAgo: 1),
                                            ("MacBook Pro", ActivityCounts(inbox: 0, waiting: 1, working: 1), minutesAgo: 48)]))
        }
    }

    func test_onlyWorking() {
        assertBothLanguages {
            ActivityBoard(summary: summary([("Mac mini", ActivityCounts(inbox: 0, waiting: 0, working: 3), minutesAgo: 0)]))
        }
    }

    func test_largeNumbers() {
        assertBothLanguages {
            ActivityBoard(summary: summary([("Mac mini", ActivityCounts(inbox: 128, waiting: 42, working: 17), minutesAgo: 0)]))
        }
    }

    func test_nothingOpened() {
        assertBothLanguages {
            ActivityBoard(summary: ActivitySummary(ActivityContentState(macs: [sealed]), now: now, stale: false))
        }
    }

    func test_macsNotCounted() {
        assertBothLanguages {
            ActivityBoard(summary: ActivitySummary(ActivityContentState(reports: [
                ActivityReport(macID: "Mac mini", macName: "Mac mini", counts: ActivityCounts(inbox: 2, waiting: 1, working: 3), measuredAt: now),
            ], omitted: 2), now: now, stale: false))
        }
    }

    private var sealed: Envelope {
        Envelope(route: "mac", sender: "mac", recipient: "phone", requestID: "request", kind: "activity", expiresAtMilliseconds: 1,
                 ephemeralKey: "key", nonce: "nonce", ciphertext: "ciphertext", signature: "signature")
    }

    private func assertBothLanguages<Content: View>(testName: String = #function, line: UInt = #line,
                                                    @ViewBuilder _ content: () -> Content) {
        assertVariants(language: .english, testName: testName, line: line, content)
        assertVariants(language: .spanish, testName: testName, line: line, content)
    }
}

/// The presentations side by side, each on the background iOS gives it: the island is always black.
private struct ActivityBoard: View {
    let summary: ActivitySummary

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            caption("Lock screen")
            ActivityLockScreenView(summary: summary)
                .background(RoundedRectangle(cornerRadius: 24).fill(ActivityPalette.lockScreenBackground))
            caption("Compact · minimal · limited width")
            HStack(spacing: 12) {
                HStack {
                    ActivityCompactLeading(summary: summary)
                    Spacer(minLength: 110)
                    ActivityCompactTrailing(summary: summary)
                }
                .padding(.horizontal, 14)
                .frame(height: 37)
                .background(Capsule().fill(.black))
                ActivityMinimalView(summary: summary)
                    .frame(width: 37, height: 37)
                    .background(Circle().fill(.black))
                ActivityLimitedWidthView(summary: summary)
                    .padding(4)
                    .background(Capsule().fill(.black))
            }
            .environment(\.colorScheme, .dark)
            caption("Expanded")
            VStack(spacing: 8) {
                HStack(alignment: .top) {
                    ActivityExpandedLeading(summary: summary)
                    Spacer()
                    ActivityExpandedCenter()
                    Spacer()
                    ActivityExpandedTrailing(summary: summary)
                }
                ActivityExpandedBottom(summary: summary)
            }
            .padding(18)
            .background(RoundedRectangle(cornerRadius: 44).fill(.black))
            .environment(\.colorScheme, .dark)
            caption("CarPlay and Apple Watch")
            ActivitySmallView(summary: summary)
                .background(RoundedRectangle(cornerRadius: 16).fill(ActivityPalette.lockScreenBackground))
        }
        .padding()
        .background(Color(.systemBackground))
    }

    private func caption(_ text: String) -> some View {
        Text(verbatim: text).font(.caption2.monospaced()).foregroundStyle(.tertiary)
    }
}
