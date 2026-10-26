import AlondraContract
import AlondraDesign
import AlondraFeatures
import SwiftUI

/// The words the server puts under a card's button, such as which commands «Allow always» stops
/// asking about, and where.
struct ButtonCaption: View {
    let action: AlondraContract.Action

    var body: some View {
        if let description = action.description {
            Text(MarkdownBlock.inline(description))
                .font(.caption)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.leading)
                .frame(maxWidth: .infinity, alignment: .leading)
                .accessibilityIdentifier("card.button.caption")
        }
        DisabledReason(action)
            .font(.caption)
            .foregroundStyle(.secondary)
            .multilineTextAlignment(.leading)
            .frame(maxWidth: .infinity, alignment: .leading)
    }
}
