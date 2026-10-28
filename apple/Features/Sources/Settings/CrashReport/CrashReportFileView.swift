import AlondraDesign
import SwiftUI

/// One file of a crash report, whole, exactly as it would leave the Mac.
struct CrashReportFileView: View {
    let viewing: CrashReportFeature.Viewing
    let close: () -> Void

    var body: some View {
        NavigationStack {
            Group {
                if let text = viewing.text {
                    let lines = text.split(separator: "\n", omittingEmptySubsequences: false).map(String.init)
                    ScrollView([.vertical, .horizontal]) {
                        LazyVStack(alignment: .leading, spacing: 0) {
                            ForEach(lines.indices, id: \.self) { index in
                                Text(verbatim: lines[index].isEmpty ? " " : lines[index])
                                    .font(.mono(.caption))
                                    .fixedSize()
                            }
                        }
                        .textSelection(.enabled)
                        .padding()
                    }
                } else if let error = viewing.error {
                    Text(verbatim: error).foregroundStyle(.secondary).padding()
                } else {
                    ProgressView()
                }
            }
            .navigationTitle(Text(verbatim: viewing.name))
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close", action: close)
                }
            }
        }
        #if os(macOS)
        .frame(minWidth: 560, minHeight: 420)
        #endif
    }
}
