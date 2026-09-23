import AlondraShared
import SwiftUI

/// A question's options as buttons, side by side when they fit and stacked when they do not.
struct QuestionOptions: View {
    let options: [QuestionOption]
    var selected: String?
    var showsKeys = true
    var detail: (QuestionOption) -> String? = \.displayDescription
    var prominent: (QuestionOption, Int) -> Bool = { _, _ in false }
    let choose: (QuestionOption) -> Void

    var body: some View {
        ViewThatFits(in: .horizontal) {
            HStack(spacing: 10) { buttons }
            VStack(spacing: 8) { buttons }
        }
    }

    private var buttons: some View {
        ForEach(options.enumerated(), id: \.offset) { index, option in
            OptionButton(
                index: index,
                label: option.displayLabel,
                detail: detail(option),
                prominent: prominent(option, index),
                selected: selected == option.label,
                showsKey: showsKeys,
                action: { choose(option) }
            )
        }
    }
}

extension QuestionOption {
    /// An option the node wants explained in words, like "Request changes: write what to change".
    var needsText: Bool {
        ["write", "escribe"].contains { description?.lowercased().hasPrefix($0) == true }
    }
}
