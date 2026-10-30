import ActivityKit
import AlondraLiveActivity
import AlondraLiveActivityUI
import SwiftUI
import WidgetKit

@main
struct AlondraLiveActivityBundle: WidgetBundle {
    var body: some Widget { AlondraActivityWidget() }
}

struct AlondraActivityWidget: Widget {
    var body: some WidgetConfiguration {
        ActivityConfiguration(for: AlondraActivityAttributes.self) { context in
            ActivityFamilyContent(summary: ActivitySummary(context.state, now: .now, stale: context.isStale))
                .widgetURL(AlondraLink.inbox.url)
        } dynamicIsland: { context in
            let summary = ActivitySummary(context.state, now: .now, stale: context.isStale)
            return DynamicIsland {
                DynamicIslandExpandedRegion(.leading) {
                    Link(destination: AlondraLink.inbox.url) { ActivityExpandedLeading(summary: summary) }
                }
                DynamicIslandExpandedRegion(.trailing) {
                    Link(destination: AlondraLink.tasks.url) { ActivityExpandedTrailing(summary: summary) }
                }
                DynamicIslandExpandedRegion(.center) { ActivityExpandedCenter() }
                DynamicIslandExpandedRegion(.bottom) {
                    Link(destination: AlondraLink.tasks.url) { ActivityExpandedBottom(summary: summary) }
                }
            } compactLeading: {
                LimitedWidth { ActivityLimitedWidthView(summary: summary) } otherwise: { ActivityCompactLeading(summary: summary) }
            } compactTrailing: {
                LimitedWidth { EmptyView() } otherwise: { ActivityCompactTrailing(summary: summary) }
            } minimal: {
                ActivityMinimalView(summary: summary)
            }
            .widgetURL(AlondraLink.inbox.url)
            .keylineTint(ActivityPalette.accent)
        }
        .supplementalActivityFamilies([.small])
    }
}

/// The lock screen draws the medium family; CarPlay and the Apple Watch ask for the small one.
private struct ActivityFamilyContent: View {
    let summary: ActivitySummary
    @Environment(\.activityFamily) private var family

    var body: some View {
        switch family {
        case .small: ActivitySmallView(summary: summary)
        default: ActivityLockScreenView(summary: summary)
        }
    }
}

/// iOS 27 limits the island's width in landscape; earlier systems never do.
private struct LimitedWidth<Limited: View, Otherwise: View>: View {
    @ViewBuilder let limited: () -> Limited
    @ViewBuilder let otherwise: () -> Otherwise

    var body: some View {
        if #available(iOS 27, *) {
            LimitedWidthReader(limited: limited, otherwise: otherwise)
        } else {
            otherwise()
        }
    }
}

@available(iOS 27, *)
private struct LimitedWidthReader<Limited: View, Otherwise: View>: View {
    let limited: () -> Limited
    let otherwise: () -> Otherwise
    @Environment(\.isDynamicIslandLimitedInWidth) private var isLimited

    var body: some View {
        if isLimited { limited() } else { otherwise() }
    }
}
