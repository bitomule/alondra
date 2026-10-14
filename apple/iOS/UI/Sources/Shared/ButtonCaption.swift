import AlondraContract
import SwiftUI

/// The words the server puts under a card's button, such as which commands «Allow always» stops
/// asking about, and where.
struct ButtonCaption: View {
    let action: AlondraContract.Action

    var body: some View {
        if let description = action.description {
            Text(Self.markdown(description))
                .font(.caption)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.leading)
                .frame(maxWidth: .infinity, alignment: .leading)
                .accessibilityIdentifier("card.button.caption")
        }
    }

    static func markdown(_ text: String) -> AttributedString {
        let options = AttributedString.MarkdownParsingOptions(interpretedSyntax: .inlineOnlyPreservingWhitespace)
        return (try? AttributedString(markdown: text, options: options)) ?? AttributedString(text)
    }
}
