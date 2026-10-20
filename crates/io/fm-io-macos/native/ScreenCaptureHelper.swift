// The Freemix screen and window capture helper (IN-011), built by build.rs
// against ScreenCaptureKit (macOS 12.3+; the workspace targets 13.0).
//
//   freemix-screen-capture-helper list
//     Prints the shareable screens and on-screen application windows, one
//     tab-separated record per line, then `END`:
//       S <index> <primary 1|0> <x> <y> <width> <height> <name>
//       W <window id> <pid> <x> <y> <width> <height> <app> <title>
//     Screens are indexed main display first, then by position, the same
//     order `capture display` uses. Positions and sizes are in points.
//
//   freemix-screen-capture-helper capture display|window <index|id> <x> <y> <width> <height> <fps> <cursor 1|0>
//     Captures the rectangle (in the target's own points) of the screen or
//     window and writes raw BGRA pictures of exactly width x height pixels
//     to standard output at fps, repeating the latest picture while nothing
//     changes (ScreenCaptureKit only delivers changed frames). It exits
//     with a non-zero status when the target disappears or capture stops,
//     and quietly when standard output closes.
//
// Exit statuses: 2 permission or start failure, 3 listing timeout,
// 4 stream stopped, 5 target not found, 64 usage.

import CoreGraphics
import CoreMedia
import CoreVideo
import Darwin
import Dispatch
import Foundation
import ScreenCaptureKit

private let maximumSide = 16_384

private func fail(_ message: String, code: Int32) -> Never {
    FileHandle.standardError.write(Data((message + "\n").utf8))
    exit(code)
}

private func field(_ text: String) -> String {
    text.replacingOccurrences(of: "\t", with: " ")
        .replacingOccurrences(of: "\r", with: " ")
        .replacingOccurrences(of: "\n", with: " ")
}

private final class ContentBox: @unchecked Sendable {
    var content: SCShareableContent?
    var error: Error?
}

private func shareableContent() -> SCShareableContent {
    let semaphore = DispatchSemaphore(value: 0)
    let box = ContentBox()
    SCShareableContent.getExcludingDesktopWindows(true, onScreenWindowsOnly: true) { content, error in
        box.content = content
        box.error = error
        semaphore.signal()
    }
    if semaphore.wait(timeout: .now() + .seconds(10)) == .timedOut {
        fail("listing the desktop timed out", code: 3)
    }
    guard let content = box.content else {
        let reason = box.error.map { String(describing: $0) } ?? "no shareable content"
        fail("screen recording is not permitted for the server: \(reason)", code: 2)
    }
    return content
}

/// Main display first, then by position.
private func orderedDisplays(_ content: SCShareableContent) -> [SCDisplay] {
    let main = CGMainDisplayID()
    return content.displays.sorted { left, right in
        if (left.displayID == main) != (right.displayID == main) {
            return left.displayID == main
        }
        if left.frame.origin.x != right.frame.origin.x {
            return left.frame.origin.x < right.frame.origin.x
        }
        return left.frame.origin.y < right.frame.origin.y
    }
}

private func list() {
    let content = shareableContent()
    let main = CGMainDisplayID()
    var lines: [String] = []
    for (index, display) in orderedDisplays(content).enumerated() {
        let frame = display.frame
        let primary = display.displayID == main ? 1 : 0
        lines.append(
            "S\t\(index)\t\(primary)\t\(Int(frame.origin.x))\t\(Int(frame.origin.y))\t"
                + "\(display.width)\t\(display.height)\tDisplay \(index + 1)")
    }
    for window in content.windows where window.windowLayer == 0 && window.isOnScreen {
        guard let application = window.owningApplication else { continue }
        let frame = window.frame
        guard frame.width >= 2, frame.height >= 2 else { continue }
        lines.append(
            "W\t\(window.windowID)\t\(application.processID)\t\(Int(frame.origin.x))\t"
                + "\(Int(frame.origin.y))\t\(Int(frame.width))\t\(Int(frame.height))\t"
                + "\(field(application.applicationName))\t\(field(window.title ?? ""))")
    }
    lines.append("END")
    print(lines.joined(separator: "\n"))
}

private func writeAll(_ data: Data) {
    data.withUnsafeBytes { (buffer: UnsafeRawBufferPointer) in
        guard var pointer = buffer.baseAddress else { return }
        var remaining = buffer.count
        while remaining > 0 {
            let written = Darwin.write(STDOUT_FILENO, pointer, remaining)
            if written < 0 {
                if errno == EINTR { continue }
                // The reader closed its end: capture is over.
                exit(0)
            }
            pointer += written
            remaining -= written
        }
    }
}

/// Keeps the latest complete picture, scaled to the output size by
/// ScreenCaptureKit and copied row by row into a packed BGRA buffer.
private final class Pictures: NSObject, SCStreamOutput, SCStreamDelegate, @unchecked Sendable {
    let width: Int
    let height: Int
    private let lock = NSLock()
    private var latest: Data

    init(width: Int, height: Int) {
        self.width = width
        self.height = height
        // Opaque black until the first picture.
        var black = Data(count: width * height * 4)
        black.withUnsafeMutableBytes { (bytes: UnsafeMutableRawBufferPointer) in
            for offset in stride(from: 3, to: bytes.count, by: 4) {
                bytes[offset] = 0xff
            }
        }
        latest = black
    }

    func current() -> Data {
        lock.lock()
        defer { lock.unlock() }
        return latest
    }

    func stream(
        _ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer,
        of type: SCStreamOutputType
    ) {
        guard type == .screen, CMSampleBufferIsValid(sampleBuffer) else { return }
        guard
            let attachments = CMSampleBufferGetSampleAttachmentsArray(
                sampleBuffer, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
            let rawStatus = attachments.first?[.status] as? Int,
            SCFrameStatus(rawValue: rawStatus) == .complete,
            let pixels = CMSampleBufferGetImageBuffer(sampleBuffer)
        else { return }
        CVPixelBufferLockBaseAddress(pixels, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(pixels, .readOnly) }
        guard let base = CVPixelBufferGetBaseAddress(pixels) else { return }
        let rowBytes = CVPixelBufferGetBytesPerRow(pixels)
        let rows = min(height, CVPixelBufferGetHeight(pixels))
        let columns = min(width, CVPixelBufferGetWidth(pixels))
        var picture = Data(count: width * height * 4)
        picture.withUnsafeMutableBytes { (target: UnsafeMutableRawBufferPointer) in
            guard let destination = target.baseAddress else { return }
            for row in 0..<rows {
                memcpy(destination + row * width * 4, base + row * rowBytes, columns * 4)
            }
        }
        lock.lock()
        latest = picture
        lock.unlock()
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        fail("capture stopped: \(error)", code: 4)
    }
}

private final class StartBox: @unchecked Sendable {
    var error: Error?
}

private func capture(_ arguments: [String]) -> Never {
    guard arguments.count == 10,
        let target = UInt32(arguments[3]),
        let x = Int(arguments[4]), let y = Int(arguments[5]),
        let width = Int(arguments[6]), let height = Int(arguments[7]),
        let fps = Int(arguments[8]),
        (1...60).contains(fps), (2...maximumSide).contains(width), (2...maximumSide).contains(height),
        x >= 0, y >= 0
    else {
        fail(
            "usage: capture display|window <index|id> <x> <y> <width> <height> <fps> <cursor 1|0>",
            code: 64)
    }
    let content = shareableContent()
    let filter: SCContentFilter
    switch arguments[2] {
    case "display":
        let displays = orderedDisplays(content)
        guard Int(target) < displays.count else {
            fail("screen \(target + 1) is not connected", code: 5)
        }
        filter = SCContentFilter(display: displays[Int(target)], excludingWindows: [])
    case "window":
        guard let window = content.windows.first(where: { $0.windowID == target }) else {
            fail("window \(target) is gone", code: 5)
        }
        filter = SCContentFilter(desktopIndependentWindow: window)
    default:
        fail("unknown capture target \(arguments[2])", code: 64)
    }

    let configuration = SCStreamConfiguration()
    configuration.width = width
    configuration.height = height
    configuration.sourceRect = CGRect(x: x, y: y, width: width, height: height)
    configuration.pixelFormat = kCVPixelFormatType_32BGRA
    configuration.showsCursor = arguments[9] == "1"
    configuration.minimumFrameInterval = CMTime(value: 1, timescale: CMTimeScale(fps))
    configuration.queueDepth = 3

    let pictures = Pictures(width: width, height: height)
    let stream = SCStream(filter: filter, configuration: configuration, delegate: pictures)
    do {
        try stream.addStreamOutput(
            pictures, type: .screen,
            sampleHandlerQueue: DispatchQueue(label: "freemix.screen-capture.pictures"))
    } catch {
        fail("capture could not start: \(error)", code: 2)
    }
    let started = DispatchSemaphore(value: 0)
    let start = StartBox()
    stream.startCapture { error in
        start.error = error
        started.signal()
    }
    started.wait()
    if let error = start.error {
        fail("capture could not start: \(error)", code: 2)
    }

    let writer = DispatchSource.makeTimerSource(
        queue: DispatchQueue(label: "freemix.screen-capture.writer"))
    writer.schedule(deadline: .now(), repeating: .nanoseconds(1_000_000_000 / fps))
    writer.setEventHandler { writeAll(pictures.current()) }
    writer.resume()
    withExtendedLifetime((stream, writer, pictures)) {
        dispatchMain()
    }
}

signal(SIGPIPE, SIG_IGN)
let arguments = CommandLine.arguments
guard arguments.count >= 2 else {
    fail("usage: freemix-screen-capture-helper list | capture ...", code: 64)
}
switch arguments[1] {
case "list":
    list()
case "capture":
    capture(arguments)
default:
    fail("unknown command \(arguments[1])", code: 64)
}
