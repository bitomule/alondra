import AlondraShared
import SwiftUI

struct TableRowCardView: View {
    let card: TableRowCard

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(card.title)
                .font(.subheadline.weight(.semibold))
            ForEach(card.fields) { field in
                VStack(alignment: .leading, spacing: 2) {
                    Text(field.name)
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(.secondary)
                    Text(field.value)
                        .font(.subheadline)
                }
            }
        }
        .multilineTextAlignment(.leading)
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Palette.night.opacity(0.5), in: .rect(cornerRadius: 12))
        .accessibilityElement(children: .combine)
    }
}
