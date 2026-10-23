import AlondraContract
import AlondraDesign
import AlondraFeatures
import SwiftUI

/// Something the node started in the background (a subagent, a shell, a task): whether it runs
/// and since when, or how it ended.
struct BackgroundRow: View {
    let activity: TaskDetail.Activity

    private var failed: Bool { activity.state?.tone == .critical }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                Image(systemName: failed ? "xmark.octagon.fill" : "terminal")
                if let kind = activity.detail { Text(verbatim: kind) }
                if let state = activity.state { Text(verbatim: "· \(state.text)") }
                Spacer(minLength: 0)
                if let since = activity.since { TimeText(raw: since).foregroundStyle(.tertiary) }
            }
            .font(.caption.weight(.semibold))
            .foregroundStyle(failed ? Palette.ember : Palette.slate)
            Text(verbatim: activity.title).font(.mono(.footnote)).foregroundStyle(Palette.ink).lineLimit(3)
        }
        .accessibilityElement(children: .combine)
    }
}
