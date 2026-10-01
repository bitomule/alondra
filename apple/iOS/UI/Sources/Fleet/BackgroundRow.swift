import AlondraDesign
import AlondraFeatures
import SwiftUI

/// Something the node started and left running (a subagent, a shell, a task), flagged when the
/// daemon suspects it hung.
struct BackgroundRow: View {
    let activity: NodeDetail.Activity

    private var suspect: Bool { activity.warning != nil }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                Image(systemName: suspect ? "exclamationmark.triangle.fill" : "terminal")
                if let kind = activity.detail { Text(verbatim: kind) }
                if let warning = activity.warning { Text(verbatim: "· \(warning)") }
                Spacer(minLength: 0)
                if let since = activity.since { TimeText(raw: since).foregroundStyle(.tertiary) }
            }
            .font(.caption.weight(.semibold))
            .foregroundStyle(suspect ? Palette.ember : Palette.slate)
            Text(verbatim: activity.title).font(.mono(.footnote)).foregroundStyle(Palette.ink).lineLimit(3)
        }
        .accessibilityElement(children: .combine)
    }
}
