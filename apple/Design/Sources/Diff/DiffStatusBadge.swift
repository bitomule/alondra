import SwiftUI

/// The letter GitHub gives a changed file: A added, D deleted, R renamed, M modified, B binary.
public struct DiffStatusBadge: View {
    let file: DiffFile

    public init(file: DiffFile) { self.file = file }

    public var body: some View {
        Text(verbatim: letter)
            .font(.caption2.monospaced().weight(.bold))
            .foregroundStyle(tint)
            .frame(minWidth: 18, minHeight: 18)
            .background(tint.opacity(0.14), in: .rect(cornerRadius: 4, style: .continuous))
            .accessibilityLabel(label)
    }

    private var letter: String {
        if file.isBinary && file.status == .modified { return "B" }
        switch file.status {
        case .added: return "A"
        case .deleted: return "D"
        case .renamed: return "R"
        case .modified: return "M"
        }
    }

    private var tint: Color {
        switch file.status {
        case .added: Palette.sage
        case .deleted: Palette.rose
        case .renamed: Palette.sky
        case .modified: file.isBinary ? Palette.slate : Palette.gold
        }
    }

    private var label: String {
        if file.isBinary && file.status == .modified { return String(localized: "Binary", table: "AlondraDesign", bundle: #bundle) }
        return switch file.status {
        case .added: String(localized: "Added", table: "AlondraDesign", bundle: #bundle)
        case .deleted: String(localized: "Deleted", table: "AlondraDesign", bundle: #bundle)
        case .renamed: String(localized: "Renamed", table: "AlondraDesign", bundle: #bundle)
        case .modified: String(localized: "Modified", table: "AlondraDesign", bundle: #bundle)
        }
    }
}
