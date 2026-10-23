import AlondraDesign
import AlondraFeatures
import SwiftUI

/// Something the node started and left running (a subagent, a shell, a task), with since when.
struct BackgroundRow: View {
    let activity: TaskDetail.Activity

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                Image(systemName: "terminal")
                if let kind = activity.detail { Text(verbatim: kind) }
                Spacer(minLength: 0)
                if let since = activity.since { TimeText(raw: since).foregroundStyle(.tertiary) }
            }
            .font(.caption.weight(.semibold))
            .foregroundStyle(Palette.slate)
            Text(verbatim: activity.title).font(.mono(.footnote)).foregroundStyle(Palette.ink).lineLimit(3)
        }
        .accessibilityElement(children: .combine)
    }
}
