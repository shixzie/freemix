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
//   freemix-screen-capture-helper audio --display <index> | --app <pid>
//     Captures sound with ScreenCaptureKit (macOS 13+): the whole system
//     output through a display, or only the application with that process
//     id, never the helper's own sound. Writes raw interleaved 48 kHz
//     stereo Float32 little-endian samples to standard output, and status
//     lines `FMAUDIO<TAB><live|no_stream|unsupported|error><TAB><detail>`
//     to standard error. While the application is not running it reports
//     no_stream and looks again every second.
//
// Exit statuses: 2 permission or start failure, 3 listing timeout (and,
// for audio, unsupported on this macOS), 4 stream stopped, 5 target not
// found, 64 usage.

import CoreAudio
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

private func audioStatus(_ state: String, _ detail: String = "") {
    let line = detail.isEmpty ? "FMAUDIO\t\(state)\n" : "FMAUDIO\t\(state)\t\(field(detail))\n"
    FileHandle.standardError.write(Data(line.utf8))
}

private func audioFail(_ message: String, code: Int32) -> Never {
    audioStatus(code == 3 ? "unsupported" : "error", message)
    exit(code)
}

/// The shareable content, or nil (with the reason reported) when it is
/// refused or times out.
private func audioContent() -> SCShareableContent? {
    let semaphore = DispatchSemaphore(value: 0)
    let box = ContentBox()
    SCShareableContent.getExcludingDesktopWindows(true, onScreenWindowsOnly: false) { content, error in
        box.content = content
        box.error = error
        semaphore.signal()
    }
    if semaphore.wait(timeout: .now() + .seconds(10)) == .timedOut {
        audioStatus("error", "listing the desktop timed out")
        return nil
    }
    if box.content == nil {
        let reason = box.error.map { String(describing: $0) } ?? "no shareable content"
        audioFail("screen recording is not permitted for the server: \(reason)", code: 2)
    }
    return box.content
}

/// Writes each audio buffer as interleaved stereo Float32: the first two
/// channels (a mono buffer on both), planar or interleaved.
private final class Sound: NSObject, SCStreamOutput, SCStreamDelegate, @unchecked Sendable {
    func stream(
        _ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer,
        of type: SCStreamOutputType
    ) {
        guard CMSampleBufferIsValid(sampleBuffer), CMSampleBufferDataIsReady(sampleBuffer),
            let description = CMSampleBufferGetFormatDescription(sampleBuffer),
            let basic = CMAudioFormatDescriptionGetStreamBasicDescription(description)?.pointee,
            basic.mFormatFlags & kAudioFormatFlagIsFloat != 0, basic.mBitsPerChannel == 32
        else { return }
        if #available(macOS 13.0, *) {
            guard type == .audio else { return }
        }
        var needed = 0
        CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
            sampleBuffer, bufferListSizeNeededOut: &needed, bufferListOut: nil, bufferListSize: 0,
            blockBufferAllocator: nil, blockBufferMemoryAllocator: nil, flags: 0,
            blockBufferOut: nil)
        guard needed > 0 else { return }
        let storage = UnsafeMutableRawPointer.allocate(
            byteCount: needed, alignment: MemoryLayout<AudioBufferList>.alignment)
        defer { storage.deallocate() }
        let list = storage.bindMemory(to: AudioBufferList.self, capacity: 1)
        var block: CMBlockBuffer?
        let status = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
            sampleBuffer, bufferListSizeNeededOut: nil, bufferListOut: list,
            bufferListSize: needed, blockBufferAllocator: nil, blockBufferMemoryAllocator: nil,
            flags: kCMSampleBufferFlag_AudioBufferList_Assure16ByteAlignment,
            blockBufferOut: &block)
        guard status == noErr else { return }
        let buffers = UnsafeMutableAudioBufferListPointer(list)
        let frames = CMSampleBufferGetNumSamples(sampleBuffer)
        guard frames > 0, buffers.count > 0 else { return }
        let planar = basic.mFormatFlags & kAudioFormatFlagIsNonInterleaved != 0
        var samples = [Float](repeating: 0, count: frames * 2)
        if planar {
            let left = buffers[0]
            let right = buffers[min(1, buffers.count - 1)]
            guard let leftData = left.mData, let rightData = right.mData else { return }
            let leftSamples = leftData.assumingMemoryBound(to: Float.self)
            let rightSamples = rightData.assumingMemoryBound(to: Float.self)
            let available = min(
                frames, Int(left.mDataByteSize) / 4, Int(right.mDataByteSize) / 4)
            for frame in 0..<available {
                samples[frame * 2] = leftSamples[frame]
                samples[frame * 2 + 1] = rightSamples[frame]
            }
        } else {
            let buffer = buffers[0]
            guard let data = buffer.mData else { return }
            let channels = max(1, Int(buffer.mNumberChannels))
            let interleaved = data.assumingMemoryBound(to: Float.self)
            let available = min(frames, Int(buffer.mDataByteSize) / (4 * channels))
            for frame in 0..<available {
                samples[frame * 2] = interleaved[frame * channels]
                samples[frame * 2 + 1] = interleaved[frame * channels + min(1, channels - 1)]
            }
        }
        // Apple platforms are little-endian: the floats are written as is.
        samples.withUnsafeBytes { bytes in writeAll(Data(bytes)) }
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        audioFail("audio capture stopped: \(error)", code: 4)
    }
}

private func audio(_ arguments: [String]) -> Never {
    guard arguments.count == 4, ["--display", "--app"].contains(arguments[2]),
        let id = UInt32(arguments[3])
    else {
        audioFail("usage: audio --display <index> | --app <pid>", code: 64)
    }
    guard #available(macOS 13.0, *) else {
        audioFail("audio capture needs macOS 13 or later", code: 3)
    }
    var filter: SCContentFilter?
    while filter == nil {
        guard let content = audioContent() else {
            sleep(1)
            continue
        }
        let displays = orderedDisplays(content)
        if arguments[2] == "--display" {
            guard Int(id) < displays.count else {
                audioFail("screen \(id + 1) is not connected", code: 5)
            }
            filter = SCContentFilter(display: displays[Int(id)], excludingWindows: [])
        } else if let application = content.applications.first(where: {
            $0.processID == pid_t(bitPattern: id)
        }), let display = displays.first {
            filter = SCContentFilter(
                display: display, including: [application], exceptingWindows: [])
        } else {
            audioStatus("no_stream", "the application is not running")
            sleep(1)
        }
    }
    guard let filter else { exit(1) }

    let configuration = SCStreamConfiguration()
    configuration.capturesAudio = true
    configuration.excludesCurrentProcessAudio = true
    configuration.sampleRate = 48_000
    configuration.channelCount = 2
    // The pictures are not used: keep them tiny and rare.
    configuration.width = 2
    configuration.height = 2
    configuration.minimumFrameInterval = CMTime(value: 1, timescale: 1)
    configuration.queueDepth = 3

    let sound = Sound()
    let stream = SCStream(filter: filter, configuration: configuration, delegate: sound)
    do {
        try stream.addStreamOutput(
            sound, type: .audio,
            sampleHandlerQueue: DispatchQueue(label: "freemix.screen-capture.audio"))
    } catch {
        audioFail("audio capture could not start: \(error)", code: 2)
    }
    let started = DispatchSemaphore(value: 0)
    let start = StartBox()
    stream.startCapture { error in
        start.error = error
        started.signal()
    }
    started.wait()
    if let error = start.error {
        audioFail("audio capture could not start: \(error)", code: 2)
    }
    audioStatus("live")
    withExtendedLifetime((stream, sound)) {
        dispatchMain()
    }
}

signal(SIGPIPE, SIG_IGN)
let arguments = CommandLine.arguments
guard arguments.count >= 2 else {
    fail("usage: freemix-screen-capture-helper list | capture ... | audio ...", code: 64)
}
switch arguments[1] {
case "list":
    list()
case "capture":
    capture(arguments)
case "audio":
    audio(arguments)
default:
    fail("unknown command \(arguments[1])", code: 64)
}
