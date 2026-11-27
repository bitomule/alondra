import AlondraContract
import SwiftUI

/// How a scenario of a delivery was validated: its state, its test and suite, and each verdict,
/// which is always marked as a judgment and shows the captures it was made on.
struct SheetResultView: View {
    let result: SheetResult

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            SheetLineView(line: result.state).font(.subheadline.weight(.semibold))
            if !result.checks.isEmpty || !result.tests.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    ForEach(Array(result.checks.enumerated()), id: \.offset) { _, line in SheetLineView(line: line) }
                    ForEach(Array(result.tests.enumerated()), id: \.offset) { _, line in SheetLineView(line: line, monospaced: true) }
                }
            }
            ForEach(Array(result.verdicts.enumerated()), id: \.offset) { _, verdict in SheetVerdictView(verdict: verdict) }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Palette.night.opacity(0.55), in: .rect(cornerRadius: 12))
        .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(Palette.hairline))
    }
}

struct SheetVerdictView: View {
    let verdict: SheetVerdict

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            SheetLineView(line: verdict.line)
            if let reasoning = verdict.reasoning {
                Text(reasoning).font(.footnote).foregroundStyle(Palette.ink).fixedSize(horizontal: false, vertical: true)
            }
            if let wrong = verdict.nearestWrong {
                VStack(alignment: .leading, spacing: 3) {
                    if let label = verdict.nearestLabel { Text(label).font(.caption.weight(.semibold)).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true) }
                    Text(wrong).font(.footnote).foregroundStyle(Palette.ink).fixedSize(horizontal: false, vertical: true)
                }
                .padding(.leading, 10)
                .overlay(alignment: .leading) { Rectangle().fill(Palette.gold.opacity(0.7)).frame(width: 2) }
            }
            ForEach(verdict.evidence) { part in SheetPartView(part: part) }
        }
    }
}
