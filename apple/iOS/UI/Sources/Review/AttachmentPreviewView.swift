import AlondraDesign
import AlondraFeatures
import SwiftUI
import UIKit

public struct AttachmentPreviewView: View {
    let preview: AttachmentPreview
    @Environment(\.dismiss) private var dismiss

    public init(preview: AttachmentPreview) { self.preview = preview }

    public var body: some View {
        NavigationStack {
            ScrollView {
                Group {
                    if preview.attachment.kind == .image, let image = UIImage(data: preview.data) {
                        Image(uiImage: image).resizable().scaledToFit()
                    } else if preview.attachment.kind == .text,
                              let text = String(data: preview.data, encoding: .utf8) {
                        Text(text).font(.system(.body, design: .monospaced))
                            .textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading)
                    } else {
                        ContentUnavailableView("Preview unavailable", systemImage: "doc")
                    }
                }
                .padding(20)
            }
            .background(DawnBackground())
            .navigationTitle(preview.attachment.name)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
        }
    }
}
