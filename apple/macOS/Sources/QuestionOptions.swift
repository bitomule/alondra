import AlondraShared
import SwiftUI

/// A question's options as buttons, side by side when they fit and stacked when they do not.
struct QuestionOptions: View {
    let options: [DomainV2.QuestionOption]
    /// The chosen option's ID.
    var selected: String?
    var showsKeys = true
    var detail: (DomainV2.QuestionOption) -> String? = { _ in nil }
    var prominent: (DomainV2.QuestionOption, Int) -> Bool = { _, _ in false }
    let choose: (DomainV2.QuestionOption) -> Void

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
                selected: selected == option.id,
                showsKey: showsKeys,
                action: { choose(option) }
            )
        }
    }
}
