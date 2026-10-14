import AlondraDesign
import SwiftUI

struct ArtifactLinks: View {
    let text: String
    let tint: Color

    var body: some View {
        ForEach(Artifacts.urls(in: text), id: \.self) { url in
            ArtifactLink(url: url, tint: tint).padding(.bottom, 12)
        }
    }
}
