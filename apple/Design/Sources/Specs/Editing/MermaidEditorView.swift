import SwiftUI

/// A diagram's Mermaid text with the diagram drawn under it as it is typed. A diagram that cannot
/// be drawn yet says so, and the code stays as it was written.
public struct MermaidEditorView: View {
    @Binding var code: String
    let placeholder: String
    @State private var previewed: String

    public init(code: Binding<String>, placeholder: String = "") {
        _code = code
        self.placeholder = placeholder
        _previewed = State(initialValue: code.wrappedValue)
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            MultilineTextField(placeholder, text: $code, lines: 6...14)
                .font(.mono(.footnote))
                .autocorrectionDisabledForCode()
                .padding(10).background(Color.primary.opacity(0.06), in: .rect(cornerRadius: 10))
            VStack(alignment: .leading, spacing: 6) {
                Text("Preview", tableName: "AlondraDesign", bundle: #bundle).font(.caption.weight(.bold)).foregroundStyle(.secondary)
                if previewed.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                    Label { Text("Type the diagram to see it drawn", tableName: "AlondraDesign", bundle: #bundle) } icon: { Image(systemName: "point.3.connected.trianglepath.dotted") }
                        .font(.footnote).foregroundStyle(.secondary)
                } else {
                    MermaidBlockView(source: previewed)
                }
            }
        }
        .task(id: code) {
            try? await Task.sleep(for: .milliseconds(400))
            if !Task.isCancelled { previewed = code }
        }
    }
}

private extension View {
    func autocorrectionDisabledForCode() -> some View { autocorrectionDisabled(true) }
}
