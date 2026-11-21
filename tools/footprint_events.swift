import CoreServices
import Foundation

let seconds = Double(CommandLine.arguments.dropFirst().first ?? "60") ?? 60
setvbuf(stdout, nil, _IOFBF, 1 << 16)

let callback: FSEventStreamCallback = { _, _, count, paths, _, _ in
    let list = unsafeBitCast(paths, to: NSArray.self)
    for index in 0..<count {
        if let path = list[index] as? String { print(path) }
    }
}

let flags = FSEventStreamCreateFlags(kFSEventStreamCreateFlagFileEvents | kFSEventStreamCreateFlagUseCFTypes | kFSEventStreamCreateFlagNoDefer)
guard let stream = FSEventStreamCreate(nil, callback, nil, ["/"] as CFArray, FSEventStreamEventId(kFSEventStreamEventIdSinceNow), 0.5, flags) else {
    FileHandle.standardError.write("FSEventStreamCreate failed\n".data(using: .utf8)!)
    exit(1)
}
FSEventStreamSetDispatchQueue(stream, DispatchQueue.main)
FSEventStreamStart(stream)
DispatchQueue.main.asyncAfter(deadline: .now() + seconds) {
    FSEventStreamStop(stream)
    FSEventStreamInvalidate(stream)
    fflush(stdout)
    exit(0)
}
dispatchMain()
