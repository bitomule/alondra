import SwiftUI

public struct DiffFileHeader: View {
    let file: DiffFile

    public init(file: DiffFile) {
        self.file = file
    }

    public var body: some View {
        HStack {
            Text(verbatim: file.path)
                .font(.caption.monospaced().weight(.semibold))
                .lineLimit(1)
                .truncationMode(.head)
            Spacer()
            Text(verbatim: "+\(file.additions)")
                .foregroundStyle(.green)
            Text(verbatim: "−\(file.deletions)")
                .foregroundStyle(.red)
        }
        .font(.caption2.monospaced())
        .padding(.horizontal, 10)
        .padding(.vertical, 7)
        .background(.bar)
    }
}
