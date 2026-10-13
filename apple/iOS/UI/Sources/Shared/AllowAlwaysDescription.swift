import AlondraContract
import SwiftUI

/// Under a permission card's «Allow always»: exactly which commands stop asking, and where, as
/// the daemon words it. Nothing under any other button.
struct AllowAlwaysDescription: View {
    let action: AlondraContract.Action

    var body: some View {
        if action.optionId == "always", let description = action.description {
            Text(Self.markdown(description))
                .font(.caption)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.leading)
                .frame(maxWidth: .infinity, alignment: .leading)
                .accessibilityIdentifier("card.allowAlways.description")
        }
    }

    static func markdown(_ text: String) -> AttributedString {
        let options = AttributedString.MarkdownParsingOptions(interpretedSyntax: .inlineOnlyPreservingWhitespace)
        return (try? AttributedString(markdown: text, options: options)) ?? AttributedString(text)
    }
}
