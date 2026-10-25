import SwiftUI

/// One changed file in the list of files: what happened to it, its name over its folder, and its lines in and out.
public struct DiffFileRow: View {
    let file: DiffFile

    public init(file: DiffFile) { self.file = file }

    public var body: some View {
        HStack(spacing: 10) {
            DiffStatusBadge(file: file)
            VStack(alignment: .leading, spacing: 2) {
                Text(verbatim: name(file.path))
                    .font(.subheadline.monospaced().weight(.medium))
                    .foregroundStyle(Palette.ink)
                    .lineLimit(1).truncationMode(.middle)
                if let folder = folder {
                    Text(verbatim: folder)
                        .font(.caption.monospaced())
                        .foregroundStyle(.secondary)
                        .lineLimit(1).truncationMode(.head)
                }
            }
            Spacer(minLength: 8)
            DiffCounts(file: file)
        }
        .accessibilityElement(children: .combine)
    }

    private func name(_ path: String) -> String { path.split(separator: "/").last.map(String.init) ?? path }

    private var folder: String? {
        if let oldPath = file.oldPath { return "\(oldPath) → \(file.path)" }
        let folder = file.path.split(separator: "/").dropLast().joined(separator: "/")
        return folder.isEmpty ? nil : folder
    }
}

/// A file's lines in and out with its five squares, or that it is binary.
public struct DiffCounts: View {
    let file: DiffFile

    public init(file: DiffFile) { self.file = file }

    public var body: some View {
        HStack(spacing: 6) {
            if file.isBinary {
                Text("Binary", tableName: "AlondraDesign", bundle: #bundle).foregroundStyle(.secondary)
            } else {
                Text(verbatim: "+\(file.additions)").foregroundStyle(Palette.sage)
                Text(verbatim: "−\(file.deletions)").foregroundStyle(Palette.rose)
                DiffStatBar(additions: file.additions, deletions: file.deletions)
            }
        }
        .font(.caption.monospacedDigit().weight(.medium))
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(file.isBinary
            ? String(localized: "Binary", table: "AlondraDesign", bundle: #bundle)
            : String(localized: "Added: \(file.additions), removed: \(file.deletions)", table: "AlondraDesign", bundle: #bundle))
    }
}
