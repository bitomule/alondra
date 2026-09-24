import AlondraShared
import PhotosUI
import SwiftUI
import UniformTypeIdentifiers

/// Attaches photos, files or a new picture from the camera to the message being written. Each
/// file is copied into the message at once and starts uploading to alondrad.
struct AttachButton: View {
    let attachments: Attachments
    @Environment(FleetStore.self) private var fleet
    @State private var choosingPhotos = false
    @State private var photos: [PhotosPickerItem] = []
    @State private var choosingFiles = false
    @State private var takingPicture = false
    @State private var refusal: String?

    var body: some View {
        Menu {
            Button("Photos", systemImage: "photo.on.rectangle") { choosingPhotos = true }
            if UIImagePickerController.isSourceTypeAvailable(.camera) {
                Button("Camera", systemImage: "camera") { takingPicture = true }
            }
            Button("Files", systemImage: "folder") { choosingFiles = true }
        } label: {
            Image(systemName: "paperclip")
                .font(.body.weight(.semibold))
                .foregroundStyle(.secondary)
                .frame(width: 36, height: 36)
        }
        .accessibilityLabel("Attach")
        .photosPicker(isPresented: $choosingPhotos, selection: $photos, maxSelectionCount: 10, matching: .any(of: [.images, .screenshots]))
        .onChange(of: photos) { _, picked in
            guard !picked.isEmpty else { return }
            photos = []
            Task { await addPhotos(picked) }
        }
        .fileImporter(isPresented: $choosingFiles, allowedContentTypes: [.item], allowsMultipleSelection: true) { result in
            guard case .success(let urls) = result else { return }
            for url in urls {
                do {
                    add(try AttachmentFile.read(url), name: url.lastPathComponent)
                } catch {
                    refusal = error.localizedDescription
                }
            }
        }
        .fullScreenCover(isPresented: $takingPicture) {
            CameraPicker { data in
                add(data, name: String(localized: "Photo \(Date.now.formatted(date: .numeric, time: .standard))").replacing("/", with: "-") + ".jpg")
            }
            .ignoresSafeArea()
        }
        .alert("Couldn’t Attach", isPresented: .init(get: { refusal != nil }, set: { if !$0 { refusal = nil } })) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(refusal ?? "")
        }
    }

    private func addPhotos(_ picked: [PhotosPickerItem]) async {
        for (index, item) in picked.enumerated() {
            guard let data = try? await item.loadTransferable(type: Data.self) else {
                refusal = String(localized: "That photo could not be read. If it is in iCloud, try again once it has downloaded.")
                continue
            }
            let ext = item.supportedContentTypes.first?.preferredFilenameExtension ?? "jpg"
            add(data, name: String(localized: "Photo \(index + 1)") + ".\(ext)")
        }
    }

    private func add(_ data: Data, name: String) {
        if let refused = attachments.add(data, name: name, using: fleet.client) {
            refusal = refused
        }
    }
}
