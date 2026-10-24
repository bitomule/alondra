import AlondraDesign
import AlondraFeatures
import SwiftUI

struct GrantLine: View {
    let grant: TaskDetail.Grant
    let revoke: (WireAction) -> Void

    var body: some View {
        HStack(alignment: .center, spacing: 12) {
            VStack(alignment: .leading, spacing: 4) {
                Text(verbatim: grant.command)
                    .font(.mono(.subheadline, weight: .semibold))
                    .foregroundStyle(Palette.ink)
                    .multilineTextAlignment(.leading)
                if let detail = grant.detail {
                    Text(verbatim: detail)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.leading)
                }
            }
            Spacer(minLength: 0)
            if let action = grant.revoke {
                Button(role: .destructive) { revoke(action) } label: {
                    Text(verbatim: action.label)
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .tint(Palette.rose)
                .accessibilityIdentifier("task.grant.revoke")
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 13)
    }
}
