import CryptoKit
import Foundation

enum LoopbackDigest {
    static func hex(_ data: Data) -> String {
        SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
    }
}
