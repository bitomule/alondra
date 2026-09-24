import AlondraShared
import SwiftUI
import UniformTypeIdentifiers

/// Picks files to attach to the message being written; each is copied at once and starts
/// uploading to alondrad.
struct AttachFilesButton: View {
    let attachments: Attachments
    let client: AlondraClient
    @State private var choosing = false
    @State private var refusal: String?

    var body: some View {
        Button {
            choosing = true
        } label: {
            Label("Attach Files…", systemImage: "paperclip")
                .labelStyle(.iconOnly)
        }
        .buttonStyle(.borderless)
        .controlSize(.large)
        .help(Text("Attach files (or drop them here)"))
        .fileImporter(isPresented: $choosing, allowedContentTypes: [.item], allowsMultipleSelection: true) { result in
            guard case .success(let urls) = result else { return }
            refusal = AttachFiles.add(urls, to: attachments, client: client)
        }
        .alert("Couldn’t Attach", isPresented: .init(get: { refusal != nil }, set: { if !$0 { refusal = nil } })) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(refusal ?? "")
        }
    }
}

/// Files dropped on a text box are attached to its message.
struct DropsFiles: ViewModifier {
    let attachments: Attachments?
    let client: AlondraClient
    @State private var targeted = false
    @State private var refusal: String?

    func body(content: Content) -> some View {
        if let attachments {
            content
                .overlay {
                    if targeted {
                        RoundedRectangle(cornerRadius: 12)
                            .strokeBorder(Color.accentColor, style: StrokeStyle(lineWidth: 2, dash: [6, 4]))
                            .background(Color.accentColor.opacity(0.08), in: .rect(cornerRadius: 12))
                            .allowsHitTesting(false)
                    }
                }
                .dropDestination(for: URL.self) { urls, _ in
                    refusal = AttachFiles.add(urls, to: attachments, client: client)
                    return !urls.isEmpty
                } isTargeted: { targeted = $0 }
                .alert("Couldn’t Attach", isPresented: .init(get: { refusal != nil }, set: { if !$0 { refusal = nil } })) {
                    Button("OK", role: .cancel) {}
                } message: {
                    Text(refusal ?? "")
                }
        } else {
            content
        }
    }
}

enum AttachFiles {
    /// Reads each file right away and adds it; gives back why one could not be attached, if any.
    @MainActor
    static func add(_ urls: [URL], to attachments: Attachments, client: AlondraClient) -> String? {
        var refusals: [String] = []
        for url in urls where url.isFileURL {
            do {
                if let refused = attachments.add(try AttachmentFile.read(url), name: url.lastPathComponent, using: client) {
                    refusals.append(refused)
                }
            } catch {
                refusals.append(error.localizedDescription)
            }
        }
        return refusals.isEmpty ? nil : refusals.joined(separator: "\n")
    }
}
