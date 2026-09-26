import AlondraAttachmentsFeature
import ComposableArchitecture
import AlondraShared
import SwiftUI

/// What the node reported about the PR it sits in, read right there in the PR's detail.
struct ServiceAttachedReport: View {
    let report: Report
    let pullRequestURL: String
    let downloads: StoreOf<AttachmentDownloadFeature>

    private var style: KindStyle { InboxItem.report(report).style }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Image(systemName: style.symbol)
                    .foregroundStyle(style.tint)
                    .accessibilityHidden(true)
                Text(report.title)
                    .textSelection(.enabled)
            }
            .font(.headline)
            if let body = report.body, !body.isEmpty {
                MarkdownText(text: body)
            }
            ServiceLinksRow(links: report.links.filter { $0.url != pullRequestURL }, attachments: report.attachments, store: downloads)
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(style.tint.opacity(0.08), in: .rect(cornerRadius: 12))
        .overlay {
            RoundedRectangle(cornerRadius: 12)
                .strokeBorder(style.tint.opacity(0.3))
        }
    }
}
