import AlondraDesign
import AlondraFeatures
import SwiftUI

/// One thing the task's node is doing now: what it runs and, for a subagent, which one.
struct ActivityLine: View {
    let activity: TaskDetail.Activity

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: "terminal").font(.footnote).foregroundStyle(.secondary).frame(width: 20)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 3) {
                HStack(alignment: .firstTextBaseline, spacing: 4) {
                    Text(verbatim: activity.title).font(.mono(.footnote)).foregroundStyle(Palette.sky).lineLimit(2)
                    Spacer(minLength: 4)
                    if let since = activity.since { TimeText(raw: since).font(.caption).foregroundStyle(.tertiary) }
                }
                if let detail = activity.detail {
                    Text(verbatim: detail).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                }
            }
        }
        .accessibilityElement(children: .combine)
    }
}
